use std::{
    fmt, fs,
    io::Write,
    path::{Path, PathBuf},
};

#[cfg(not(windows))]
use std::fs::OpenOptions;

use thiserror::Error;

use crate::ConfigPaths;

const TOKEN_BYTES: usize = 32;
const TOKEN_STAGING_ATTEMPTS: usize = 16;

#[derive(Clone, PartialEq, Eq)]
pub struct ApiToken(String);

impl ApiToken {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ApiToken([REDACTED])")
    }
}

#[derive(Debug, Error)]
pub enum TokenError {
    #[error("token I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("secure token generation failed: {0}")]
    Random(#[from] getrandom::Error),
    #[error("stored API token is not exactly 256 bits of hexadecimal data")]
    InvalidStoredToken,
    #[cfg(windows)]
    #[error("Windows token ACL operation failed: {0}")]
    Windows(#[from] windows::core::Error),
}

pub fn load_or_create_token(paths: &ConfigPaths) -> Result<ApiToken, TokenError> {
    fs::create_dir_all(paths.root())?;
    let path = paths.token_file();
    if path.exists() {
        let token = parse_token(&fs::read_to_string(&path)?)?;
        restrict_token_file(&path)?;
        return Ok(token);
    }

    let mut random = [0_u8; TOKEN_BYTES];
    getrandom::fill(&mut random)?;
    let token = ApiToken(encode_hex(&random));
    let mut persisted = token.0.as_bytes().to_vec();
    persisted.push(b'\n');
    secure_atomic_write_with(&path, &persisted, |_| Ok(()))?;
    Ok(token)
}

fn secure_atomic_write_with<F>(path: &Path, bytes: &[u8], after_create: F) -> Result<(), TokenError>
where
    F: FnOnce(&Path) -> Result<(), TokenError>,
{
    let (staging, mut file) = create_secure_staging(path)?;
    let result = (|| -> Result<(), TokenError> {
        after_create(&staging)?;
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
        validate_staging_handle(&file)?;
        commit_staging_file(file, &staging, path)?;
        Ok(())
    })();
    if let Err(error) = result {
        // The handle is closed before cleanup. No secret bytes can remain at a broad ACL:
        // creation either applied the protected DACL atomically or produced no file.
        let _ = fs::remove_file(&staging);
        return Err(error);
    }
    Ok(())
}

fn create_secure_staging(path: &Path) -> Result<(PathBuf, fs::File), TokenError> {
    create_secure_staging_with(path, token_staging_path)
}

fn create_secure_staging_with<F>(
    path: &Path,
    mut next_staging: F,
) -> Result<(PathBuf, fs::File), TokenError>
where
    F: FnMut(&Path) -> Result<PathBuf, TokenError>,
{
    for _ in 0..TOKEN_STAGING_ATTEMPTS {
        let staging = next_staging(path)?;
        match create_secure_staging_file(&staging) {
            Ok(file) => return Ok((staging, file)),
            Err(TokenError::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(TokenError::Io(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a unique secure token staging file",
    )))
}

fn token_staging_path(path: &Path) -> Result<PathBuf, TokenError> {
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random)?;
    Ok(path.with_file_name(format!(".api-token.{}.tmp", encode_hex(&random))))
}

#[cfg(windows)]
fn create_secure_staging_file(path: &Path) -> Result<fs::File, TokenError> {
    windows_acl::create_secure_file(path)
}

#[cfg(not(windows))]
fn create_secure_staging_file(path: &Path) -> Result<fs::File, TokenError> {
    let file = OpenOptions::new().write(true).create_new(true).open(path)?;
    restrict_token_file(path)?;
    Ok(file)
}

#[cfg(windows)]
fn validate_staging_handle(file: &fs::File) -> Result<(), TokenError> {
    windows_acl::validate_regular_file_handle(file)
}

#[cfg(not(windows))]
fn validate_staging_handle(_file: &fs::File) -> Result<(), TokenError> {
    Ok(())
}

#[cfg(windows)]
fn commit_staging_file(file: fs::File, _staging: &Path, target: &Path) -> Result<(), TokenError> {
    windows_acl::rename_file_handle(&file, target)?;
    drop(file);
    Ok(())
}

#[cfg(not(windows))]
fn commit_staging_file(file: fs::File, staging: &Path, target: &Path) -> Result<(), TokenError> {
    drop(file);
    fs::rename(staging, target)?;
    Ok(())
}

fn parse_token(contents: &str) -> Result<ApiToken, TokenError> {
    let value = contents.strip_suffix('\n').unwrap_or(contents);
    if value.len() != TOKEN_BYTES * 2 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(TokenError::InvalidStoredToken);
    }
    Ok(ApiToken(value.to_owned()))
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

#[cfg(windows)]
fn restrict_token_file(path: &Path) -> Result<(), TokenError> {
    windows_acl::restrict(path)
}

#[cfg(not(windows))]
fn restrict_token_file(path: &Path) -> Result<(), TokenError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

pub fn token_file_is_restricted_to_current_user(path: &Path) -> Result<bool, TokenError> {
    #[cfg(windows)]
    {
        windows_acl::is_restricted(path)
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;

        Ok(fs::metadata(path)?.permissions().mode() & 0o077 == 0)
    }
}

#[cfg(windows)]
mod windows_acl {
    use std::{
        ffi::c_void,
        fs::File,
        iter,
        os::windows::{ffi::OsStrExt, io::AsRawHandle, io::FromRawHandle},
        path::Path,
        ptr,
    };

    use windows::{
        Win32::{
            Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree, WIN32_ERROR},
            Security::{
                ACCESS_ALLOWED_ACE, ACE_FLAGS, ACL,
                Authorization::{
                    EXPLICIT_ACCESS_W, GetNamedSecurityInfoW, SE_FILE_OBJECT, SET_ACCESS,
                    SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_USER,
                    TRUSTEE_W,
                },
                DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetSecurityDescriptorControl,
                GetTokenInformation, InitializeSecurityDescriptor, OWNER_SECURITY_INFORMATION,
                PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED,
                SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR, SetSecurityDescriptorControl,
                SetSecurityDescriptorDacl, TOKEN_QUERY, TOKEN_USER, TokenUser,
            },
            Storage::FileSystem::{
                BY_HANDLE_FILE_INFORMATION, CREATE_NEW, CreateFileW, DELETE, FILE_ALL_ACCESS,
                FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
                FILE_GENERIC_WRITE, FILE_RENAME_INFO, FILE_SHARE_NONE, FileRenameInfo,
                GetFileInformationByHandle, SetFileInformationByHandle,
            },
            System::Threading::{GetCurrentProcess, OpenProcessToken},
        },
        core::{HRESULT, PWSTR},
    };

    use super::TokenError;

    struct OwnedHandle(HANDLE);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            // SAFETY: The handle was returned by OpenProcessToken and is owned here.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    struct LocalAllocation(*mut c_void);

    impl Drop for LocalAllocation {
        fn drop(&mut self) {
            // SAFETY: The pointer was allocated by a Windows LocalAlloc-family API.
            unsafe {
                LocalFree(Some(HLOCAL(self.0)));
            }
        }
    }

    pub(super) fn restrict(path: &Path) -> Result<(), TokenError> {
        let sid_storage = current_user_sid()?;
        let sid = token_user_sid(&sid_storage);
        let entry = current_user_entry(sid);
        let mut acl = ptr::null_mut::<ACL>();
        // SAFETY: The explicit entry and SID storage remain valid until the ACL is created.
        win32_result(unsafe { SetEntriesInAclW(Some(&[entry]), None, &mut acl) })?;
        let allocation = LocalAllocation(acl.cast());
        let path_wide = wide_path(path);
        // SAFETY: The path is nul-terminated and the ACL remains allocated for this call.
        let result = unsafe {
            SetNamedSecurityInfoW(
                windows::core::PCWSTR(path_wide.as_ptr()),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(acl),
                None,
            )
        };
        win32_result(result)?;
        drop(allocation);
        Ok(())
    }

    pub(super) fn create_secure_file(path: &Path) -> Result<File, TokenError> {
        let sid_storage = current_user_sid()?;
        let sid = token_user_sid(&sid_storage);
        let entry = current_user_entry(sid);
        let mut acl = ptr::null_mut::<ACL>();
        // SAFETY: The explicit entry and SID storage remain valid while the ACL is created.
        win32_result(unsafe { SetEntriesInAclW(Some(&[entry]), None, &mut acl) })?;
        let _acl_allocation = LocalAllocation(acl.cast());

        let mut descriptor = SECURITY_DESCRIPTOR::default();
        let descriptor_pointer = PSECURITY_DESCRIPTOR(ptr::addr_of_mut!(descriptor).cast());
        // SAFETY: descriptor and ACL remain live through CreateFileW.
        unsafe {
            InitializeSecurityDescriptor(descriptor_pointer, 1)?;
            SetSecurityDescriptorDacl(descriptor_pointer, true, Some(acl), false)?;
            SetSecurityDescriptorControl(descriptor_pointer, SE_DACL_PROTECTED, SE_DACL_PROTECTED)?;
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor_pointer.0,
            bInheritHandle: false.into(),
        };
        let path_wide = wide_path(path);
        // SAFETY: All pointers remain live for this call. CREATE_NEW prevents opening an
        // attacker-controlled path and FILE_SHARE_NONE prevents path replacement while open.
        let handle = unsafe {
            CreateFileW(
                windows::core::PCWSTR(path_wide.as_ptr()),
                FILE_GENERIC_WRITE.0 | DELETE.0,
                FILE_SHARE_NONE,
                Some(&attributes),
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                None,
            )
        }
        .map_err(|error| {
            let io_error = std::io::Error::last_os_error();
            if io_error.raw_os_error().is_some() {
                TokenError::Io(io_error)
            } else {
                TokenError::Windows(error)
            }
        })?;
        // SAFETY: CreateFileW returned an owned file handle. File closes it exactly once.
        Ok(unsafe { File::from_raw_handle(handle.0) })
    }

    pub(super) fn validate_regular_file_handle(file: &File) -> Result<(), TokenError> {
        let handle = HANDLE(file.as_raw_handle());
        let mut information = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: handle is owned by the live File and information is writable.
        unsafe { GetFileInformationByHandle(handle, &mut information)? };
        if information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
            || information.nNumberOfLinks != 1
        {
            return Err(TokenError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "token staging file is a reparse point or has multiple links",
            )));
        }
        Ok(())
    }

    pub(super) fn rename_file_handle(file: &File, target: &Path) -> Result<(), TokenError> {
        let target_wide: Vec<u16> = target.as_os_str().encode_wide().collect();
        let name_bytes = target_wide
            .len()
            .checked_mul(std::mem::size_of::<u16>())
            .and_then(|length| u32::try_from(length).ok())
            .ok_or_else(|| {
                TokenError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "token target path is too long",
                ))
            })?;
        let header_bytes = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
        let total_bytes = header_bytes
            .checked_add(name_bytes as usize)
            .ok_or_else(|| {
                TokenError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "token rename buffer is too large",
                ))
            })?;
        let word = std::mem::size_of::<usize>();
        let mut buffer = vec![0_usize; total_bytes.div_ceil(word)];
        let information = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
        // SAFETY: buffer is aligned and large enough for the fixed header and full UTF-16 name.
        unsafe {
            (*information).Anonymous.ReplaceIfExists = false;
            (*information).RootDirectory = HANDLE::default();
            (*information).FileNameLength = name_bytes;
            ptr::copy_nonoverlapping(
                target_wide.as_ptr(),
                ptr::addr_of_mut!((*information).FileName).cast::<u16>(),
                target_wide.len(),
            );
            SetFileInformationByHandle(
                HANDLE(file.as_raw_handle()),
                FileRenameInfo,
                information.cast(),
                u32::try_from(total_bytes).map_err(|_| {
                    TokenError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "token rename buffer exceeds Win32 limits",
                    ))
                })?,
            )?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn make_directory_permissive(path: &Path) -> Result<(), TokenError> {
        use windows::Win32::Security::{
            CONTAINER_INHERIT_ACE, CreateWellKnownSid, OBJECT_INHERIT_ACE, WinWorldSid,
        };

        let mut needed = 0_u32;
        // SAFETY: This is the documented size query for a well-known SID.
        let _ = unsafe { CreateWellKnownSid(WinWorldSid, None, None, &mut needed) };
        if needed == 0 {
            return Err(windows::core::Error::from_win32().into());
        }
        let word = std::mem::size_of::<usize>();
        let mut sid_storage = vec![0_usize; (needed as usize).div_ceil(word)];
        let everyone = PSID(sid_storage.as_mut_ptr().cast());
        // SAFETY: sid_storage is aligned and has at least needed bytes.
        unsafe { CreateWellKnownSid(WinWorldSid, None, Some(everyone), &mut needed)? };
        let mut entry = current_user_entry(everyone);
        entry.grfInheritance = CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE;
        let mut acl = ptr::null_mut::<ACL>();
        // SAFETY: The explicit entry and SID remain live while the ACL is created.
        win32_result(unsafe { SetEntriesInAclW(Some(&[entry]), None, &mut acl) })?;
        let _acl_allocation = LocalAllocation(acl.cast());
        let path_wide = wide_path(path);
        // SAFETY: path and ACL remain live for the duration of this call.
        win32_result(unsafe {
            SetNamedSecurityInfoW(
                windows::core::PCWSTR(path_wide.as_ptr()),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(acl),
                None,
            )
        })
    }

    pub(super) fn is_restricted(path: &Path) -> Result<bool, TokenError> {
        let path_wide = wide_path(path);
        let mut owner = PSID::default();
        let mut dacl = ptr::null_mut::<ACL>();
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: Output pointers are valid and the returned descriptor is released below.
        win32_result(unsafe {
            GetNamedSecurityInfoW(
                windows::core::PCWSTR(path_wide.as_ptr()),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                Some(&mut owner),
                None,
                Some(&mut dacl),
                None,
                &mut descriptor,
            )
        })?;
        let descriptor_allocation = LocalAllocation(descriptor.0);
        if dacl.is_null() {
            return Ok(false);
        }

        let mut control = 0_u16;
        let mut revision = 0_u32;
        // SAFETY: descriptor points to the valid security descriptor returned above.
        unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision)? };
        // SAFETY: dacl points inside the live descriptor and its header is readable.
        if unsafe { (*dacl).AceCount } != 1 || control & SE_DACL_PROTECTED.0 == 0 {
            return Ok(false);
        }

        let mut ace_pointer = ptr::null_mut::<c_void>();
        // SAFETY: The DACL contains exactly one ACE, so index zero is valid.
        unsafe { GetAce(dacl, 0, &mut ace_pointer)? };
        // SAFETY: GetAce returned a valid ACCESS_ALLOWED_ACE pointer for the live DACL.
        let ace = unsafe { &*ace_pointer.cast::<ACCESS_ALLOWED_ACE>() };
        if ace.Header.AceType != 0 || ace.Mask != FILE_ALL_ACCESS.0 {
            return Ok(false);
        }
        let ace_sid = PSID(ptr::addr_of!(ace.SidStart).cast_mut().cast());
        let sid_storage = current_user_sid()?;
        let current_sid = token_user_sid(&sid_storage);
        // SAFETY: Both SID pointers refer to live, valid security identifier buffers.
        let same_ace_user = unsafe { EqualSid(ace_sid, current_sid).is_ok() };
        // SAFETY: The owner SID is part of the live descriptor and current_sid is live storage.
        let same_owner = unsafe { EqualSid(owner, current_sid).is_ok() };
        drop(descriptor_allocation);
        Ok(same_ace_user && same_owner)
    }

    fn current_user_sid() -> Result<Vec<usize>, TokenError> {
        let mut raw_handle = HANDLE::default();
        // SAFETY: raw_handle is a valid output pointer; the returned handle is closed by OwnedHandle.
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw_handle)? };
        let handle = OwnedHandle(raw_handle);
        let mut needed = 0_u32;
        // SAFETY: A zero-length query with no output buffer is the documented size probe.
        let _ = unsafe { GetTokenInformation(handle.0, TokenUser, None, 0, &mut needed) };
        if needed == 0 {
            return Err(windows::core::Error::from_win32().into());
        }
        let word = std::mem::size_of::<usize>();
        let mut storage = vec![0_usize; (needed as usize).div_ceil(word)];
        // SAFETY: storage is aligned and has at least `needed` writable bytes.
        unsafe {
            GetTokenInformation(
                handle.0,
                TokenUser,
                Some(storage.as_mut_ptr().cast()),
                needed,
                &mut needed,
            )?;
        }
        Ok(storage)
    }

    fn token_user_sid(storage: &[usize]) -> PSID {
        // SAFETY: current_user_sid fills storage with a TOKEN_USER structure.
        unsafe { (*(storage.as_ptr().cast::<TOKEN_USER>())).User.Sid }
    }

    fn current_user_entry(sid: PSID) -> EXPLICIT_ACCESS_W {
        EXPLICIT_ACCESS_W {
            grfAccessPermissions: FILE_ALL_ACCESS.0,
            grfAccessMode: SET_ACCESS,
            grfInheritance: ACE_FLAGS(0),
            Trustee: TRUSTEE_W {
                pMultipleTrustee: ptr::null_mut(),
                MultipleTrusteeOperation: Default::default(),
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_USER,
                ptstrName: PWSTR(sid.0.cast()),
            },
        }
    }

    fn wide_path(path: &Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(iter::once(0))
            .collect()
    }

    fn win32_result(error: WIN32_ERROR) -> Result<(), TokenError> {
        if error.0 == 0 {
            Ok(())
        } else {
            Err(windows::core::Error::from_hresult(HRESULT::from_win32(error.0)).into())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, collections::VecDeque, fs, io, path::Path};

    use tempfile::TempDir;

    use super::{TokenError, create_secure_staging_with, secure_atomic_write_with};

    #[cfg(windows)]
    #[test]
    fn staging_acl_is_restricted_before_secret_bytes_are_written() {
        let temp = TempDir::new().expect("temporary token directory");
        super::windows_acl::make_directory_permissive(temp.path())
            .expect("parent directory becomes permissive");
        assert!(!super::windows_acl::is_restricted(temp.path()).expect("parent ACL readable"));
        let path = temp.path().join("api-token");
        let diverted = temp.path().join("diverted-token");
        let protection_observed = Cell::new(false);

        secure_atomic_write_with(&path, b"secret", |staging| {
            assert_eq!(fs::metadata(staging)?.len(), 0);
            assert!(super::windows_acl::is_restricted(staging)?);
            assert!(
                fs::rename(staging, &diverted).is_err(),
                "the retained no-share handle must prevent path swaps"
            );
            protection_observed.set(true);
            Ok(())
        })
        .expect("secure write succeeds");

        assert!(protection_observed.get());
        assert!(!diverted.exists());
        assert_eq!(fs::read(&path).expect("token readable"), b"secret");
        assert!(super::windows_acl::is_restricted(&path).expect("final ACL readable"));
    }

    #[test]
    fn protection_failure_removes_empty_staging_without_writing_secret_bytes() {
        let temp = TempDir::new().expect("temporary token directory");
        let path = temp.path().join("api-token");

        let result = secure_atomic_write_with(&path, b"must-not-be-written", |staging| {
            assert_eq!(fs::metadata(staging)?.len(), 0);
            Err(TokenError::Io(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "injected ACL failure",
            )))
        });

        assert!(result.is_err());
        assert!(!path.exists());
        assert!(directory_is_empty(temp.path()));
    }

    #[test]
    fn staging_name_collisions_are_retried_without_touching_existing_files() {
        let temp = TempDir::new().expect("temporary token directory");
        let path = temp.path().join("api-token");
        let collision = temp.path().join(".api-token.collision.tmp");
        let available = temp.path().join(".api-token.available.tmp");
        fs::write(&collision, b"existing").expect("collision fixture written");
        let mut candidates = VecDeque::from([collision.clone(), available.clone()]);

        let (staging, file) = create_secure_staging_with(&path, |_| {
            Ok(candidates.pop_front().expect("candidate is available"))
        })
        .expect("collision is retried");

        assert_eq!(staging, available);
        assert_eq!(
            fs::read(&collision).expect("collision remains"),
            b"existing"
        );
        drop(file);
        fs::remove_file(staging).expect("secure staging cleanup");
    }

    fn directory_is_empty(path: &Path) -> bool {
        fs::read_dir(path)
            .expect("directory is readable")
            .next()
            .is_none()
    }
}
