use base64::Engine;
use chrono::{Datelike, Duration, NaiveDate, Utc};
use rand::Rng;
use sha2::{Digest, Sha256};

pub fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub fn today() -> String {
    Utc::now().format("%Y-%m-%d").to_string()
}

pub fn this_month() -> String {
    Utc::now().format("%Y-%m").to_string()
}

pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// First day of the current billing cycle (UTC), given the monthly reset day (1-28).
pub fn cycle_start_day(reset_day: i64) -> String {
    let now = Utc::now().date_naive();
    let k = reset_day.clamp(1, 28) as u32;
    let this = NaiveDate::from_ymd_opt(now.year(), now.month(), k).unwrap_or(now);
    if this <= now {
        return this.format("%Y-%m-%d").to_string();
    }
    // cycle started in the previous month
    let first_this = NaiveDate::from_ymd_opt(now.year(), now.month(), 1).unwrap_or(now);
    let prev_last = first_this - Duration::days(1);
    let prev = NaiveDate::from_ymd_opt(prev_last.year(), prev_last.month(), k)
        .unwrap_or(prev_last);
    prev.format("%Y-%m-%d").to_string()
}

/// First day of the current UTC month, for display windows.
pub fn month_start_day() -> String {
    format!("{}-01", this_month())
}

const PW_CHARS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnpqrstuvwxyz23456789";

pub fn gen_password(n: usize) -> String {
    let mut rng = rand::thread_rng();
    (0..n)
        .map(|_| PW_CHARS[rng.gen_range(0..PW_CHARS.len())] as char)
        .collect()
}

pub fn rand_hex(n: usize) -> String {
    const H: &[u8] = b"0123456789abcdef";
    let mut rng = rand::thread_rng();
    (0..n).map(|_| H[rng.gen_range(0..H.len())] as char).collect()
}

pub fn new_sub_token() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

pub fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    hex::encode(h.finalize())
}

pub fn admin_hash(password: &str) -> String {
    sha256_hex(&format!("hy2panel:{password}"))
}

pub fn b64_encode(s: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(s.as_bytes())
}

/// Percent-encode everything except unreserved URI characters.
pub fn pct(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn parse_expiry(s: &str) -> Option<chrono::DateTime<Utc>> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            // accept plain dates "YYYY-MM-DD" (end of that day, UTC)
            NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .ok()
                .and_then(|d| d.and_hms_opt(23, 59, 59))
                .map(|dt| chrono::DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc))
        })
}

pub fn days_left(expiry: &Option<String>) -> Option<i64> {
    expiry
        .as_ref()
        .and_then(|e| parse_expiry(e))
        .map(|t| (t - Utc::now()).num_days().max(0))
}

pub fn expiry_unix(expiry: &Option<String>) -> i64 {
    expiry
        .as_ref()
        .and_then(|e| parse_expiry(e))
        .map(|t| t.timestamp())
        .unwrap_or(0)
}

/// Extract the hpanel_session cookie value from request headers.
pub fn cookie_token(headers: &axum::http::HeaderMap) -> Option<String> {
    let raw = headers.get(axum::http::header::COOKIE)?.to_str().ok()?;
    for part in raw.split(';') {
        let p = part.trim();
        if let Some(v) = p.strip_prefix("hpanel_session=") {
            return Some(v.to_string());
        }
    }
    None
}

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
