use crate::db;
use crate::models::User;
use crate::state::App;
use crate::util;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use qrcode::QrCode;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(Deserialize, Default)]
pub struct SubQ {
    /// "singbox" (default) | "links" | "info"
    pub target: Option<String>,
    /// force Content-Disposition: attachment
    pub dl: Option<String>,
    /// base64-wrap the links output
    pub b64: Option<String>,
}

const CLIENT_MARKERS: &[&str] = &[
    "sing-box", "singbox", "sfa/", "sfm/", "sfi/", "sfw/", "hiddify", "nekobox", "nekoray",
    "v2ray", "clash", "shadowrocket", "streisand", "karing", "curl", "wget", "python",
    "okhttp", "hysteria", "mihomo", "surge",
];

fn resolve_target(q: &SubQ, headers: &HeaderMap) -> String {
    if let Some(t) = &q.target {
        return match t.as_str() {
            "links" | "info" => t.clone(),
            _ => "singbox".to_string(),
        };
    }
    let ua = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    let is_client = ua.is_empty() || CLIENT_MARKERS.iter().any(|m| ua.contains(m));
    if is_client {
        "singbox".to_string()
    } else {
        "info".to_string()
    }
}

fn base_of(app: &App, headers: &HeaderMap) -> String {
    let set = app.settings();
    if !set.base_url.is_empty() {
        return set.base_url.trim_end_matches('/').to_string();
    }
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost");
    format!("http://{host}")
}

fn userinfo_header(u: &User, used_tx: i64, used_rx: i64, used: i64) -> String {
    let total = if u.reset_mode == "none" { 0 } else { u.data_limit_bytes.unwrap_or(0).max(0) };
    let _ = used;
    format!(
        "upload={utx}; download={urx}; total={total}; expire={exp}",
        utx = used_tx.max(0),
        urx = used_rx.max(0),
        total = total,
        exp = util::expiry_unix(&u.expiry)
    )
}

fn simple(code: StatusCode, ctype: &str, body: String) -> Response {
    Response::builder()
        .status(code)
        .header(header::CONTENT_TYPE, ctype)
        .body(Body::from(body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

pub async fn sub_handler(
    State(app): State<Arc<App>>,
    Path(token): Path<String>,
    Query(q): Query<SubQ>,
    headers: HeaderMap,
) -> Response {
    let (user, used, utx, urx) = {
        let conn = app.db.lock().unwrap();
        match db::get_by_token(&conn, &token) {
            Some(u) => {
                let used = u.used_bytes(&conn);
                let (utx, urx) = u.used_split(&conn);
                (u, used, utx, urx)
            }
            None => {
                drop(conn);
                return simple(
                    StatusCode::NOT_FOUND,
                    "text/plain; charset=utf-8",
                    "invalid subscription token".into(),
                );
            }
        }
    };

    let set = app.settings();
    let base = base_of(&app, &headers);
    let sub_url = format!("{base}/sub/{}", user.sub_token);
    let target = resolve_target(&q, &headers);

    let common_headers = |rb: axum::http::response::Builder| {
        rb.header("subscription-userinfo", userinfo_header(&user, utx, urx, used))
            .header("profile-title", format!("base64:{}", util::b64_encode(&format!("{} · {}", set.profile_name, user.username))))
            .header("profile-update-interval", "6")
            .header("profile-web-page-url", format!("{sub_url}?target=info"))
    };

    match target.as_str() {
        "links" => {
            let uri = hy2_uri(&set, &user);
            let body = if q.b64.as_deref() == Some("1") {
                util::b64_encode(&uri)
            } else {
                uri
            };
            common_headers(Response::builder().status(StatusCode::OK))
                .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
                .body(Body::from(body))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
        "info" => {
            let uri = hy2_uri(&set, &user);
            let html = info_page(&app, &user, used, utx, urx, &sub_url, &uri, &base, &headers);
            common_headers(Response::builder().status(StatusCode::OK))
                .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
                .body(Body::from(html))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
        _ => {
            let cfg = match build_singbox(&app.cfg, &set, &user) {
                Ok(v) => v,
                Err(e) => {
                    return simple(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "text/plain; charset=utf-8",
                        format!("subscription template error: {e}"),
                    )
                }
            };
            let body = match serde_json::to_string_pretty(&cfg) {
                Ok(s) => s,
                Err(e) => {
                    return simple(StatusCode::INTERNAL_SERVER_ERROR, "text/plain; charset=utf-8", format!("serialize error: {e}"))
                }
            };
            let dl = if q.dl.as_deref() == Some("1") {
                format!("attachment; filename=\"singbox-{}.json\"", user.username)
            } else {
                "inline".to_string()
            };
            common_headers(Response::builder().status(StatusCode::OK))
                .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
                .header(header::CONTENT_DISPOSITION, dl)
                .body(Body::from(body))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
    }
}

pub fn build_singbox(
    cfg: &crate::config::Config,
    set: &crate::settings::Settings,
    u: &User,
) -> Result<Value, String> {
    let raw = if cfg.template_path.is_empty() {
        crate::ui::DEFAULT_TEMPLATE.to_string()
    } else {
        std::fs::read_to_string(&cfg.template_path).unwrap_or_else(|_| crate::ui::DEFAULT_TEMPLATE.to_string())
    };
    let mut v: Value = serde_json::from_str(&raw).map_err(|e| format!("invalid JSON: {e}"))?;
    let outbounds = v
        .get_mut("outbounds")
        .and_then(Value::as_array_mut)
        .ok_or("template: missing outbounds array")?;
    let out = outbounds
        .iter_mut()
        .find(|o| o.get("type").and_then(Value::as_str) == Some("hysteria2"))
        .ok_or("template: no hysteria2 outbound")?;
    let obj = out.as_object_mut().ok_or("template: outbound is not an object")?;

    obj.insert("server".into(), json!(set.hy2_server));
    obj.insert("server_port".into(), json!(set.hy2_server_port));
    obj.insert("password".into(), json!(u.password));

    if !obj.contains_key("tls") {
        obj.insert("tls".into(), json!({}));
    }
    if !obj.get("tls").map(Value::is_object).unwrap_or(false) {
        obj.insert("tls".into(), json!({}));
    }
    let tls = obj.get_mut("tls").unwrap();
    {
        let t = tls.as_object_mut().unwrap();
        t.insert("enabled".into(), json!(true));
        if set.hy2_sni.is_empty() {
            t.remove("server_name");
        } else {
            t.insert("server_name".into(), json!(set.hy2_sni));
        }
        t.insert("insecure".into(), json!(set.hy2_insecure));
    }

    if set.hy2_obfs_password.is_empty() {
        obj.remove("obfs");
    } else {
        let t = if set.hy2_obfs_type.is_empty() { "salamander" } else { set.hy2_obfs_type.as_str() };
        obj.insert("obfs".into(), json!({ "type": t, "password": set.hy2_obfs_password }));
    }
    Ok(v)
}

pub fn hy2_uri(set: &crate::settings::Settings, u: &User) -> String {
    let mut q: Vec<String> = vec![];
    if !set.hy2_sni.is_empty() {
        q.push(format!("sni={}", util::pct(&set.hy2_sni)));
    }
    if set.hy2_insecure {
        q.push("insecure=1".into());
    }
    if !set.hy2_obfs_password.is_empty() {
        let t = if set.hy2_obfs_type.is_empty() { "salamander" } else { set.hy2_obfs_type.as_str() };
        q.push(format!("obfs={}", util::pct(t)));
        q.push(format!("obfs-password={}", util::pct(&set.hy2_obfs_password)));
    }
    let name = format!("{} · {}", set.profile_name, u.username);
    format!(
        "hysteria2://{}@{}:{}/?{}#{}",
        util::pct(&u.password),
        set.hy2_server,
        set.hy2_server_port,
        q.join("&"),
        util::pct(&name)
    )
}

fn qr_svg(data: &str) -> String {
    match QrCode::new(data.as_bytes()) {
        Ok(code) => code
            .render::<qrcode::render::svg::Color>()
            .quiet_zone(true)
            .min_dimensions(220, 220)
            .build(),
        Err(_) => String::new(),
    }
}

fn fmt_bytes(n: i64) -> String {
    let sign = if n < 0 { "-" } else { "" };
    let mut v = n.unsigned_abs() as f64;
    for unit in ["B", "KB", "MB", "GB", "TB", "PB"] {
        if v < 1024.0 {
            return format!("{sign}{v:.2} {unit}");
        }
        v /= 1024.0;
    }
    format!("{sign}{v:.2} EB")
}

#[allow(clippy::too_many_arguments)]
fn info_page(
    app: &Arc<App>,
    u: &User,
    used: i64,
    utx: i64,
    urx: i64,
    sub_url: &str,
    uri: &str,
    base: &str,
    headers: &HeaderMap,
) -> String {
    let set = app.settings();
    let name = format!("{} · {}", set.profile_name, u.username);
    let status = u.status(used);
    let (pill, pill_cls) = match status {
        "active" => ("Active", "ok"),
        "capped" => ("Data cap reached", "warn"),
        "expired" => ("Expired", "bad"),
        _ => ("Disabled", "off"),
    };

    let limit = if u.reset_mode == "none" || u.data_limit_bytes.is_none() {
        None
    } else {
        u.data_limit_bytes
    };
    let pct = limit.map(|l| ((used as f64 / l.max(1) as f64) * 100.0).min(100.0));
    let usage_line = match limit {
        Some(l) => format!("{} <span class='dim'>/ {}</span>", fmt_bytes(used), fmt_bytes(l)),
        None => format!("{} <span class='dim'>· unlimited</span>", fmt_bytes(used)),
    };
    let bar_html = pct
        .map(|p| format!("<div class='bar'><div class='fill {}' style='width:{p:.1}%'></div></div>", if p > 90.0 { "hot" } else { "" }))
        .unwrap_or_default();

    let cycle = match u.reset_mode.as_str() {
        "monthly" => format!("resets on day {} of each month (UTC)", u.reset_day),
        "total" => "one-time quota — resets only by the admin".to_string(),
        _ => "no data quota".to_string(),
    };

    let expiry_line = match &u.expiry {
        Some(e) => {
            let d = util::parse_expiry(e).map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string()).unwrap_or_else(|| e.clone());
            match util::days_left(&u.expiry) {
                Some(days) => format!("{d} <span class='dim'>({days} day{} left)</span>", if days == 1 { "" } else { "s" }),
                None => d,
            }
        }
        None => "<span class='dim'>never expires</span>".to_string(),
    };

    let devices = app.online_count(&u.username);
    let dev_line = match u.concurrent_limit {
        Some(l) => format!("{devices} <span class='dim'>/ {l} devices</span>"),
        None => format!("{devices} <span class='dim'>online</span>"),
    };

    let note = if u.note.is_empty() {
        String::new()
    } else {
        format!("<p class='note'>{}</p>", util::esc(&u.note))
    };

    let qr = qr_svg(sub_url);
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
    let escaped = util::esc;

    PAGE
        .replace("%%TITLE%%", &escaped(&util::esc(&name)))
        .replace("%%PILL%%", pill)
        .replace("%%PILLCLS%%", pill_cls)
        .replace("%%USAGE%%", &usage_line)
        .replace("%%BAR%%", &bar_html)
        .replace("%%CYCLE%%", &escaped(&cycle))
        .replace("%%UPTX%%", &fmt_bytes(utx))
        .replace("%%URX%%", &fmt_bytes(urx))
        .replace("%%EXPIRY%%", &expiry_line)
        .replace("%%DEVICES%%", &dev_line)
        .replace("%%QR%%", &qr)
        .replace("%%SUBURL%%", &escaped(sub_url))
        .replace("%%URI%%", &escaped(uri))
        .replace("%%CONFIGURL%%", &escaped(&format!("{base}/sub/{}?target=singbox&dl=1", u.sub_token)))
        .replace("%%NOTE%%", &note)
        .replace("%%HOST%%", &escaped(host))
        .replace("%%USER%%", &escaped(&u.username))
}

const PAGE: &str = r#"<!doctype html>
<html lang="en"><head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>%%TITLE%% — subscription</title>
<style>
:root{color-scheme:dark}
*{box-sizing:border-box}
body{margin:0;min-height:100vh;display:flex;align-items:center;justify-content:center;
 background:#0b0f17;color:#e6edf3;font:15px/1.55 system-ui,-apple-system,"Segoe UI",Roboto,sans-serif;padding:24px}
.card{width:100%;max-width:460px;background:#131a26;border:1px solid #1f2a3a;border-radius:16px;padding:28px}
h1{font-size:20px;margin:0 0 4px}
.pill{display:inline-block;font-size:12px;font-weight:600;padding:3px 10px;border-radius:999px;margin-bottom:14px}
.ok{background:#0e2a1c;color:#4ade80}.warn{background:#2a2008;color:#fbbf24}
.bad{background:#2a0e12;color:#f87171}.off{background:#1a2230;color:#94a3b8}
.dim{color:#7d8aa0;font-size:13px}
.row{margin:12px 0}
.lbl{font-size:12px;text-transform:uppercase;letter-spacing:.06em;color:#7d8aa0;margin-bottom:3px}
.bar{height:8px;background:#0d1420;border-radius:6px;overflow:hidden;margin-top:6px}
.fill{height:100%;background:#4f8cff;border-radius:6px}
.fill.hot{background:#f87171}
.grid{display:flex;gap:18px;font-size:13px;color:#94a3b8}
.grid b{color:#e6edf3;font-weight:600}
.qr{background:#fff;border-radius:12px;padding:12px;width:max-content;margin:16px auto}
.links{display:flex;flex-direction:column;gap:8px;margin-top:16px}
a.btn,button.btn{display:block;text-align:center;padding:10px 14px;border-radius:10px;border:1px solid #2b3b55;
 background:#0d1420;color:#e6edf3;text-decoration:none;font-size:14px;cursor:pointer;width:100%}
a.btn:hover,button.btn:hover{background:#16223a}
.url{font-family:ui-monospace,monospace;font-size:11px;word-break:break-all;color:#7d8aa0;background:#0d1420;
 border-radius:8px;padding:8px 10px;margin-top:10px}
.note{font-size:13px;color:#94a3b8;background:#0d1420;border-radius:8px;padding:10px}
footer{margin-top:18px;text-align:center;font-size:11px;color:#4b5769}
@media(max-width:480px){.card{padding:20px}}
</style></head><body>
<div class="card">
  <h1>%%TITLE%%</h1>
  <span class="pill %%PILLCLS%%">%%PILL%%</span>
  <div class="row">
    <div class="lbl">Data usage — %%CYCLE%%</div>
    <div>%%USAGE%%</div>
    %%BAR%%
    <div class="grid" style="margin-top:8px"><span>↑ <b>%%UPTX%%</b></span><span>↓ <b>%%URX%%</b></span><span>🌐 <b>%%DEVICES%%</b></span></div>
  </div>
  <div class="row"><div class="lbl">Expires</div><div>%%EXPIRY%%</div></div>
  <div class="qr">%%QR%%</div>
  <div class="links">
    <button class="btn" onclick="cp('%%SUBURL_ESC%%')">📋 Copy sing-box subscription URL</button>
    <a class="btn" href="%%CONFIGURL%%">⬇️ Download sing-box config (JSON)</a>
    <button class="btn" onclick="cp('%%URI_ESC%%')">🔗 Copy hysteria2 share link</button>
  </div>
  <div class="url">%%SUBURL%%</div>
  %%NOTE%%
  <footer>hy2panel — sing-box → Profiles → New → URL: paste the link above</footer>
</div>
<script>
function cp(t){navigator.clipboard.writeText(t).then(function(){var b=event.target;var o=b.textContent;b.textContent='✅ Copied';setTimeout(function(){b.textContent=o},1200)})}
</script>
</body></html>
"#;
