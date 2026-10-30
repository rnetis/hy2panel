mod admin;
mod auth;
mod config;
mod db;
mod models;
mod settings;
mod state;
mod stats;
mod sub;
mod ui;
mod util;

use crate::state::App;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, RwLock};

fn app_cfg_poll_secs(cfg: &config::Config) -> u64 {
    if cfg.traffic_stats.enabled {
        cfg.traffic_stats.poll_secs + 5
    } else {
        120 // no stats API: keep a 2 minute heuristic window
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "config.toml".to_string());
    let cfg = config::Config::load(&cfg_path)?;
    if !cfg.auth_path.starts_with('/') {
        anyhow::bail!("auth_path must start with '/'");
    }

    let conn = db::open(&cfg.db_path)?;
    db::migrate(&conn)?;
    let set = settings::load(&conn, &cfg);

    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()?;

    let recent_ttl = std::time::Duration::from_secs(
        app_cfg_poll_secs(&cfg).max(10),
    );
    let app = Arc::new(App {
        db: Mutex::new(conn),
        cfg,
        set: RwLock::new(set.clone()),
        online: RwLock::new(HashMap::new()),
        stats_up: RwLock::new(false),
        recent: Mutex::new(HashMap::new()),
        http,
        sessions: Mutex::new(HashMap::new()),
        login_attempts: Mutex::new(HashMap::new()),
        recent_ttl,
    });

    stats::spawn(app.clone());

    let addr: SocketAddr = app.cfg.listen.parse()?;
    println!("hy2panel v{}", env!("CARGO_PKG_VERSION"));
    println!("  admin UI : http://{addr}/admin");
    println!("  auth API : http://{addr}{}   <- set as hysteria2 auth.url", set.auth_path);
    if app.cfg.traffic_stats.enabled {
        println!(
            "  stats API: {} (poll every {}s)",
            app.cfg.traffic_stats.url, app.cfg.traffic_stats.poll_secs
        );
    } else {
        println!(
            "  stats API: DISABLED — usage accounting & device limits degraded;\n             enable trafficStats in the hysteria2 server config"
        );
    }
    println!("  template : {}", app.cfg.template_path);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(
        listener,
        ui::router(app).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}
