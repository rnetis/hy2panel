use crate::models::User;
use crate::util;
use rusqlite::{params, Connection, OptionalExtension};

pub fn open(path: &str) -> anyhow::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(conn)
}

pub fn migrate(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS users(
            username TEXT PRIMARY KEY,
            password TEXT UNIQUE NOT NULL,
            sub_token TEXT UNIQUE NOT NULL,
            data_limit_bytes INTEGER,
            reset_mode TEXT NOT NULL DEFAULT 'monthly',
            reset_day INTEGER NOT NULL DEFAULT 1,
            expiry TEXT,
            concurrent_limit INTEGER,
            enabled INTEGER NOT NULL DEFAULT 1,
            note TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS counters(
            username TEXT PRIMARY KEY,
            tx INTEGER NOT NULL DEFAULT 0,
            rx INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS usage_daily(
            username TEXT NOT NULL,
            day TEXT NOT NULL,
            tx INTEGER NOT NULL DEFAULT 0,
            rx INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY(username, day)
        );
        CREATE TABLE IF NOT EXISTS auth_log(
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ts TEXT NOT NULL,
            username TEXT,
            addr TEXT NOT NULL,
            ok INTEGER NOT NULL,
            reason TEXT NOT NULL DEFAULT ''
        );
        CREATE TABLE IF NOT EXISTS settings(
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_usage_day ON usage_daily(day);
        "#,
    )?;
    Ok(())
}

const USER_COLS: &str = "username,password,sub_token,data_limit_bytes,reset_mode,reset_day,expiry,concurrent_limit,enabled,note,created_at";

fn row_user(r: &rusqlite::Row) -> rusqlite::Result<User> {
    Ok(User {
        username: r.get(0)?,
        password: r.get(1)?,
        sub_token: r.get(2)?,
        data_limit_bytes: r.get(3)?,
        reset_mode: r.get(4)?,
        reset_day: r.get(5)?,
        expiry: r.get(6)?,
        concurrent_limit: r.get(7)?,
        enabled: r.get::<_, i64>(8)? != 0,
        note: r.get(9)?,
        created_at: r.get(10)?,
    })
}

fn query_user(conn: &Connection, sql: &str, p: &[&dyn rusqlite::ToSql]) -> Option<User> {
    conn.query_row(sql, p, row_user).optional().ok().flatten()
}

pub fn list_users(conn: &Connection) -> Vec<User> {
    let sql = format!("SELECT {USER_COLS} FROM users ORDER BY username");
    let Ok(mut st) = conn.prepare(&sql) else {
        return vec![];
    };
    let rows = st.query_map([], row_user);
    match rows {
        Ok(it) => it.filter_map(|r| r.ok()).collect(),
        Err(_) => vec![],
    }
}

pub fn get_user(conn: &Connection, username: &str) -> Option<User> {
    query_user(
        conn,
        &format!("SELECT {USER_COLS} FROM users WHERE username = ?1"),
        &[&username],
    )
}

pub fn get_by_password(conn: &Connection, pw: &str) -> Option<User> {
    query_user(
        conn,
        &format!("SELECT {USER_COLS} FROM users WHERE password = ?1"),
        &[&pw],
    )
}

pub fn get_by_token(conn: &Connection, tok: &str) -> Option<User> {
    query_user(
        conn,
        &format!("SELECT {USER_COLS} FROM users WHERE sub_token = ?1"),
        &[&tok],
    )
}

pub fn password_taken(conn: &Connection, pw: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM users WHERE password = ?1 LIMIT 1",
        params![pw],
        |_| Ok(()),
    )
    .optional()
    .ok()
    .flatten()
    .is_some()
}

pub fn insert_user(conn: &Connection, u: &User) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO users(username,password,sub_token,data_limit_bytes,reset_mode,reset_day,expiry,concurrent_limit,enabled,note,created_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            u.username,
            u.password,
            u.sub_token,
            u.data_limit_bytes,
            u.reset_mode,
            u.reset_day,
            u.expiry,
            u.concurrent_limit,
            u.enabled as i64,
            u.note,
            u.created_at
        ],
    )?;
    Ok(())
}

pub fn update_user(conn: &Connection, u: &User) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE users SET password=?2, sub_token=?3, data_limit_bytes=?4, reset_mode=?5, reset_day=?6, expiry=?7, concurrent_limit=?8, enabled=?9, note=?10 WHERE username=?1",
        params![
            u.username,
            u.password,
            u.sub_token,
            u.data_limit_bytes,
            u.reset_mode,
            u.reset_day,
            u.expiry,
            u.concurrent_limit,
            u.enabled as i64,
            u.note
        ],
    )?;
    Ok(())
}

pub fn delete_user(conn: &Connection, username: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM users WHERE username = ?1", params![username])?;
    conn.execute("DELETE FROM counters WHERE username = ?1", params![username])?;
    conn.execute("DELETE FROM usage_daily WHERE username = ?1", params![username])?;
    Ok(())
}

// ---- traffic counters / usage ---------------------------------------------

pub fn get_counter(conn: &Connection, username: &str) -> Option<(i64, i64)> {
    conn.query_row(
        "SELECT tx, rx FROM counters WHERE username = ?1",
        params![username],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
    )
    .optional()
    .ok()
    .flatten()
}

pub fn set_counter(conn: &Connection, username: &str, tx: i64, rx: i64) {
    let _ = conn.execute(
        "INSERT INTO counters(username, tx, rx) VALUES(?1,?2,?3)
         ON CONFLICT(username) DO UPDATE SET tx=excluded.tx, rx=excluded.rx",
        params![username, tx, rx],
    );
}

pub fn drop_counter(conn: &Connection, username: &str) {
    let _ = conn.execute("DELETE FROM counters WHERE username = ?1", params![username]);
}

pub fn add_usage(conn: &Connection, username: &str, day: &str, dtx: i64, drx: i64) {
    if dtx == 0 && drx == 0 {
        return;
    }
    let _ = conn.execute(
        "INSERT INTO usage_daily(username, day, tx, rx) VALUES(?1,?2,?3,?4)
         ON CONFLICT(username, day) DO UPDATE SET tx = tx + excluded.tx, rx = rx + excluded.rx",
        params![username, day, dtx, drx],
    );
}

/// (tx, rx) totals since `from_day` (inclusive), or all-time when None.
pub fn usage_split(conn: &Connection, username: &str, from_day: Option<&str>) -> (i64, i64) {
    let res = match from_day {
        Some(d) => conn.query_row(
            "SELECT COALESCE(SUM(tx),0), COALESCE(SUM(rx),0) FROM usage_daily WHERE username=?1 AND day>=?2",
            params![username, d],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
        ),
        None => conn.query_row(
            "SELECT COALESCE(SUM(tx),0), COALESCE(SUM(rx),0) FROM usage_daily WHERE username=?1",
            params![username],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
        ),
    };
    res.unwrap_or((0, 0))
}

pub fn usage_since(conn: &Connection, username: &str, from_day: Option<&str>) -> i64 {
    let (tx, rx) = usage_split(conn, username, from_day);
    tx + rx
}

pub fn usage_days(conn: &Connection, username: &str, days: i64) -> Vec<(String, i64, i64)> {
    let mut st = match conn.prepare(
        "SELECT day, tx, rx FROM usage_daily WHERE username=?1 ORDER BY day DESC LIMIT ?2",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    let res = st.query_map(params![username, days], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))
    });
    match res {
        Ok(it) => it.filter_map(|r| r.ok()).collect(),
        Err(_) => vec![],
    }
}

/// (tx, rx) across all users since `from_day` (inclusive).
pub fn totals_since_day(conn: &Connection, from_day: &str) -> (i64, i64) {
    conn.query_row(
        "SELECT COALESCE(SUM(tx),0), COALESCE(SUM(rx),0) FROM usage_daily WHERE day>=?1",
        params![from_day],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
    )
    .unwrap_or((0, 0))
}

pub fn clear_usage(conn: &Connection, username: &str) {
    let _ = conn.execute("DELETE FROM usage_daily WHERE username = ?1", params![username]);
}

// ---- auth log ----------------------------------------------------------------

pub fn log_auth(conn: &Connection, username: Option<&str>, addr: &str, ok: bool, reason: &str) {
    let _ = conn.execute(
        "INSERT INTO auth_log(ts, username, addr, ok, reason) VALUES(?1,?2,?3,?4,?5)",
        params![util::now_iso(), username, addr, ok as i64, reason],
    );
    // keep the table bounded
    let _ = conn.execute(
        "DELETE FROM auth_log WHERE id <= (SELECT COALESCE(MAX(id),0) FROM auth_log) - 5000",
        [],
    );
}

pub struct AuthRow {
    pub ts: String,
    pub username: Option<String>,
    pub addr: String,
    pub ok: bool,
    pub reason: String,
}

pub fn list_auth_log(conn: &Connection, limit: i64) -> Vec<AuthRow> {
    let mut st = match conn.prepare(
        "SELECT ts, username, addr, ok, reason FROM auth_log ORDER BY id DESC LIMIT ?1",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    let res = st.query_map(params![limit.clamp(1, 1000)], |r| {
        Ok(AuthRow {
            ts: r.get(0)?,
            username: r.get(1)?,
            addr: r.get(2)?,
            ok: r.get::<_, i64>(3)? != 0,
            reason: r.get(4)?,
        })
    });
    match res {
        Ok(it) => it.filter_map(|r| r.ok()).collect(),
        Err(_) => vec![],
    }
}

// ---- key/value settings -------------------------------------------------------

pub fn get_setting(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        params![key],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .ok()
    .flatten()
}

pub fn set_setting(conn: &Connection, key: &str, value: &str) {
    let _ = conn.execute(
        "INSERT INTO settings(key, value) VALUES(?1,?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    );
}
