use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPaths {
    root: PathBuf,
}

impl ConfigPaths {
    pub fn from_user_profile(user_profile: &Path) -> Self {
        Self {
            root: user_profile.join(".config").join("netsustack"),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn config_file(&self) -> PathBuf {
        self.root.join("config.json")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    pub fn token_file(&self) -> PathBuf {
        self.root.join("api-token")
    }

    pub fn resume_after_update_file(&self) -> PathBuf {
        self.root.join("resume-after-update.json")
    }
}
