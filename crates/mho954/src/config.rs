//! `~/.config/mho954/config.toml`

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::Context;
use mho_service::LiveConfig;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// `usbtmc` (auto-detect), `/dev/usbtmc0`, `tcp:192.168.1.20[:5555]` or `sim`.
    pub connection: String,
    /// Address of `mho954 serve`. Loopback by default; the API has no authentication.
    pub bind: SocketAddr,
    /// Allow arbitrary SCPI via REST/MCP.
    pub allow_raw_scpi: bool,
    pub capture_dir: PathBuf,
    pub backup_dir: PathBuf,
    /// Newest firmware you know of (Rigol website); `fw status` compares against it.
    pub latest_firmware: String,
    pub live: LiveConfig,
}

impl Default for Config {
    fn default() -> Self {
        let home = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf()).unwrap_or_else(|| ".".into());
        Config {
            connection: "usbtmc".into(),
            bind: "127.0.0.1:8954".parse().unwrap(),
            allow_raw_scpi: false,
            capture_dir: home.join("mho954-captures"),
            backup_dir: home.join("mho954-backups"),
            latest_firmware: "00.01.00.00.26".into(),
            live: LiveConfig::default(),
        }
    }
}

impl Config {
    pub fn default_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "mho954").map(|d| d.config_dir().join("config.toml"))
    }

    /// Loads the config file if it exists, defaults otherwise.
    pub fn load(path: Option<&Path>) -> anyhow::Result<Config> {
        let path = match path {
            Some(p) => p.to_path_buf(),
            None => match Self::default_path() {
                Some(p) => p,
                None => return Ok(Config::default()),
            },
        };
        if !path.exists() {
            return Ok(Config::default());
        }
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn url(&self) -> String {
        let host = if self.bind.ip().is_unspecified() { "127.0.0.1".to_string() } else { self.bind.ip().to_string() };
        format!("http://{host}:{}", self.bind.port())
    }
}
