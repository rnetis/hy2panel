use crate::db;
use crate::util;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub struct User {
    pub username: String,
    pub password: String,
    pub sub_token: String,
    pub data_limit_bytes: Option<i64>,
    pub reset_mode: String, // "monthly" | "total" | "none"
    pub reset_day: i64,
    pub expiry: Option<String>, // RFC3339 UTC or plain YYYY-MM-DD
    pub concurrent_limit: Option<i64>,
    pub enabled: bool,
    pub note: String,
    pub created_at: String,
}

impl User {
    pub fn expired(&self) -> bool {
        match &self.expiry {
            Some(e) => match util::parse_expiry(e) {
                Some(t) => util::unix_now() > t.timestamp(),
                None => false,
            },
            None => false,
        }
    }

    pub fn capped(&self, used: i64) -> bool {
        if self.reset_mode == "none" {
            return false;
        }
        match self.data_limit_bytes {
            Some(limit) if limit > 0 => used >= limit,
            _ => false,
        }
    }

    /// "active" | "disabled" | "expired" | "capped"
    pub fn status(&self, used: i64) -> &'static str {
        if !self.enabled {
            return "disabled";
        }
        if self.expired() {
            return "expired";
        }
        if self.capped(used) {
            return "capped";
        }
        "active"
    }

    /// Reason to deny a new hysteria2 auth, or None if allowed.
    pub fn blocked_reason(&self, used: i64) -> Option<String> {
        match self.status(used) {
            "disabled" => Some("user disabled".into()),
            "expired" => Some("subscription expired".into()),
            "capped" => Some("data cap reached".into()),
            _ => None,
        }
    }

    /// Bytes used in the window relevant for this user's reset mode.
    pub fn used_bytes(&self, conn: &rusqlite::Connection) -> i64 {
        match self.reset_mode.as_str() {
            "total" => db::usage_since(conn, &self.username, None),
            "none" => db::usage_since(conn, &self.username, Some(&util::month_start_day())),
            _ => db::usage_since(
                conn,
                &self.username,
                Some(&util::cycle_start_day(self.reset_day)),
            ),
        }
    }

    /// (tx, rx) bytes in the same window, for the subscription header.
    pub fn used_split(&self, conn: &rusqlite::Connection) -> (i64, i64) {
        match self.reset_mode.as_str() {
            "total" => db::usage_split(conn, &self.username, None),
            "none" => db::usage_split(conn, &self.username, Some(&util::month_start_day())),
            _ => db::usage_split(
                conn,
                &self.username,
                Some(&util::cycle_start_day(self.reset_day)),
            ),
        }
    }
}

#[derive(Serialize)]
pub struct UsageOut {
    pub day: String,
    pub tx: i64,
    pub rx: i64,
}

/// Body for POST /api/users and PUT /api/users/{username}.
/// Missing fields keep their current value on update.
#[derive(Deserialize, Default)]
pub struct UserInput {
    pub username: Option<String>,
    pub password: Option<String>,
    pub note: Option<String>,
    pub data_limit_gb: Option<f64>,
    pub clear_data_limit: Option<bool>,
    pub reset_mode: Option<String>,
    pub reset_day: Option<i64>,
    pub expiry: Option<String>,
    pub concurrent_limit: Option<i64>,
    pub clear_concurrent: Option<bool>,
    pub enabled: Option<bool>,
}

pub fn valid_username(u: &str) -> bool {
    !u.is_empty()
        && u.len() <= 32
        && u.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

pub fn valid_password(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 128
        && p.chars().all(|c| !c.is_whitespace())
}
