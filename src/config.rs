use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub procs: HashMap<String, ProcConfig>,
}

#[derive(Debug, Deserialize)]
pub struct ProcConfig {
    pub shell: Option<String>,
    pub cmd: Option<Vec<String>>,
}

impl ProcConfig {
    pub fn command(&self) -> Option<String> {
        if let Some(shell) = &self.shell {
            return Some(shell.clone());
        }
        self.cmd.as_ref().map(|args| args.join(" "))
    }
}

pub fn load(path: &Path) -> Result<Config> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    serde_yaml::from_str(&text).context("parsing config")
}

pub fn find_config() -> Option<std::path::PathBuf> {
    for name in &["mprocs.yml", "mprocs.yaml", "tmprocs.yml", "tmprocs.yaml"] {
        let p = std::path::PathBuf::from(name);
        if p.exists() {
            return Some(p);
        }
    }
    None
}
