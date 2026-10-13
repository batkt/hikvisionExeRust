use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub server:       ServerConfig,
    pub cameras:      Vec<CameraEntry>,
    pub barilgiin_id: Option<String>,
    #[serde(default = "default_port")]
    pub api_port:     u16,
    /// LED самбарын машингүй (idle) үеийн мөрүүд. "plate", "YYYY-MM-DD", "hh-mm-ss" токенуудыг камер өөрөө орлуулна
    #[serde(default = "default_sambar_idle")]
    pub sambar_idle:      Vec<String>,
    /// /api/sambar-аар гаргасан текстийг хэдэн секунд барьсны дараа idle руу буцаах
    #[serde(default = "default_sambar_hold_secs")]
    pub sambar_hold_secs: u64,
}

fn default_port() -> u16 { 5000 }
fn default_sambar_idle() -> Vec<String> { vec!["Тавтай морил".into(), "YYYY-MM-DD".into()] }
fn default_sambar_hold_secs() -> u64 { 20 }

#[derive(Debug, Deserialize, Clone)]
pub struct ServerConfig {
    pub url:   String,
    pub token: String,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

fn default_timeout() -> u64 { 30 }

#[derive(Debug, Deserialize, Clone)]
pub struct CameraEntry {
    pub role:         Option<String>,
    pub ip:           String,
    #[serde(default = "default_http_port")]
    pub port:         u16,
    #[serde(default = "default_username")]
    pub username:     String,
    pub password:     String,
    /// Тусдаа сүлжээний LED контроллер (vtLEDProtocol.dll, TCP 10000). Байхгүй бол камерын
    /// RS-485 самбарыг ISAPI-аар удирдана
    pub led_ip:       Option<String>,
    /// false бол камерын ISAPI LED удирдлагыг унтраана (самбаргүй камер)
    #[serde(default = "default_true")]
    pub led_isapi:    bool,
    pub barilgiin_id: Option<String>,
}

fn default_http_port() -> u16 { 80 }
fn default_true() -> bool { true }
fn default_username() -> String { "admin".to_string() }

/// config.toml-г exe-ийн хавтаснаас, байхгүй бол exe-ийн хажуугийн dlls/ хавтаснаас хайна.
pub fn load_config() -> Result<Config> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));

    let candidates = [exe_dir.join("config.toml"), exe_dir.join("dlls").join("config.toml")];
    let path = candidates
        .iter()
        .find(|p| p.exists())
        .unwrap_or(&candidates[0]);

    let content = std::fs::read_to_string(path)
        .with_context(|| format!("config.toml олдсонгүй: {}", path.display()))?;

    toml::from_str(&content).context("config.toml задруулахад алдаа")
}
