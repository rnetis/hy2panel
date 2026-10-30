use crate::admin;
use crate::auth;
use crate::state::App;
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde_json::json;
use std::sync::Arc;

pub const DEFAULT_TEMPLATE: &str = include_str!("../template.singbox.json");
const ADMIN_HTML: &str = include_str!("../assets/admin.html");

async fn guard(State(app): State<Arc<App>>, req: Request, next: Next) -> Response {
    let path = req.uri().path();
    if path.starts_with("/api") && path != "/api/login" {
        let authed = crate::util::cookie_token(req.headers())
            .map(|t| {
                let mut s = app.sessions.lock().unwrap();
                s.retain(|_, exp| *exp > crate::util::unix_now());
                s.contains_key(&t)
            })
            .unwrap_or(false);
        if !authed {
            return (StatusCode::UNAUTHORIZED, Json(json!({"error": "unauthorized"}))).into_response();
        }
    }
    next.run(req).await
}

async fn root() -> Response {
    Redirect::temporary("/admin").into_response()
}

async fn admin_page() -> Response {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        ADMIN_HTML,
    )
        .into_response()
}

pub fn router(app: Arc<App>) -> Router {
    let auth_route = if app.cfg.auth_path.starts_with('/') {
        app.cfg.auth_path.clone()
    } else {
        "/auth".to_string()
    };
    let mut router = Router::new()
        .route("/", get(root))
        .route("/admin", get(admin_page))
        .route("/auth", post(auth::hy_auth))
        .route("/api/login", post(admin::login))
        .route("/api/logout", post(admin::logout))
        .route("/api/password", post(admin::change_password))
        .route("/api/overview", get(admin::overview))
        .route("/api/users", get(admin::list_users).post(admin::create_user))
        .route("/api/users/{username}", put(admin::update_user).delete(admin::delete_user))
        .route("/api/users/{username}/reset-traffic", post(admin::reset_traffic))
        .route("/api/users/{username}/regen-token", post(admin::regen_token))
        .route("/api/users/{username}/kick", post(admin::kick_user))
        .route("/api/users/{username}/usage", get(admin::user_usage))
        .route("/api/authlog", get(admin::auth_log))
        .route("/api/settings", get(admin::get_settings).put(admin::put_settings))
        .route("/sub/{token}", get(crate::sub::sub_handler));
    if auth_route != "/auth" {
        router = router.route(&auth_route, post(auth::hy_auth));
    }
    router
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
}
