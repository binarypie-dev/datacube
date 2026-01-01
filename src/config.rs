//! Configuration management for datacube

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::info;

/// Main configuration struct
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Socket path (default: $XDG_RUNTIME_DIR/datacube.sock)
    #[serde(default = "default_socket_path")]
    pub socket_path: PathBuf,

    /// Maximum results per provider
    #[serde(default = "default_max_results")]
    pub max_results: usize,

    /// Provider-specific configuration
    #[serde(default)]
    pub providers: ProvidersConfig,
}

/// Provider-specific configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProvidersConfig {
    /// Applications provider config
    #[serde(default)]
    pub applications: ApplicationsConfig,

    /// Calculator provider config
    #[serde(default)]
    pub calculator: CalculatorConfig,
}

/// Applications provider configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplicationsConfig {
    /// Whether this provider is enabled
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Terminal emulator to use for terminal apps
    #[serde(default = "default_terminal")]
    pub terminal: String,

    /// Additional directories to search for .desktop files
    #[serde(default)]
    pub extra_dirs: Vec<PathBuf>,
}

impl Default for ApplicationsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            terminal: default_terminal(),
            extra_dirs: Vec::new(),
        }
    }
}

/// Calculator provider configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalculatorConfig {
    /// Whether this provider is enabled
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Prefix to trigger calculator (default: "=")
    #[serde(default = "default_calc_prefix")]
    pub prefix: String,
}

impl Default for CalculatorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            prefix: default_calc_prefix(),
        }
    }
}

// Default value functions for serde
fn default_socket_path() -> PathBuf {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| format!("/run/user/{}", unsafe { libc::getuid() }));
    PathBuf::from(runtime_dir).join("datacube.sock")
}

fn default_max_results() -> usize {
    50
}

fn default_true() -> bool {
    true
}

fn default_terminal() -> String {
    "foot".to_string()
}

fn default_calc_prefix() -> String {
    "=".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            socket_path: default_socket_path(),
            max_results: default_max_results(),
            providers: ProvidersConfig::default(),
        }
    }
}

impl Config {
    /// Load configuration from file or use defaults
    pub fn load() -> Self {
        let config_path = Self::config_path();

        if config_path.exists() {
            match std::fs::read_to_string(&config_path) {
                Ok(content) => match toml::from_str(&content) {
                    Ok(config) => {
                        info!("Loaded config from {:?}", config_path);
                        return config;
                    }
                    Err(e) => {
                        tracing::warn!("Failed to parse config: {}", e);
                    }
                },
                Err(e) => {
                    tracing::warn!("Failed to read config: {}", e);
                }
            }
        }

        info!("Using default configuration");
        Self::default()
    }

    /// Get the config file path
    pub fn config_path() -> PathBuf {
        let config_dir = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("/"))
                    .join(".config")
            });

        config_dir.join("datacube").join("config.toml")
    }
}
