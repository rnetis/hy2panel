use crate::config::Config;
use crate::db;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

/// Runtime settings: config.toml defaults overlaid with DB values
/// (editable from the admin UI).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    pub base_url: String,
    pub hy2_server: String,
    pub hy2_server_port: u16,
    pub hy2_sni: String,
    pub hy2_insecure: bool,
    pub hy2_obfs_type: String,
    pub hy2_obfs_password: String,
    pub profile_name: String,
    /// not UI-editable; comes from config.toml
    pub auth_path: String,
}

pub fn from_config(cfg: &Config) -> Settings {
    Settings {
        base_url: cfg.base_url.trim_end_matches('/').to_string(),
        hy2_server: cfg.hysteria2.server.clone(),
        hy2_server_port: cfg.hysteria2.server_port,
        hy2_sni: cfg.hysteria2.sni.clone(),
        hy2_insecure: cfg.hysteria2.insecure,
        hy2_obfs_type: cfg.hysteria2.obfs_type.clone(),
        hy2_obfs_password: cfg.hysteria2.obfs_password.clone(),
        profile_name: cfg.profile_name.clone(),
        auth_path: cfg.auth_path.clone(),
    }
}

pub fn load(conn: &Connection, cfg: &Config) -> Settings {
    let mut s = from_config(cfg);
    let get = |k: &str| db::get_setting(conn, k);
    if let Some(v) = get("base_url") {
        s.base_url = v.trim_end_matches('/').to_string();
    }
    if let Some(v) = get("hy2_server") {
        s.hy2_server = v;
    }
    if let Some(v) = get("hy2_server_port") {
        if let Ok(p) = v.parse::<u16>() {
            s.hy2_server_port = p;
        }
    }
    if let Some(v) = get("hy2_sni") {
        s.hy2_sni = v;
    }
    if let Some(v) = get("hy2_insecure") {
        s.hy2_insecure = v == "1" || v == "true";
    }
    if let Some(v) = get("hy2_obfs_type") {
        s.hy2_obfs_type = v;
    }
    if let Some(v) = get("hy2_obfs_password") {
        s.hy2_obfs_password = v;
    }
    if let Some(v) = get("profile_name") {
        s.profile_name = v;
    }
    s
}

pub fn save(conn: &Connection, s: &Settings) {
    let put = |k: &str, v: String| db::set_setting(conn, k, &v);
    put("base_url", s.base_url.clone());
    put("hy2_server", s.hy2_server.clone());
    put("hy2_server_port", s.hy2_server_port.to_string());
    put("hy2_sni", s.hy2_sni.clone());
    put("hy2_insecure", if s.hy2_insecure { "1".into() } else { "0".into() });
    put("hy2_obfs_type", s.hy2_obfs_type.clone());
    put("hy2_obfs_password", s.hy2_obfs_password.clone());
    put("profile_name", s.profile_name.clone());
}

/// Input body for PUT /api/settings.
#[derive(Deserialize)]
pub struct SettingsInput {
    pub base_url: Option<String>,
    pub hy2_server: Option<String>,
    pub hy2_server_port: Option<u16>,
    pub hy2_sni: Option<String>,
    pub hy2_insecure: Option<bool>,
    pub hy2_obfs_type: Option<String>,
    pub hy2_obfs_password: Option<String>,
    pub profile_name: Option<String>,
}
