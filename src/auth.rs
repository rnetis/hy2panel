use crate::state::App;
use crate::db;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Request body hysteria2 sends on every client connection attempt.
#[derive(Deserialize)]
pub struct HyAuthReq {
    #[serde(default)]
    pub addr: String,
    #[serde(default)]
    pub auth: String,
    /// part of the hysteria2 protocol payload; kept for completeness
    #[serde(default)]
    #[allow(dead_code)]
    pub tx: i64,
}

/// Response body. `msg` is a common extension; unknown fields are ignored
/// by the hysteria server, and ok=false (or non-200) denies the client.
#[derive(Serialize)]
pub struct HyAuthResp {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub msg: Option<String>,
}

fn deny(msg: &str) -> HyAuthResp {
    HyAuthResp { ok: false, id: None, msg: Some(msg.to_string()) }
}

/// POST {auth_path} — the hysteria2 HTTP authentication backend.
/// Always responds 200; auth success/failure is expressed via `ok`.
/// The body is parsed from raw bytes so the endpoint is tolerant of
/// any Content-Type the hysteria server (or a proxy) sends.
pub async fn hy_auth(State(app): State<Arc<App>>, body: axum::body::Bytes) -> Response {
    let req: HyAuthReq = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(_) => {
            let conn = app.db.lock().unwrap();
            db::log_auth(&conn, None, "", false, "malformed auth request");
            return (StatusCode::OK, Json(deny("malformed request"))).into_response();
        }
    };
    let ip = req
        .addr
        .rsplit_once(':')
        .map(|(h, _)| h.trim_matches(['[', ']']).to_string())
        .unwrap_or_else(|| req.addr.clone());
    let addr_full = req.addr.clone();

    let (resp, username, reason) = evaluate(&app, &req.auth, &ip);

    {
        let conn = app.db.lock().unwrap();
        db::log_auth(&conn, username.as_deref(), &addr_full, resp.ok, &reason);
    }
    (StatusCode::OK, Json(resp)).into_response()
}

fn evaluate(app: &Arc<App>, auth: &str, ip: &str) -> (HyAuthResp, Option<String>, String) {
    let conn = app.db.lock().unwrap();

    let Some(user) = db::get_by_password(&conn, auth) else {
        return (deny("invalid credentials"), None, "unknown password".into());
    };

    let used = user.used_bytes(&conn);
    if let Some(reason) = user.blocked_reason(used) {
        return (deny(&reason), Some(user.username.clone()), reason);
    }

    // Concurrent device limit.
    if let Some(limit) = user.concurrent_limit.filter(|l| *l > 0) {
        let eff = app.effective_devices(&user.username);
        if eff >= limit as u32 {
            let msg = format!("device limit reached ({eff}/{limit} devices)");
            return (deny(&msg), Some(user.username.clone()), msg);
        }
    }

    drop(conn);
    app.record_auth_ip(&user.username, ip);
    (
        HyAuthResp { ok: true, id: Some(user.username.clone()), msg: None },
        Some(user.username.clone()),
        "ok".into(),
    )
}
