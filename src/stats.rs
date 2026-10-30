use crate::state::App;
use crate::{db, models, util};
use serde_json::Value;
use std::collections::HashMap;

/// Background loop: poll hysteria2 Traffic Stats API, accumulate per-user
/// usage in SQLite, track online device counts, and auto-kick blocked users.
pub fn spawn(app: std::sync::Arc<App>) {
    let secs = app.cfg.traffic_stats.poll_secs.clamp(5, 3600);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(secs));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            if let Err(e) = poll_once(&app).await {
                elog(&format!("stats poll failed: {e}"));
            }
        }
    });
}

fn ts() -> String {
    util::now_iso()
}

fn elog(msg: &str) {
    println!("[{}] [stats] {msg}", ts());
}

async fn poll_once(app: &std::sync::Arc<App>) -> anyhow::Result<()> {
    if !app.cfg.traffic_stats.enabled {
        return Ok(());
    }
    let users = {
        let conn = app.db.lock().unwrap();
        db::list_users(&conn)
    };
    let usernames: Vec<String> = users.iter().map(|u| u.username.clone()).collect();

    let traffic = fetch_traffic(app, &usernames).await;
    let online = fetch_online(app, &usernames).await;

    match &traffic {
        Some(map) => {
            *app.stats_up.write().unwrap() = true;
            apply_deltas(app, &users, map);
        }
        None => {
            // keep the last known online map; counters are untouched and
            // deltas resume from them on the next successful poll
            *app.stats_up.write().unwrap() = false;
            elog("traffic stats API unreachable (usage accounting degraded)");
        }
    }

    if let Some(map) = online {
        app.set_online_map(map);
    }

    enforce(app, &users).await;
    Ok(())
}

/// GET /traffic — bulk on hysteria >= 2.6, falls back to per-user /traffic/{id}.
async fn fetch_traffic(
    app: &std::sync::Arc<App>,
    usernames: &[String],
) -> Option<HashMap<String, (i64, i64)>> {
    let url = format!("{}/traffic", app.cfg.traffic_stats.url.trim_end_matches('/'));
    if let Some(v) = get_json(app, &url).await {
        let mut out = HashMap::new();
        if let Some(obj) = v.as_object() {
            for (user, entry) in obj {
                if let (Some(tx), Some(rx)) = (
                    entry.get("tx").and_then(Value::as_i64),
                    entry.get("rx").and_then(Value::as_i64),
                ) {
                    out.insert(user.clone(), (tx, rx));
                }
            }
        }
        return Some(out);
    }
    // legacy per-user endpoints
    let mut out = HashMap::new();
    for u in usernames {
        let url = format!(
            "{}/traffic/{}",
            app.cfg.traffic_stats.url.trim_end_matches('/'),
            crate::util::pct(u)
        );
        if let Some(v) = get_json(app, &url).await {
            if let (Some(tx), Some(rx)) = (
                v.get("tx").and_then(Value::as_i64),
                v.get("rx").and_then(Value::as_i64),
            ) {
                out.insert(u.clone(), (tx, rx));
            }
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// GET /online — bulk map user -> device count, falls back to /online/{id}.
async fn fetch_online(
    app: &std::sync::Arc<App>,
    usernames: &[String],
) -> Option<HashMap<String, u32>> {
    let url = format!("{}/online", app.cfg.traffic_stats.url.trim_end_matches('/'));
    if let Some(v) = get_json(app, &url).await {
        let mut out = HashMap::new();
        if let Some(obj) = v.as_object() {
            for (user, n) in obj {
                if let Some(n) = n.as_u64() {
                    out.insert(user.clone(), n as u32);
                }
            }
        }
        return Some(out);
    }
    let mut out = HashMap::new();
    for u in usernames {
        let url = format!(
            "{}/online/{}",
            app.cfg.traffic_stats.url.trim_end_matches('/'),
            crate::util::pct(u)
        );
        if let Some(v) = get_json(app, &url).await {
            if let Some(n) = v.get("online").and_then(Value::as_u64) {
                out.insert(u.clone(), n as u32);
            }
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

async fn get_json(app: &std::sync::Arc<App>, url: &str) -> Option<Value> {
    let mut req = app.http.get(url).timeout(std::time::Duration::from_secs(5));
    if !app.cfg.traffic_stats.secret.is_empty() {
        req = req.header("Authorization", app.cfg.traffic_stats.secret.clone());
    }
    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<Value>().await.ok()
}

/// Convert cumulative hysteria counters into deltas and store them per day.
fn apply_deltas(
    app: &std::sync::Arc<App>,
    users: &[models::User],
    traffic: &HashMap<String, (i64, i64)>,
) {
    let conn = app.db.lock().unwrap();
    let today = util::today();
    for u in users {
        let Some(&(tx, rx)) = traffic.get(&u.username) else {
            continue;
        };
        let old = db::get_counter(&conn, &u.username);
        let (dtx, drx) = match old {
            Some((otx, orx)) if tx >= otx && rx >= orx => (tx - otx, rx - orx),
            // counter reset (server restart or ?clear=1): treat everything as new
            _ => (tx, rx),
        };
        db::set_counter(&conn, &u.username, tx, rx);
        if dtx > 0 || drx > 0 {
            db::add_usage(&conn, &u.username, &today, dtx, drx);
        }
    }
}

/// Deny service to users that are expired / over quota / disabled:
/// they are already rejected at auth time; if they are still online, kick them.
async fn enforce(app: &std::sync::Arc<App>, users: &[models::User]) {
    if !app.cfg.auto_kick {
        return;
    }
    let to_kick: Vec<String> = {
        let conn = app.db.lock().unwrap();
        users
            .iter()
            .filter(|u| {
                let used = u.used_bytes(&conn);
                u.blocked_reason(used).is_some() && app.online_count(&u.username) > 0
            })
            .map(|u| u.username.clone())
            .collect()
    };
    if !to_kick.is_empty() {
        elog(&format!("kicking blocked users: {to_kick:?}"));
        kick(app, &to_kick).await;
    }
}

/// POST /kick — disconnect hysteria clients by auth id.
pub async fn kick(app: &std::sync::Arc<App>, ids: &[String]) -> bool {
    if !app.cfg.traffic_stats.enabled {
        return false;
    }
    let url = format!("{}/kick", app.cfg.traffic_stats.url.trim_end_matches('/'));
    let mut req = app
        .http
        .post(&url)
        .timeout(std::time::Duration::from_secs(5))
        .json(&ids);
    if !app.cfg.traffic_stats.secret.is_empty() {
        req = req.header("Authorization", app.cfg.traffic_stats.secret.clone());
    }
    match req.send().await {
        Ok(r) => r.status().is_success(),
        Err(_) => false,
    }
}
