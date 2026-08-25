use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::DeserializeOwned};
use thiserror::Error;
use url::{ParseError, Url};

use crate::ids::{is_project_id, is_server_id, new_project_id, new_server_id};

pub const CONFIG_VERSION: u32 = 1;
pub const DEFAULT_API_PORT: u16 = 7737;
pub const DEFAULT_PROJECT_ICON: &str = "shippingbox.fill";
pub const DEFAULT_PROJECT_COLOR: &str = "#8E8E93";
pub const MIN_HEALTH_INTERVAL_SECONDS: u64 = 2;
pub const MAX_HEALTH_INTERVAL_SECONDS: u64 = 120;
pub const MIN_RESTART_ATTEMPTS: u32 = 1;
pub const MAX_RESTART_ATTEMPTS: u32 = 20;
pub const MIN_LOG_BUFFER_LINES: usize = 500;
pub const MAX_LOG_BUFFER_LINES: usize = 50_000;
pub const MIN_LOG_FILE_MAX_MB: u64 = 1;
pub const MAX_LOG_FILE_MAX_MB: u64 = 100;
pub const MIN_HTTP_STATUS: u16 = 100;
pub const MAX_HTTP_STATUS: u16 = 599;

use crate::memory::{MAXIMUM_MEMORY_LIMIT_BYTES, MINIMUM_MEMORY_LIMIT_BYTES};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum PreferredShell {
    #[default]
    Auto,
    Powershell7,
    WindowsPowershell,
    Cmd,
    Custom(String),
}

impl Serialize for PreferredShell {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let value = match self {
            Self::Auto => "auto",
            Self::Powershell7 => "powershell7",
            Self::WindowsPowershell => "windowsPowershell",
            Self::Cmd => "cmd",
            Self::Custom(path) => path,
        };
        serializer.serialize_str(value)
    }
}

impl<'de> Deserialize<'de> for PreferredShell {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(match value.as_str() {
            "auto" => Self::Auto,
            "powershell7" => Self::Powershell7,
            "windowsPowershell" => Self::WindowsPowershell,
            "cmd" => Self::Cmd,
            _ => Self::Custom(value),
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MemoryLimitMode {
    #[default]
    Inherit,
    Disabled,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerAction {
    pub name: String,
    pub command: String,
}

impl ServerAction {
    pub fn new(name: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            command: command.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    #[serde(default = "new_server_id", deserialize_with = "deserialize_server_id")]
    pub id: String,
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub directory: Option<String>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub env: HashMap<String, String>,
    #[serde(default, rename = "healthURL")]
    pub health_url: Option<String>,
    #[serde(default)]
    pub health_status: Option<u16>,
    #[serde(default = "default_true")]
    pub auto_restart: bool,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub actions: Vec<ServerAction>,
}

impl ServerConfig {
    pub fn new(name: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            id: new_server_id(),
            name: name.into(),
            command: command.into(),
            port: None,
            directory: None,
            env: HashMap::new(),
            health_url: None,
            health_status: None,
            auto_restart: true,
            actions: Vec::new(),
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    #[serde(
        default = "new_project_id",
        deserialize_with = "deserialize_project_id"
    )]
    pub id: String,
    pub name: String,
    #[serde(
        default = "default_project_icon",
        deserialize_with = "deserialize_project_icon"
    )]
    pub icon: String,
    #[serde(
        default = "default_project_color",
        deserialize_with = "deserialize_project_color"
    )]
    pub color: String,
    pub root: String,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub servers: Vec<ServerConfig>,
    #[serde(default)]
    pub memory_limit_mode: MemoryLimitMode,
    #[serde(default)]
    pub memory_limit_bytes: Option<u64>,
}

impl Project {
    pub fn new(name: impl Into<String>, root: impl Into<String>) -> Self {
        Self {
            id: new_project_id(),
            name: name.into(),
            icon: default_project_icon(),
            color: default_project_color(),
            root: root.into(),
            servers: Vec::new(),
            memory_limit_mode: MemoryLimitMode::Inherit,
            memory_limit_bytes: None,
        }
    }

    pub fn effective_memory_limit(&self, global_limit: Option<u64>) -> Option<u64> {
        match self.memory_limit_mode {
            MemoryLimitMode::Inherit => global_limit,
            MemoryLimitMode::Disabled => None,
            MemoryLimitMode::Custom => self.memory_limit_bytes,
        }
    }
}

fn default_project_icon() -> String {
    DEFAULT_PROJECT_ICON.to_owned()
}

fn default_project_color() -> String {
    DEFAULT_PROJECT_COLOR.to_owned()
}

fn deserialize_null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

fn deserialize_server_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?.unwrap_or_default();
    Ok(if value.is_empty() {
        new_server_id()
    } else {
        value
    })
}

fn deserialize_project_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?.unwrap_or_default();
    Ok(if value.is_empty() {
        new_project_id()
    } else {
        value
    })
}

fn deserialize_project_icon<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?.unwrap_or_default();
    Ok(if value.is_empty() {
        default_project_icon()
    } else {
        value
    })
}

fn deserialize_project_color<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?.unwrap_or_default();
    Ok(if value.is_empty() {
        default_project_color()
    } else {
        value
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetsuStackConfig {
    pub version: u32,
    pub api_port: u16,
    #[serde(default = "default_health_interval_seconds")]
    pub health_interval_seconds: u64,
    #[serde(default = "default_max_restart_attempts")]
    pub max_restart_attempts: u32,
    #[serde(default = "default_log_buffer_lines")]
    pub log_buffer_lines: usize,
    #[serde(default = "default_log_file_max_mb", rename = "logFileMaxMB")]
    pub log_file_max_mb: u64,
    #[serde(default)]
    pub global_memory_limit_bytes: Option<u64>,
    #[serde(default)]
    pub preferred_shell: PreferredShell,
    #[serde(deserialize_with = "deserialize_null_default")]
    pub projects: Vec<Project>,
}

fn default_health_interval_seconds() -> u64 {
    10
}

fn default_max_restart_attempts() -> u32 {
    5
}

fn default_log_buffer_lines() -> usize {
    5_000
}

fn default_log_file_max_mb() -> u64 {
    10
}

impl Default for NetsuStackConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            api_port: DEFAULT_API_PORT,
            health_interval_seconds: default_health_interval_seconds(),
            max_restart_attempts: default_max_restart_attempts(),
            log_buffer_lines: default_log_buffer_lines(),
            log_file_max_mb: default_log_file_max_mb(),
            global_memory_limit_bytes: None,
            preferred_shell: PreferredShell::Auto,
            projects: Vec::new(),
        }
    }
}

impl NetsuStackConfig {
    pub fn resolve_project(&self, query: &str) -> Option<&Project> {
        self.projects
            .iter()
            .find(|project| project.id == query)
            .or_else(|| {
                self.projects
                    .iter()
                    .find(|project| case_insensitive_eq(&project.name, query))
            })
    }

    pub fn resolve_server(&self, query: &str) -> Option<ResolvedServer<'_>> {
        for project in &self.projects {
            if let Some(server) = project.servers.iter().find(|server| server.id == query) {
                return Some(ResolvedServer { project, server });
            }
        }

        if let Some((project_query, server_query)) = query.split_once('/') {
            let project = self.resolve_project(project_query)?;
            let server = project
                .servers
                .iter()
                .find(|server| case_insensitive_eq(&server.name, server_query))?;
            return Some(ResolvedServer { project, server });
        }

        for project in &self.projects {
            if let Some(server) = project
                .servers
                .iter()
                .find(|server| case_insensitive_eq(&server.name, query))
            {
                return Some(ResolvedServer { project, server });
            }
        }
        None
    }

    pub fn validate(&self) -> Result<(), ConfigValidationError> {
        if self.version != CONFIG_VERSION {
            return Err(ConfigValidationError::UnsupportedConfigVersion {
                found: self.version,
                supported: CONFIG_VERSION,
            });
        }
        if self.api_port == 0 {
            return Err(ConfigValidationError::InvalidApiPort {
                port: self.api_port,
            });
        }
        if !(MIN_HEALTH_INTERVAL_SECONDS..=MAX_HEALTH_INTERVAL_SECONDS)
            .contains(&self.health_interval_seconds)
        {
            return Err(ConfigValidationError::InvalidHealthInterval {
                seconds: self.health_interval_seconds,
            });
        }
        if !(MIN_RESTART_ATTEMPTS..=MAX_RESTART_ATTEMPTS).contains(&self.max_restart_attempts) {
            return Err(ConfigValidationError::InvalidMaxRestartAttempts {
                attempts: self.max_restart_attempts,
            });
        }
        if !(MIN_LOG_BUFFER_LINES..=MAX_LOG_BUFFER_LINES).contains(&self.log_buffer_lines) {
            return Err(ConfigValidationError::InvalidLogBufferLines {
                lines: self.log_buffer_lines,
            });
        }
        if !(MIN_LOG_FILE_MAX_MB..=MAX_LOG_FILE_MAX_MB).contains(&self.log_file_max_mb) {
            return Err(ConfigValidationError::InvalidLogFileMaxMb {
                megabytes: self.log_file_max_mb,
            });
        }
        if let PreferredShell::Custom(shell) = &self.preferred_shell {
            if !is_valid_custom_shell_path(shell) {
                return Err(ConfigValidationError::InvalidPreferredShell {
                    value: shell.clone(),
                });
            }
        }
        if let Some(bytes) = self.global_memory_limit_bytes {
            validate_memory_limit(bytes, None)?;
        }

        let mut project_names = HashSet::new();
        let mut project_ids = HashSet::new();
        let mut server_ids = HashSet::new();
        let mut occupied_ports = HashMap::new();

        for project in &self.projects {
            if !is_project_id(&project.id) {
                return Err(ConfigValidationError::InvalidProjectId {
                    id: project.id.clone(),
                });
            }
            if project.name.trim().is_empty() {
                return Err(ConfigValidationError::EmptyProjectName {
                    project_id: project.id.clone(),
                });
            }
            if !is_addressable_target_name(&project.name) {
                return Err(ConfigValidationError::InvalidProjectName {
                    name: project.name.clone(),
                });
            }
            if project.root.trim().is_empty() {
                return Err(ConfigValidationError::EmptyProjectRoot {
                    project: project.name.clone(),
                });
            }
            if !is_absolute_windows_path(&project.root) {
                return Err(ConfigValidationError::RelativeProjectRoot {
                    project: project.name.clone(),
                    root: project.root.clone(),
                });
            }
            if !project_ids.insert(project.id.as_str()) {
                return Err(ConfigValidationError::DuplicateProjectId {
                    id: project.id.clone(),
                });
            }
            if !project_names.insert(project.name.to_lowercase()) {
                return Err(ConfigValidationError::DuplicateProjectName {
                    name: project.name.clone(),
                });
            }
            if project.memory_limit_mode == MemoryLimitMode::Custom {
                match project.memory_limit_bytes {
                    Some(bytes) => validate_memory_limit(bytes, Some(&project.name))?,
                    None => {
                        return Err(ConfigValidationError::MissingCustomMemoryLimit {
                            project: project.name.clone(),
                        });
                    }
                }
            }

            let mut server_names = HashSet::new();
            for server in &project.servers {
                if !is_server_id(&server.id) {
                    return Err(ConfigValidationError::InvalidServerId {
                        id: server.id.clone(),
                    });
                }
                if server.name.trim().is_empty() {
                    return Err(ConfigValidationError::EmptyServerName {
                        project: project.name.clone(),
                    });
                }
                if !is_addressable_target_name(&server.name) {
                    return Err(ConfigValidationError::InvalidServerName {
                        name: server.name.clone(),
                    });
                }
                if server.command.trim().is_empty() {
                    return Err(ConfigValidationError::EmptyServerCommand {
                        server: server.name.clone(),
                    });
                }
                if !server_ids.insert(server.id.as_str()) {
                    return Err(ConfigValidationError::DuplicateServerId {
                        id: server.id.clone(),
                    });
                }
                if !server_names.insert(server.name.to_lowercase()) {
                    return Err(ConfigValidationError::DuplicateServerName {
                        project: project.name.clone(),
                        name: server.name.clone(),
                    });
                }
                if let Some(port) = server.port {
                    if port == 0 {
                        return Err(ConfigValidationError::InvalidServerPort {
                            server: server.name.clone(),
                            port,
                        });
                    }
                    if port == self.api_port {
                        return Err(ConfigValidationError::ApiPortConflict {
                            server: server.name.clone(),
                            port,
                        });
                    }
                    if let Some(first_server) = occupied_ports.insert(port, server.name.as_str()) {
                        return Err(ConfigValidationError::DuplicateServerPort {
                            port,
                            first_server: first_server.to_owned(),
                            second_server: server.name.clone(),
                        });
                    }
                }
                if let Some(status) = server.health_status {
                    if !(MIN_HTTP_STATUS..=MAX_HTTP_STATUS).contains(&status) {
                        return Err(ConfigValidationError::InvalidHealthStatus {
                            server: server.name.clone(),
                            status,
                        });
                    }
                }
                if let Some(health_url) = &server.health_url {
                    if !is_valid_health_url(health_url, server.port) {
                        return Err(ConfigValidationError::InvalidHealthUrl {
                            server: server.name.clone(),
                            url: health_url.clone(),
                        });
                    }
                }

                let mut action_names = HashSet::new();
                for action in &server.actions {
                    if action.name.trim().is_empty() {
                        return Err(ConfigValidationError::EmptyActionName {
                            server: server.name.clone(),
                        });
                    }
                    if action.name != action.name.trim() {
                        return Err(ConfigValidationError::InvalidActionName {
                            name: action.name.clone(),
                        });
                    }
                    if action.command.trim().is_empty() {
                        return Err(ConfigValidationError::EmptyActionCommand {
                            server: server.name.clone(),
                            action: action.name.clone(),
                        });
                    }
                    if !action_names.insert(action.name.to_lowercase()) {
                        return Err(ConfigValidationError::DuplicateActionName {
                            server: server.name.clone(),
                            name: action.name.clone(),
                        });
                    }
                }
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedServer<'a> {
    pub project: &'a Project,
    pub server: &'a ServerConfig,
}

fn case_insensitive_eq(left: &str, right: &str) -> bool {
    left.to_lowercase() == right.to_lowercase()
}

fn is_addressable_target_name(value: &str) -> bool {
    value == value.trim() && !value.contains('/')
}

fn is_absolute_windows_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    let is_separator = |byte| matches!(byte, b'\\' | b'/');

    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && is_separator(bytes[2])
    {
        return true;
    }

    if bytes.len() >= 5 && is_separator(bytes[0]) && is_separator(bytes[1]) {
        let mut components = value[2..]
            .split(['\\', '/'])
            .filter(|component| !component.is_empty());
        return components.next().is_some() && components.next().is_some();
    }

    false
}

fn is_valid_custom_shell_path(value: &str) -> bool {
    if value != value.trim() || !is_absolute_windows_path(value) {
        return false;
    }

    let bytes = value.as_bytes();
    let is_separator = |byte| matches!(byte, b'\\' | b'/');
    if bytes.last().is_some_and(|byte| is_separator(*byte)) {
        return false;
    }

    let components: Vec<_> = value
        .split(['\\', '/'])
        .filter(|component| !component.is_empty())
        .collect();
    let minimum_components = if bytes.len() >= 2 && is_separator(bytes[0]) && is_separator(bytes[1])
    {
        3
    } else {
        2
    };
    components.len() >= minimum_components
        && components
            .last()
            .is_some_and(|component| !matches!(*component, "." | ".."))
}

fn is_valid_health_url(value: &str, port: Option<u16>) -> bool {
    if value.is_empty() || value != value.trim() {
        return false;
    }

    match Url::parse(value) {
        Ok(url) => matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
        Err(ParseError::RelativeUrlWithoutBase) => {
            let Some(port) = port else {
                return false;
            };
            if value.starts_with("//") {
                return false;
            }
            let Ok(base) = Url::parse(&format!("http://localhost:{port}/")) else {
                return false;
            };
            base.join(value).is_ok_and(|resolved| {
                resolved.scheme() == "http"
                    && resolved.host_str() == Some("localhost")
                    && resolved.port_or_known_default() == Some(port)
            })
        }
        Err(_) => false,
    }
}

fn validate_memory_limit(bytes: u64, project: Option<&str>) -> Result<(), ConfigValidationError> {
    if (MINIMUM_MEMORY_LIMIT_BYTES..=MAXIMUM_MEMORY_LIMIT_BYTES).contains(&bytes) {
        Ok(())
    } else {
        Err(ConfigValidationError::InvalidMemoryLimit {
            project: project.map(str::to_owned),
            bytes,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ConfigValidationError {
    #[error("config version {found} is unsupported; this build supports version {supported}")]
    UnsupportedConfigVersion { found: u32, supported: u32 },
    #[error("API port {port} is outside 1-65535")]
    InvalidApiPort { port: u16 },
    #[error("health interval {seconds} is outside 2-120 seconds")]
    InvalidHealthInterval { seconds: u64 },
    #[error("maximum restart attempts {attempts} is outside 1-20")]
    InvalidMaxRestartAttempts { attempts: u32 },
    #[error("log buffer line count {lines} is outside 500-50000")]
    InvalidLogBufferLines { lines: usize },
    #[error("log file limit {megabytes} MB is outside 1-100 MB")]
    InvalidLogFileMaxMb { megabytes: u64 },
    #[error("preferred custom shell {value} must be a trimmed absolute Windows path")]
    InvalidPreferredShell { value: String },
    #[error("project {project_id} has an empty name")]
    EmptyProjectName { project_id: String },
    #[error("project name {name} must be trimmed and cannot contain '/'")]
    InvalidProjectName { name: String },
    #[error("project {project} has an empty root")]
    EmptyProjectRoot { project: String },
    #[error("project {project} root {root} is not an absolute Windows path")]
    RelativeProjectRoot { project: String, root: String },
    #[error("project id {id} is duplicated")]
    DuplicateProjectId { id: String },
    #[error("project id {id} must use the prj_ prefix and eight lowercase hexadecimal digits")]
    InvalidProjectId { id: String },
    #[error("project name {name} conflicts case-insensitively")]
    DuplicateProjectName { name: String },
    #[error("project {project} uses custom memory limits without a size")]
    MissingCustomMemoryLimit { project: String },
    #[error("memory limit {bytes} is outside 128 MiB-1 TiB")]
    InvalidMemoryLimit { project: Option<String>, bytes: u64 },
    #[error("project {project} has a server with an empty name")]
    EmptyServerName { project: String },
    #[error("server name {name} must be trimmed and cannot contain '/'")]
    InvalidServerName { name: String },
    #[error("server {server} has an empty command")]
    EmptyServerCommand { server: String },
    #[error("server id {id} is duplicated")]
    DuplicateServerId { id: String },
    #[error("server id {id} must use the srv_ prefix and eight lowercase hexadecimal digits")]
    InvalidServerId { id: String },
    #[error("server name {name} conflicts case-insensitively in project {project}")]
    DuplicateServerName { project: String, name: String },
    #[error("server {server} has invalid port {port}")]
    InvalidServerPort { server: String, port: u16 },
    #[error("server {server} has invalid HTTP health status {status}")]
    InvalidHealthStatus { server: String, status: u16 },
    #[error("server {server} has invalid health URL {url}")]
    InvalidHealthUrl { server: String, url: String },
    #[error("server {server} reuses API port {port}")]
    ApiPortConflict { server: String, port: u16 },
    #[error("servers {first_server} and {second_server} both use port {port}")]
    DuplicateServerPort {
        port: u16,
        first_server: String,
        second_server: String,
    },
    #[error("server {server} has an action with an empty name")]
    EmptyActionName { server: String },
    #[error("action name {name} must be trimmed")]
    InvalidActionName { name: String },
    #[error("action {action} on server {server} has an empty command")]
    EmptyActionCommand { server: String, action: String },
    #[error("action name {name} conflicts case-insensitively on server {server}")]
    DuplicateActionName { server: String, name: String },
}
