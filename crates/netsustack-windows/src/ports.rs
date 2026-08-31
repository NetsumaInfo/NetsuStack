use std::{
    collections::{BTreeMap, BTreeSet},
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
};

use crate::WindowsError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressFamily {
    Ipv4,
    Ipv6,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TcpListenerEntry {
    pub local_addresses: Vec<IpAddr>,
    pub port: u16,
    pub pid: u32,
}

impl TcpListenerEntry {
    pub fn new(local_address: IpAddr, port: u16, pid: u32) -> Self {
        Self {
            local_addresses: vec![local_address],
            port,
            pid,
        }
    }

    fn from_addresses(
        local_addresses: impl IntoIterator<Item = IpAddr>,
        port: u16,
        pid: u32,
    ) -> Self {
        Self {
            local_addresses: local_addresses.into_iter().collect(),
            port,
            pid,
        }
    }
}

pub fn deduplicate_tcp_listeners(
    listeners: impl IntoIterator<Item = TcpListenerEntry>,
) -> Vec<TcpListenerEntry> {
    let mut unique: BTreeMap<(u16, u32), BTreeSet<IpAddr>> = BTreeMap::new();
    for listener in listeners {
        unique
            .entry((listener.port, listener.pid))
            .or_default()
            .extend(listener.local_addresses);
    }
    unique
        .into_iter()
        .map(|((port, pid), addresses)| TcpListenerEntry::from_addresses(addresses, port, pid))
        .collect()
}

#[cfg(windows)]
pub fn list_tcp_listeners() -> Result<Vec<TcpListenerEntry>, WindowsError> {
    list_tcp_listeners_with(read_table)
}

fn list_tcp_listeners_with(
    mut read: impl FnMut(AddressFamily) -> Result<Vec<TcpListenerEntry>, WindowsError>,
) -> Result<Vec<TcpListenerEntry>, WindowsError> {
    let mut listeners = read(AddressFamily::Ipv4)?;
    match read(AddressFamily::Ipv6) {
        Ok(ipv6) => listeners.extend(ipv6),
        Err(WindowsError::Api { code, .. }) if code == error_not_supported_code() => {}
        Err(error) => return Err(error),
    }
    Ok(deduplicate_tcp_listeners(listeners))
}

fn error_not_supported_code() -> i32 {
    #[cfg(windows)]
    {
        windows::Win32::Foundation::ERROR_NOT_SUPPORTED.0 as i32
    }
    #[cfg(not(windows))]
    {
        50
    }
}

#[cfg(windows)]
fn read_table(family: AddressFamily) -> Result<Vec<TcpListenerEntry>, WindowsError> {
    use std::{ffi::c_void, mem::size_of, ptr};

    use windows::Win32::{
        Foundation::{ERROR_INSUFFICIENT_BUFFER, NO_ERROR},
        NetworkManagement::IpHelper::{
            GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCPROW_OWNER_PID,
            TCP_TABLE_OWNER_PID_LISTENER,
        },
        Networking::WinSock::{AF_INET, AF_INET6},
    };

    let address_family = match family {
        AddressFamily::Ipv4 => u32::from(AF_INET.0),
        AddressFamily::Ipv6 => u32::from(AF_INET6.0),
    };
    let mut byte_size = 0_u32;
    // SAFETY: A null output buffer is the documented sizing call. `byte_size`
    // is a valid writable pointer and the remaining values are documented.
    let sizing = unsafe {
        GetExtendedTcpTable(
            None,
            &mut byte_size,
            false,
            address_family,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        )
    };
    if sizing != ERROR_INSUFFICIENT_BUFFER.0 && sizing != NO_ERROR.0 {
        return Err(WindowsError::api_code("GetExtendedTcpTable(size)", sizing));
    }

    loop {
        let word_count = (byte_size as usize).div_ceil(size_of::<u32>());
        let mut buffer = vec![0_u32; word_count];
        let mut provided_size = byte_size;
        // SAFETY: `buffer` is writable for `provided_size` bytes and remains
        // allocated for the duration of the API call.
        let status = unsafe {
            GetExtendedTcpTable(
                Some(buffer.as_mut_ptr().cast::<c_void>()),
                &mut provided_size,
                false,
                address_family,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        };
        if status == ERROR_INSUFFICIENT_BUFFER.0 {
            byte_size = provided_size;
            continue;
        }
        if status != NO_ERROR.0 {
            return Err(WindowsError::api_code("GetExtendedTcpTable", status));
        }

        let base = buffer.as_ptr().cast::<u8>();
        // SAFETY: A successful table call always starts with `dwNumEntries`.
        let count = unsafe { ptr::read_unaligned(base.cast::<u32>()) } as usize;
        let row_base = size_of::<u32>();
        let row_size = match family {
            AddressFamily::Ipv4 => size_of::<MIB_TCPROW_OWNER_PID>(),
            AddressFamily::Ipv6 => size_of::<MIB_TCP6ROW_OWNER_PID>(),
        };
        let required = row_base.saturating_add(count.saturating_mul(row_size));
        if required > provided_size as usize {
            return Err(WindowsError::api_code(
                "GetExtendedTcpTable(invalid table)",
                13,
            ));
        }

        let mut rows = Vec::with_capacity(count);
        for index in 0..count {
            let row = unsafe { base.add(row_base + index * row_size) };
            let (local_address, raw_port, pid) = match family {
                AddressFamily::Ipv4 => {
                    // SAFETY: The bounds check above covers this complete row;
                    // unaligned reads are used because the API has a byte ABI.
                    let row = unsafe { ptr::read_unaligned(row.cast::<MIB_TCPROW_OWNER_PID>()) };
                    (
                        IpAddr::V4(Ipv4Addr::from(row.dwLocalAddr.to_ne_bytes())),
                        row.dwLocalPort,
                        row.dwOwningPid,
                    )
                }
                AddressFamily::Ipv6 => {
                    // SAFETY: Same invariant as the IPv4 branch.
                    let row = unsafe { ptr::read_unaligned(row.cast::<MIB_TCP6ROW_OWNER_PID>()) };
                    (
                        IpAddr::V6(Ipv6Addr::from(row.ucLocalAddr)),
                        row.dwLocalPort,
                        row.dwOwningPid,
                    )
                }
            };
            rows.push(TcpListenerEntry::new(
                local_address,
                u16::from_be(raw_port as u16),
                pid,
            ));
        }
        return Ok(rows);
    }
}

#[cfg(not(windows))]
pub fn list_tcp_listeners() -> Result<Vec<TcpListenerEntry>, WindowsError> {
    Err(WindowsError::InvalidInput {
        field: "platform",
        reason: "Windows is required",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_ipv6_table_keeps_the_ipv4_listeners() {
        let ipv4 = TcpListenerEntry::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 5173, 42);

        let listeners = list_tcp_listeners_with(|family| match family {
            AddressFamily::Ipv4 => Ok(vec![ipv4.clone()]),
            AddressFamily::Ipv6 => Err(api_error(50)),
        })
        .expect("an unavailable IPv6 family is optional");

        assert_eq!(listeners, vec![ipv4]);
    }

    #[test]
    fn an_ipv6_table_error_other_than_unsupported_is_preserved() {
        let error = list_tcp_listeners_with(|family| match family {
            AddressFamily::Ipv4 => Ok(Vec::new()),
            AddressFamily::Ipv6 => Err(api_error(5)),
        })
        .expect_err("access errors must not be hidden as an empty IPv6 table");

        assert!(matches!(
            error,
            WindowsError::Api {
                operation: "GetExtendedTcpTable",
                code: 5,
                ..
            }
        ));
    }

    fn api_error(code: i32) -> WindowsError {
        WindowsError::Api {
            operation: "GetExtendedTcpTable",
            code,
            message: format!("fixture error {code}"),
        }
    }
}
