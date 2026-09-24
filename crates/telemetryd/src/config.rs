//! Daemon configuration (`config.toml`).
//!
//! Only the fields used so far are defined; later milestones add the
//! `[cloud]`, `[output]` and `[metrics]` sections from the spec.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub vin: String,
    pub model: String,
    pub catalog_path: PathBuf,
    pub ingest: IngestConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IngestConfig {
    pub udp_bind: SocketAddr,
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_configs_parse() {
        for text in [
            include_str!("../../../dev/config.toml"),
            include_str!("../../../dev/config.r2.toml"),
        ] {
            let cfg: Config = toml::from_str(text).unwrap();
            assert!(!cfg.vin.is_empty());
            assert_eq!(cfg.ingest.udp_bind.ip().to_string(), "127.0.0.1");
        }
    }
}
