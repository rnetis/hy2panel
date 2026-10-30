use serde::Deserialize;

fn d_listen() -> String { "127.0.0.1:8080".into() }
fn d_auth_path() -> String { "/auth".into() }
fn d_db() -> String { "panel.db".into() }
fn d_admin() -> String { "CHANGE_ME".into() }
fn d_port() -> u16 { 443 }
fn d_obfs_type() -> String { "salamander".into() }
fn d_stats_url() -> String { "http://127.0.0.1:9999".into() }
fn d_poll() -> u64 { 30 }
fn d_profile() -> String { "HY2".into() }
fn d_tpl() -> String { "template.singbox.json".into() }

#[derive(Deserialize, Clone)]
pub struct Config {
    #[serde(default = "d_listen")]
    pub listen: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default = "d_auth_path")]
    pub auth_path: String,
    #[serde(default = "d_admin")]
    pub admin_password: String,
    #[serde(default = "d_db")]
    pub db_path: String,
    #[serde(default = "d_tpl")]
    pub template_path: String,
    #[serde(default = "d_true")]
    pub auto_kick: bool,
    #[serde(default = "d_profile")]
    pub profile_name: String,
    #[serde(default)]
    pub hysteria2: Hy2Cfg,
    #[serde(default)]
    pub traffic_stats: StatsCfg,
}

fn d_true() -> bool { true }

#[derive(Deserialize, Clone, Default)]
pub struct Hy2Cfg {
    #[serde(default)]
    pub server: String,
    #[serde(default = "d_port")]
    pub server_port: u16,
    #[serde(default)]
    pub sni: String,
    #[serde(default)]
    pub insecure: bool,
    #[serde(default = "d_obfs_type")]
    pub obfs_type: String,
    #[serde(default)]
    pub obfs_password: String,
}

#[derive(Deserialize, Clone)]
pub struct StatsCfg {
    #[serde(default = "d_true")]
    pub enabled: bool,
    #[serde(default = "d_stats_url")]
    pub url: String,
    #[serde(default)]
    pub secret: String,
    #[serde(default = "d_poll")]
    pub poll_secs: u64,
}

impl Default for StatsCfg {
    fn default() -> Self {
        StatsCfg { enabled: false, url: d_stats_url(), secret: String::new(), poll_secs: d_poll() }
    }
}

impl Config {
    pub fn load(path: &str) -> anyhow::Result<Config> {
        let raw = std::fs::read_to_string(path).map_err(|e| {
            anyhow::anyhow!(
                "cannot read config file '{path}': {e}\nCopy config.example.toml to config.toml and edit it."
            )
        })?;
        let cfg: Config = toml::from_str(&raw)?;
        Ok(cfg)
    }
}
