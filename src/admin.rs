use crate::db;
use crate::models::{valid_password, valid_username, UsageOut, User, UserInput};
use crate::settings::SettingsInput;
use crate::state::App;
use crate::stats;
use crate::util;
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

const SESSION_SECS: i64 = 7 * 24 * 3600;
const COOKIE: &str = "hpanel_session";

fn ok_json(v: Value) -> Response {
    Json(v).into_response()
}

fn err(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({ "error": msg }))).into_response()
}

fn session_cookie(app: &App, token: &str) -> String {
    let secure = if app.settings().base_url.starts_with("https://") {
        "; Secure"
    } else {
        ""
    };
    format!("{COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={SESSION_SECS}{secure}")
}

// ---- auth (admin) -----------------------------------------------------------

#[derive(Deserialize)]
pub struct LoginReq {
    pub password: String,
}

pub async fn login(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(b): Json<LoginReq>,
) -> Response {
    let ip = addr.ip().to_string();
    {
        let mut m = app.login_attempts.lock().unwrap();
        let e = m.entry(ip.clone()).or_insert((0, Instant::now()));
        if e.1.elapsed() > Duration::from_secs(60) {
            *e = (0, Instant::now());
        }
        e.0 += 1;
        if e.0 > 10 {
            return err(StatusCode::TOO_MANY_REQUESTS, "too many attempts, wait a minute");
        }
    }

    let hash = util::admin_hash(&b.password);
    let valid = {
        let conn = app.db.lock().unwrap();
        match db::get_setting(&conn, "admin_hash") {
            Some(stored) => stored == hash,
            None => {
                // first run: adopt the password from config.toml
                let boot = util::admin_hash(&app.cfg.admin_password);
                if boot == hash {
                    db::set_setting(&conn, "admin_hash", &boot);
                    true
                } else {
                    false
                }
            }
        }
    };
    if !valid {
        return err(StatusCode::UNAUTHORIZED, "wrong password");
    }

    app.login_attempts.lock().unwrap().remove(&ip);
    let token = util::rand_hex(32);
    app.sessions
        .lock()
        .unwrap()
        .insert(token.clone(), util::unix_now() + SESSION_SECS);
    let cookie = session_cookie(&app, &token);
    ([(header::SET_COOKIE, cookie)], Json(json!({"ok": true}))).into_response()
}

pub async fn logout(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    if let Some(t) = util::cookie_token(&headers) {
        app.sessions.lock().unwrap().remove(&t);
    }
    let clear = format!("{COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0");
    ([(header::SET_COOKIE, clear)], Json(json!({"ok": true}))).into_response()
}

#[derive(Deserialize)]
pub struct PwReq {
    pub old: String,
    pub new: String,
}

pub async fn change_password(State(app): State<Arc<App>>, Json(b): Json<PwReq>) -> Response {
    if b.new.len() < 6 {
        return err(StatusCode::BAD_REQUEST, "new password too short (min 6)");
    }
    let old_hash = util::admin_hash(&b.old);
    let new_hash = util::admin_hash(&b.new);
    let conn = app.db.lock().unwrap();
    let stored = db::get_setting(&conn, "admin_hash")
        .unwrap_or_else(|| util::admin_hash(&app.cfg.admin_password));
    if stored != old_hash {
        return err(StatusCode::UNAUTHORIZED, "wrong current password");
    }
    db::set_setting(&conn, "admin_hash", &new_hash);
    drop(conn);
    ok_json(json!({"ok": true}))
}

// ---- overview ----------------------------------------------------------------

pub async fn overview(State(app): State<Arc<App>>) -> Response {
    let conn = app.db.lock().unwrap();
    let users = db::list_users(&conn);
    let mut active = 0;
    let mut online_devices = 0u32;
    for u in &users {
        let used = u.used_bytes(&conn);
        if u.status(used) == "active" {
            active += 1;
        }
        online_devices += app.online_count(&u.username);
    }
    let (today_tx, today_rx) = db::totals_since_day(&conn, &util::today());
    let (month_tx, month_rx) = db::totals_since_day(&conn, &util::month_start_day());
    drop(conn);
    ok_json(json!({
        "users": users.len(),
        "active": active,
        "online_devices": online_devices,
        "today_tx": today_tx,
        "today_rx": today_rx,
        "month_tx": month_tx,
        "month_rx": month_rx,
        "stats_up": *app.stats_up.read().unwrap(),
        "stats_enabled": app.cfg.traffic_stats.enabled,
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

// ---- users -------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ListParams {
    #[serde(default)]
    pub q: String,
}

fn user_view(app: &App, conn: &rusqlite::Connection, u: &User) -> Value {
    let used = u.used_bytes(conn);
    let (utx, urx) = u.used_split(conn);
    let set = app.settings();
    let base = if set.base_url.is_empty() { String::new() } else { set.base_url.clone() };
    let sub_url = format!("{}/sub/{}", base, u.sub_token);
    let limit = u.data_limit_bytes;
    json!({
        "username": u.username,
        "password": u.password,
        "sub_token": u.sub_token,
        "sub_url": sub_url,
        "enabled": u.enabled,
        "note": u.note,
        "expiry": u.expiry,
        "days_left": util::days_left(&u.expiry),
        "concurrent_limit": u.concurrent_limit,
        "data_limit_bytes": limit,
        "reset_mode": u.reset_mode,
        "reset_day": u.reset_day,
        "used_tx": utx,
        "used_rx": urx,
        "used_bytes": used,
        "online": app.online_count(&u.username),
        "status": u.status(used),
        "created_at": u.created_at,
    })
}

pub async fn list_users(State(app): State<Arc<App>>, Query(p): Query<ListParams>) -> Response {
    let conn = app.db.lock().unwrap();
    let users = db::list_users(&conn);
    let ql = p.q.to_lowercase();
    let views: Vec<Value> = users
        .iter()
        .filter(|u| ql.is_empty() || u.username.to_lowercase().contains(&ql) || u.note.to_lowercase().contains(&ql))
        .map(|u| user_view(&app, &conn, u))
        .collect();
    drop(conn);
    ok_json(json!({ "users": views }))
}

fn build_user_for_insert(app: &App, conn: &rusqlite::Connection, b: &UserInput) -> Result<User, Response> {
    let username = b
        .username
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| err(StatusCode::BAD_REQUEST, "username required"))?;
    if !valid_username(username) {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "username: 1-32 chars, letters/digits/_/- only",
        ));
    }
    if db::get_user(conn, username).is_some() {
        return Err(err(StatusCode::CONFLICT, "username already exists"));
    }

    // password: provided or generated, must be unique
    let password = match b.password.as_deref().map(str::trim) {
        Some(p) if !p.is_empty() => {
            if !valid_password(p) {
                return Err(err(
                    StatusCode::BAD_REQUEST,
                    "password: no spaces, max 128 chars",
                ));
            }
            p.to_string()
        }
        _ => {
            let mut gen = String::new();
            for _ in 0..20 {
                let p = util::gen_password(10);
                if !db::password_taken(conn, &p) {
                    gen = p;
                    break;
                }
            }
            if gen.is_empty() {
                return Err(err(StatusCode::INTERNAL_SERVER_ERROR, "could not generate password"));
            }
            gen
        }
    };
    if db::password_taken(conn, &password) {
        return Err(err(StatusCode::CONFLICT, "password already in use by another user"));
    }

    let reset_mode = match b.reset_mode.as_deref() {
        Some(m @ ("monthly" | "total" | "none")) => m.to_string(),
        Some(_) => return Err(err(StatusCode::BAD_REQUEST, "reset_mode must be monthly|total|none")),
        None => "monthly".to_string(),
    };
    let reset_day = b.reset_day.unwrap_or(1).clamp(1, 28);
    let expiry = match &b.expiry {
        Some(e) if !e.trim().is_empty() => {
            if util::parse_expiry(e).is_none() {
                return Err(err(StatusCode::BAD_REQUEST, "expiry: invalid datetime"));
            }
            Some(e.trim().to_string())
        }
        _ => None,
    };
    let data_limit_bytes = match b.data_limit_gb {
        Some(gb) if gb > 0.0 => Some((gb * 1024.0 * 1024.0 * 1024.0).round() as i64),
        Some(_) => None,
        None => None,
    };
    let _ = app;
    Ok(User {
        username: username.to_string(),
        password,
        sub_token: util::new_sub_token(),
        data_limit_bytes,
        reset_mode,
        reset_day,
        expiry,
        concurrent_limit: b.concurrent_limit.filter(|l| *l > 0),
        enabled: b.enabled.unwrap_or(true),
        note: b.note.clone().unwrap_or_default(),
        created_at: util::now_iso(),
    })
}

pub async fn create_user(State(app): State<Arc<App>>, Json(b): Json<UserInput>) -> Response {
    let conn = app.db.lock().unwrap();
    let user = match build_user_for_insert(&app, &conn, &b) {
        Ok(u) => u,
        Err(r) => return r,
    };
    if let Err(e) = db::insert_user(&conn, &user) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("db error: {e}"));
    }
    let view = user_view(&app, &conn, &user);
    drop(conn);
    (StatusCode::CREATED, Json(view)).into_response()
}

pub async fn update_user(
    State(app): State<Arc<App>>,
    Path(username): Path<String>,
    Json(b): Json<UserInput>,
) -> Response {
    let conn = app.db.lock().unwrap();
    let Some(mut u) = db::get_user(&conn, &username) else {
        return err(StatusCode::NOT_FOUND, "user not found");
    };
    if let Some(pw) = b.password.as_deref().map(str::trim) {
        if !pw.is_empty() {
            if !valid_password(pw) {
                return err(StatusCode::BAD_REQUEST, "password: no spaces, max 128 chars");
            }
            if let Some(other) = db::get_by_password(&conn, pw) {
                if other.username != username {
                    return err(StatusCode::CONFLICT, "password already in use by another user");
                }
            }
            u.password = pw.to_string();
        }
    }
    if let Some(note) = b.note {
        u.note = note;
    }
    if let Some(gb) = b.data_limit_gb {
        u.data_limit_bytes = if gb > 0.0 {
            Some((gb * 1024.0 * 1024.0 * 1024.0).round() as i64)
        } else {
            None
        };
    }
    if b.clear_data_limit == Some(true) {
        u.data_limit_bytes = None;
    }
    if let Some(m) = b.reset_mode {
        match m.as_str() {
            "monthly" | "total" | "none" => u.reset_mode = m,
            _ => return err(StatusCode::BAD_REQUEST, "reset_mode must be monthly|total|none"),
        }
    }
    if let Some(d) = b.reset_day {
        u.reset_day = d.clamp(1, 28);
    }
    if let Some(e) = b.expiry {
        let e = e.trim().to_string();
        if e.is_empty() {
            u.expiry = None;
        } else {
            if util::parse_expiry(&e).is_none() {
                return err(StatusCode::BAD_REQUEST, "expiry: invalid datetime");
            }
            u.expiry = Some(e);
        }
    }
    if let Some(cl) = b.concurrent_limit {
        u.concurrent_limit = if cl > 0 { Some(cl) } else { None };
    }
    if b.clear_concurrent == Some(true) {
        u.concurrent_limit = None;
    }
    if let Some(en) = b.enabled {
        u.enabled = en;
    }
    if let Err(e) = db::update_user(&conn, &u) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("db error: {e}"));
    }
    let view = user_view(&app, &conn, &u);
    drop(conn);
    ok_json(view)
}

pub async fn delete_user(State(app): State<Arc<App>>, Path(username): Path<String>) -> Response {
    let conn = app.db.lock().unwrap();
    if db::get_user(&conn, &username).is_none() {
        return err(StatusCode::NOT_FOUND, "user not found");
    }
    let _ = db::delete_user(&conn, &username);
    drop(conn);
    app.clear_user(&username);
    ok_json(json!({"ok": true}))
}

pub async fn reset_traffic(State(app): State<Arc<App>>, Path(username): Path<String>) -> Response {
    let conn = app.db.lock().unwrap();
    if db::get_user(&conn, &username).is_none() {
        return err(StatusCode::NOT_FOUND, "user not found");
    }
    db::clear_usage(&conn, &username);
    db::drop_counter(&conn, &username);
    drop(conn);
    ok_json(json!({"ok": true}))
}

pub async fn regen_token(State(app): State<Arc<App>>, Path(username): Path<String>) -> Response {
    let conn = app.db.lock().unwrap();
    let Some(mut u) = db::get_user(&conn, &username) else {
        return err(StatusCode::NOT_FOUND, "user not found");
    };
    u.sub_token = util::new_sub_token();
    if let Err(e) = db::update_user(&conn, &u) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("db error: {e}"));
    }
    let view = user_view(&app, &conn, &u);
    drop(conn);
    ok_json(view)
}

pub async fn kick_user(State(app): State<Arc<App>>, Path(username): Path<String>) -> Response {
    let kicked = stats::kick(&app, &[username.clone()]).await;
    if kicked {
        ok_json(json!({"ok": true}))
    } else {
        err(StatusCode::INTERNAL_SERVER_ERROR, "kick failed (traffic stats API unreachable?)")
    }
}

#[derive(Deserialize)]
pub struct UsageParams {
    days: Option<i64>,
}

#[derive(Deserialize)]
pub struct LogParams {
    limit: Option<i64>,
}

pub async fn user_usage(
    State(app): State<Arc<App>>,
    Path(username): Path<String>,
    Query(p): Query<UsageParams>,
) -> Response {
    let conn = app.db.lock().unwrap();
    if db::get_user(&conn, &username).is_none() {
        return err(StatusCode::NOT_FOUND, "user not found");
    }
    let rows = db::usage_days(&conn, &username, p.days.unwrap_or(14).clamp(1, 90));
    drop(conn);
    let out: Vec<UsageOut> = rows
        .into_iter()
        .map(|(day, tx, rx)| UsageOut { day, tx, rx })
        .collect();
    ok_json(json!({ "usage": out }))
}

pub async fn auth_log(State(app): State<Arc<App>>, Query(p): Query<LogParams>) -> Response {
    let conn = app.db.lock().unwrap();
    let rows = db::list_auth_log(&conn, p.limit.unwrap_or(150));
    drop(conn);
    let out: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "ts": r.ts,
                "username": r.username,
                "addr": r.addr,
                "ok": r.ok,
                "reason": r.reason,
            })
        })
        .collect();
    ok_json(json!({ "log": out }))
}

// ---- settings ------------------------------------------------------------------

pub async fn get_settings(State(app): State<Arc<App>>) -> Response {
    let set = app.settings();
    ok_json(json!({
        "settings": set,
        "stats": {
            "enabled": app.cfg.traffic_stats.enabled,
            "url": app.cfg.traffic_stats.url,
            "poll_secs": app.cfg.traffic_stats.poll_secs,
            "up": *app.stats_up.read().unwrap(),
        },
        "auto_kick": app.cfg.auto_kick,
        "template_path": app.cfg.template_path,
    }))
}

pub async fn put_settings(
    State(app): State<Arc<App>>,
    Json(b): Json<SettingsInput>,
) -> Response {
    let conn = app.db.lock().unwrap();
    let mut set = app.settings();
    if let Some(v) = b.base_url {
        set.base_url = v.trim().trim_end_matches('/').to_string();
    }
    if let Some(v) = b.hy2_server {
        set.hy2_server = v.trim().to_string();
    }
    if let Some(v) = b.hy2_server_port {
        if v == 0 {
            return err(StatusCode::BAD_REQUEST, "invalid port");
        }
        set.hy2_server_port = v;
    }
    if let Some(v) = b.hy2_sni {
        set.hy2_sni = v.trim().to_string();
    }
    if let Some(v) = b.hy2_insecure {
        set.hy2_insecure = v;
    }
    if let Some(v) = b.hy2_obfs_type {
        let v = v.trim().to_string();
        if !["", "salamander", "gecko"].contains(&v.as_str()) {
            return err(StatusCode::BAD_REQUEST, "obfs type must be salamander|gecko|empty");
        }
        set.hy2_obfs_type = v;
    }
    if let Some(v) = b.hy2_obfs_password {
        set.hy2_obfs_password = v.trim().to_string();
    }
    if let Some(v) = b.profile_name {
        set.profile_name = v.trim().to_string();
    }
    crate::settings::save(&conn, &set);
    drop(conn);
    *app.set.write().unwrap() = set;
    ok_json(json!({"ok": true}))
}
