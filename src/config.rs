use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub fn config_dir() -> PathBuf {
    if let Ok(p) = std::env::var("CLASS_CONFIG_DIR") {
        return PathBuf::from(p);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("class")
}

pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("class")
}

fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub active_account: Option<String>,
    pub default_dir: Option<String>,
    pub folder_template: String,
    pub download_materials: bool,
    pub browser: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            active_account: None,
            default_dir: None,
            folder_template: "{course}_{title}".into(),
            download_materials: true,
            browser: None,
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(&path)?;
        toml::from_str(&text).with_context(|| format!("Invalid config file {}", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        fs::create_dir_all(config_dir())?;
        fs::write(config_path(), toml::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn path() -> PathBuf {
        config_path()
    }

    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        let opt = |v: &str| (!v.is_empty()).then(|| v.to_string());
        match key {
            "default_dir" => self.default_dir = opt(value),
            "folder_template" => {
                if !value.contains("{title}") {
                    bail!("Template must contain {{title}}");
                }
                self.folder_template = value.into()
            }
            "download_materials" => self.download_materials = matches!(value, "true" | "1" | "yes"),
            "active_account" => self.active_account = opt(value),
            "browser" => {
                if !value.is_empty() && !value.contains('/') {
                    crate::browser::kind(value)?;
                }
                self.browser = opt(value)
            }
            _ => bail!(
                "Unknown key '{key}'. Available: default_dir, folder_template, download_materials, active_account, browser"
            ),
        }
        Ok(())
    }

    pub fn default_dir(&self) -> Option<PathBuf> {
        self.default_dir.as_deref().map(expand_tilde)
    }
}

pub fn expand_tilde(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(s)
}

pub fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(path)
        .with_context(|| format!("Cannot write {}", path.display()))?;
    f.write_all(data)?;
    Ok(())
}
