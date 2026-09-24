//! Daemon configuration (`config.toml`, spec Section 7.2).
//!
//! Sections are added as the milestones that use them land (`[cloud]` in M5,
//! `[metrics]` in M7). Unknown keys are rejected so typos fail loudly.
//! Relative paths are resolved against the working directory.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{ensure, Context};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub vin: String,
    pub model: String,
    pub catalog_path: PathBuf,
    /// Temporary (M4): load an unsigned rule set from a local file instead of
    /// the cloud. Without it the daemon runs with an empty rule set.
    #[serde(default)]
    pub rules_file: Option<PathBuf>,
    pub ingest: IngestConfig,
    pub output: OutputConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IngestConfig {
    pub udp_bind: SocketAddr,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputConfig {
    pub log_dir: PathBuf,
    /// Rotate the current file once it would exceed this size.
    #[serde(default = "default_max_file_bytes")]
    pub max_file_bytes: u64,
    /// Total files kept, including the one being written.
    #[serde(default = "default_max_files")]
    pub max_files: u32,
}

fn default_max_file_bytes() -> u64 {
    10 * 1024 * 1024
}

fn default_max_files() -> u32 {
    5
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("invalid config {}", path.display()))
    }

    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let cfg: Config = toml::from_str(text)?;
        ensure!(!cfg.vin.trim().is_empty(), "vin must not be empty");
        ensure!(!cfg.model.trim().is_empty(), "model must not be empty");
        ensure!(
            cfg.output.max_file_bytes >= 1024,
            "output.max_file_bytes must be at least 1024"
        );
        ensure!(
            cfg.output.max_files >= 1,
            "output.max_files must be at least 1"
        );
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
        vin = "V1"
        model = "R1S"
        catalog_path = "c.json"
        [ingest]
        udp_bind = "127.0.0.1:5005"
        [output]
        log_dir = "out"
    "#;

    #[test]
    fn dev_configs_parse() {
        for text in [
            include_str!("../../../dev/config.toml"),
            include_str!("../../../dev/config.r2.toml"),
        ] {
            let cfg = Config::parse(text).unwrap();
            assert_eq!(cfg.ingest.udp_bind.ip().to_string(), "127.0.0.1");
            assert!(cfg.rules_file.is_some());
        }
    }

    #[test]
    fn output_defaults_match_spec() {
        let cfg = Config::parse(MINIMAL).unwrap();
        assert_eq!(cfg.output.max_file_bytes, 10 * 1024 * 1024);
        assert_eq!(cfg.output.max_files, 5);
        assert!(cfg.rules_file.is_none());
    }

    #[test]
    fn rejects_unknown_keys_and_bad_values() {
        assert!(Config::parse(&format!("bogus = 1\n{MINIMAL}")).is_err());
        assert!(Config::parse(&format!("{MINIMAL}\nmax_filez = 3")).is_err());
        assert!(Config::parse(&MINIMAL.replace("\"V1\"", "\" \"")).is_err());
        let tiny = MINIMAL.replace(
            "log_dir = \"out\"",
            "log_dir = \"out\"\nmax_file_bytes = 10",
        );
        assert!(Config::parse(&tiny).is_err());
        let none = MINIMAL.replace("log_dir = \"out\"", "log_dir = \"out\"\nmax_files = 0");
        assert!(Config::parse(&none).is_err());
    }
}
