use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::MemoryLimitMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ServerState {
    Stopped,
    Starting,
    Running,
    Unhealthy,
    Restarting,
    Failed,
}

impl ServerState {
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Starting | Self::Running | Self::Unhealthy | Self::Restarting
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TemporaryJobState {
    Running,
    Succeeded,
    Failed,
    TimedOut,
    Stopped,
}

impl TemporaryJobState {
    pub fn is_finished(self) -> bool {
        self != Self::Running
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TemporaryJobStatus {
    pub id: String,
    pub name: String,
    pub command: String,
    pub directory: String,
    pub state: TemporaryJobState,
    pub pid: Option<u32>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub timeout_seconds: u64,
    pub deadline: Option<DateTime<Utc>>,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
}

impl TemporaryJobStatus {
    pub fn process_exit_code(&self) -> i32 {
        match self.state {
            TemporaryJobState::Succeeded | TemporaryJobState::Running => 0,
            TemporaryJobState::TimedOut => 124,
            TemporaryJobState::Stopped => 130,
            TemporaryJobState::Failed => self.exit_code.filter(|code| *code != 0).unwrap_or(1),
        }
    }

    pub fn elapsed_seconds_at(&self, now: DateTime<Utc>) -> Option<f64> {
        let started_at = self.started_at?;
        let finished_at = self.finished_at.unwrap_or(now);
        let milliseconds = (finished_at - started_at).num_milliseconds();
        Some(milliseconds as f64 / 1_000.0)
    }

    pub fn elapsed_seconds(&self) -> Option<f64> {
        self.elapsed_seconds_at(Utc::now())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStatus {
    pub id: String,
    pub name: String,
    #[serde(rename = "projectID")]
    pub project_id: String,
    pub project_name: String,
    pub command: String,
    pub port: Option<u16>,
    pub directory: String,
    pub state: ServerState,
    pub pid: Option<u32>,
    pub started_at: Option<DateTime<Utc>>,
    pub restart_count: u32,
    pub last_exit_code: Option<i32>,
    pub last_error: Option<String>,
    pub healthy: bool,
    pub url: Option<String>,
    pub cpu_percent: Option<f64>,
    pub memory_bytes: Option<u64>,
    pub resident_memory_bytes: Option<u64>,
    pub process_count: Option<u32>,
    pub temporary: Option<bool>,
    pub timeout_seconds: Option<u64>,
    pub deadline: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub timed_out: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectStatus {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub color: String,
    pub root: String,
    pub servers: Vec<ServerStatus>,
    #[serde(default)]
    pub memory_limit_mode: MemoryLimitMode,
    #[serde(default)]
    pub memory_limit_bytes: Option<u64>,
    #[serde(default)]
    pub effective_memory_limit_bytes: Option<u64>,
    #[serde(default)]
    pub last_memory_restart_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_memory_restart_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetsuStackStatus {
    pub version: String,
    pub api_port: u16,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub global_memory_limit_bytes: Option<u64>,
    pub projects: Vec<ProjectStatus>,
    #[serde(default)]
    pub temporary_servers: Vec<ServerStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortOccupant {
    pub port: u16,
    pub pid: u32,
    pub command: String,
    pub user: String,
    #[serde(rename = "ownedByNetsuStack", alias = "ownedByPortly")]
    pub owned_by_netsustack: bool,
    #[serde(rename = "serverID")]
    pub server_id: Option<String>,
    #[serde(rename = "dockerContainerID")]
    pub docker_container_id: Option<String>,
    pub docker_container_name: Option<String>,
    pub docker_compose_project: Option<String>,
    pub docker_compose_service: Option<String>,
}
