use crate::config::Config;
use crate::settings::Settings;
use std::collections::HashMap;
use std::sync::{Mutex, RwLock};
use std::time::Instant;


pub struct App {
    pub db: Mutex<rusqlite::Connection>,
    pub cfg: Config,
    pub set: RwLock<Settings>,
    /// user -> devices online, last known from the traffic stats API
    pub online: RwLock<HashMap<String, u32>>,
    /// whether the last stats poll succeeded
    pub stats_up: RwLock<bool>,
    /// user -> { client ip -> last successful auth } (fallback device tracking)
    pub recent: Mutex<HashMap<String, HashMap<String, Instant>>>,
    pub http: reqwest::Client,
    /// admin session token -> expires (unix seconds)
    pub sessions: Mutex<HashMap<String, i64>>,
    /// login rate limiting: ip -> (attempts, window start)
    pub login_attempts: Mutex<HashMap<String, (u32, Instant)>>,
    /// race-guard window: how long a successful auth stays counted as a
    /// potential device (covers staleness until the next stats poll)
    pub recent_ttl: std::time::Duration,
}

impl App {
    pub fn settings(&self) -> Settings {
        self.set.read().unwrap().clone()
    }

    pub fn online_count(&self, user: &str) -> u32 {
        self.online.read().unwrap().get(user).copied().unwrap_or(0)
    }

    pub fn set_online_map(&self, map: HashMap<String, u32>) {
        *self.online.write().unwrap() = map;
    }

    pub fn record_auth_ip(&self, user: &str, ip: &str) {
        let mut m = self.recent.lock().unwrap();
        m.entry(user.to_string())
            .or_default()
            .insert(ip.to_string(), Instant::now());
    }

    /// Distinct client IPs with a successful auth inside the race-guard window.
    pub fn recent_ip_count(&self, user: &str) -> u32 {
        let mut m = self.recent.lock().unwrap();
        match m.get_mut(user) {
            Some(e) => {
                e.retain(|_, t| t.elapsed() < self.recent_ttl);
                e.len() as u32
            }
            None => 0,
        }
    }

    /// Effective device count = max(stats API, recent-IP heuristic).
    pub fn effective_devices(&self, user: &str) -> u32 {
        self.online_count(user).max(self.recent_ip_count(user))
    }

    pub fn clear_user(&self, user: &str) {
        self.online.write().unwrap().remove(user);
        self.recent.lock().unwrap().remove(user);
    }
}
