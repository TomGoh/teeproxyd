use serde::Deserialize;
use std::path::PathBuf;

/// Daemon configuration. All fields have sensible defaults via `#[serde(default)]`.
/// Loaded from `/data/teeproxy/teeproxyd.conf` (JSON) if present.
#[derive(Debug, Deserialize)]
pub struct Config {
    // VM
    #[serde(default = "default_image_dir")]
    pub image_dir: PathBuf,
    #[serde(default = "default_vm_cpus")]
    pub vm_cpus: u32,
    #[serde(default = "default_vm_mem_mb")]
    pub vm_mem_mb: u32,
    #[serde(default = "default_vsock_cid")]
    pub vsock_cid: u32,

    // CA
    #[serde(default = "default_ca_binary")]
    pub ca_binary: PathBuf,
    #[serde(default = "default_ca_port")]
    pub ca_port: u16,
    #[serde(default = "default_ca_admin_token")]
    pub ca_admin_token: String,
    #[serde(default = "default_ca_log_level")]
    pub ca_log_level: String,

    // Daemon
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    #[serde(default = "default_log_dir")]
    pub log_dir: PathBuf,
    #[serde(default = "default_auto_start")]
    pub auto_start: bool,
    #[serde(default = "default_health_interval_secs")]
    pub health_interval_secs: u64,
    #[serde(default = "default_health_fail_threshold")]
    pub health_fail_threshold: u32,

    // Timeouts
    #[serde(default = "default_vm_boot_timeout_secs")]
    pub vm_boot_timeout_secs: u64,
    #[serde(default = "default_vsock_probe_delay_secs")]
    pub vsock_probe_delay_secs: u64,
    #[serde(default = "default_ca_ready_timeout_secs")]
    pub ca_ready_timeout_secs: u64,
    #[serde(default = "default_shutdown_grace_secs")]
    pub shutdown_grace_secs: u64,
}

fn default_image_dir() -> PathBuf { PathBuf::from("/data/teeproxy/vm") }
fn default_vm_cpus() -> u32 { 2 }
fn default_vm_mem_mb() -> u32 { 256 }
fn default_vsock_cid() -> u32 { 103 }
fn default_ca_binary() -> PathBuf { PathBuf::from("/data/teeproxy/bin/secret_proxy_ca") }
fn default_ca_port() -> u16 { 19030 }
fn default_ca_admin_token() -> String { "dev-admin-token-change-me-please-0001".into() }
fn default_ca_log_level() -> String { "info".into() }
fn default_data_dir() -> PathBuf { PathBuf::from("/data/teeproxy") }
fn default_log_dir() -> PathBuf { PathBuf::from("/data/teeproxy/logs") }
fn default_auto_start() -> bool { true }
fn default_health_interval_secs() -> u64 { 10 }
fn default_health_fail_threshold() -> u32 { 3 }
fn default_vm_boot_timeout_secs() -> u64 { 60 }
fn default_vsock_probe_delay_secs() -> u64 { 15 }
fn default_ca_ready_timeout_secs() -> u64 { 15 }
fn default_shutdown_grace_secs() -> u64 { 3 }

impl Default for Config {
    fn default() -> Self {
        Self {
            image_dir: default_image_dir(),
            vm_cpus: default_vm_cpus(),
            vm_mem_mb: default_vm_mem_mb(),
            vsock_cid: default_vsock_cid(),
            ca_binary: default_ca_binary(),
            ca_port: default_ca_port(),
            ca_admin_token: default_ca_admin_token(),
            ca_log_level: default_ca_log_level(),
            data_dir: default_data_dir(),
            log_dir: default_log_dir(),
            auto_start: default_auto_start(),
            health_interval_secs: default_health_interval_secs(),
            health_fail_threshold: default_health_fail_threshold(),
            vm_boot_timeout_secs: default_vm_boot_timeout_secs(),
            vsock_probe_delay_secs: default_vsock_probe_delay_secs(),
            ca_ready_timeout_secs: default_ca_ready_timeout_secs(),
            shutdown_grace_secs: default_shutdown_grace_secs(),
        }
    }
}

impl Config {
    pub fn load_with_warnings() -> Self {
        let path = PathBuf::from("/data/teeproxy/teeproxyd.conf");
        if !path.exists() {
            log::info!("no config at {}, using defaults", path.display());
            return Self::default();
        }
        match std::fs::read_to_string(&path) {
            Ok(content) => match serde_json::from_str(&content) {
                Ok(config) => {
                    log::info!("loaded config from {}", path.display());
                    config
                }
                Err(e) => {
                    log::warn!("config parse error: {e}, using defaults");
                    Self::default()
                }
            },
            Err(e) => {
                log::warn!("config read error: {e}, using defaults");
                Self::default()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        let c = Config::default();
        assert_eq!(c.ca_port, 19030);
        assert_eq!(c.vsock_cid, 103);
        assert!(c.auto_start);
    }

    #[test]
    fn partial_json_uses_defaults_for_missing() {
        let json = r#"{"vm_cpus": 4}"#;
        let c: Config = serde_json::from_str(json).unwrap();
        assert_eq!(c.vm_cpus, 4);
        assert_eq!(c.ca_port, 19030); // default
    }

    #[test]
    fn empty_json_gives_all_defaults() {
        let c: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(c.vm_cpus, 2);
        assert_eq!(c.vm_mem_mb, 256);
        assert_eq!(c.vsock_cid, 103);
        assert!(c.auto_start);
    }

    #[test]
    fn malformed_json_returns_error() {
        let result: Result<Config, _> = serde_json::from_str("{broken");
        assert!(result.is_err());
    }

    #[test]
    fn wrong_type_field_fails_entire_parse() {
        // serde(default) per-field only protects missing fields, not wrong types
        let json = r#"{"vm_cpus": "not_a_number"}"#;
        let result: Result<Config, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn all_fields_overridden() {
        let json = r#"{
            "image_dir": "/tmp/vm",
            "vm_cpus": 8,
            "vm_mem_mb": 4096,
            "vsock_cid": 200,
            "ca_binary": "/tmp/ca",
            "ca_port": 9999,
            "ca_admin_token": "my-secure-token-that-is-long-enough",
            "ca_log_level": "debug",
            "data_dir": "/tmp/data",
            "log_dir": "/tmp/logs",
            "auto_start": false,
            "health_interval_secs": 30,
            "health_fail_threshold": 5,
            "vm_boot_timeout_secs": 120,
            "vsock_probe_delay_secs": 20,
            "ca_ready_timeout_secs": 30,
            "shutdown_grace_secs": 5
        }"#;
        let c: Config = serde_json::from_str(json).unwrap();
        assert_eq!(c.vm_cpus, 8);
        assert_eq!(c.vsock_cid, 200);
        assert_eq!(c.ca_port, 9999);
        assert!(!c.auto_start);
    }
}
