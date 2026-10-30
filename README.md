# hy2panel — Hysteria2 user management panel

A single-binary Rust panel that turns a **Hysteria2** server with static passwords into a
multi-user service with:

- **Per-user accounts** — each user gets their own password (no more shared secrets)
- **Concurrent device limits** — "max 2 devices per user", enforced at connect time
- **Date expiry** — subscriptions that die at a chosen date/time
- **Data usage expiry (quotas)** — monthly-cycle, one-time or unlimited traffic caps
- **Auto-kick** — users that hit a cap / expiry while online are disconnected immediately
- **Per-user sing-box subscription pages** — a URL per user that returns a ready-made
  `sing-box` config (with QR page, `hysteria2://` share links, usage display)
- **Admin web UI** — dashboard, live usage, auth log, settings; no external dependencies

Built with `axum` + `SQLite` (embedded), talks to Hysteria2's official
[HTTP auth](https://v2.hysteria.network/docs/advanced/Full-Server-Config/) and
[Traffic Stats API](https://v2.hysteria.network/docs/advanced/Traffic-Stats-API/).

```
                         ┌──────────────────────────────────────────┐
 sing-box client ──QUIC──▶ Hysteria2 server :443                     │
   (per-user pw)         │   auth ──POST /auth──▶ ┌───────────────┐  │
                         │                        │   hy2panel    │  │
                         │   trafficStats :9999 ◀─│  (this panel) │  │
                         │     GET /traffic       │  SQLite       │  │
                         │     GET /online        │  admin UI     │  │
                         │     POST /kick         │  /sub/<token> │  │
                         └────────────────────────┴───────┬───────┘  │
                                                          │          │
                                        subscription URL ─┴──▶ user's
                                        sing-box client imports it
```

---

## 1. Quick start

### Build

```bash
cargo build --release
# binary: target/release/hy2panel  (a prebuilt linux-x86_64 binary ships in the zip)
```

### Configure

```bash
cp config.example.toml config.toml
nano config.toml        # set admin_password, base_url, and your [hysteria2] details
```

The `[hysteria2]` section describes **your existing hysteria2 server** (address, port,
SNI, obfs password) — it is only used to generate subscription configs for users.

### Run

```bash
./hy2panel                       # reads ./config.toml
./hy2panel /path/to/config.toml  # or pass the path
```

Open `http://<panel>:8080/admin`, sign in with `admin_password`.

---

## 2. Switch your Hysteria2 server to the panel

Replace the static `auth: password:` in your server config with the panel's HTTP auth
and enable the Traffic Stats API (this is what powers usage counting, device limits
and kicking). Obfuscation and TLS stay exactly as they are:

```yaml
listen: :443

tls:
  cert: /etc/hysteria/fullchain.pem
  key: /etc/hysteria/privkey.pem

obfs:
  type: salamander
  salamander:
    password: rayan            # unchanged

# was: auth: { type: password, password: rayanrayan }
auth:
  type: http
  url: http://127.0.0.1:8080/auth   # panel listen address + auth_path
  insecure: true                    # plain http on localhost is fine

trafficStats:
  listen: 127.0.0.1:9999            # panel-only access; keep it local
  secret: Se7en                     # = [traffic_stats].secret in config.toml
```

A complete example lives in [`deploy/hysteria-server.example.yaml`](deploy/hysteria-server.example.yaml).
Restart hysteria2 — old static passwords stop working, panel users start working.

> **Migration note:** your current client password (`rayanrayan`) won't authenticate
> anymore. Create a user in the panel and import its subscription URL instead.

---

## 3. Using the panel

### Users

- **+ New user** — set username, optional password (generated if empty), data limit (GB),
  reset mode, device limit, expiry date/time, note.
- After creation you get the **password** and the **subscription URL** to hand to the user.

### Limits — how each one works

| Control | Field | Enforced |
|---|---|---|
| **Data usage** | `data_limit_gb` + `reset_mode` | Auth is denied once used ≥ limit. Hysteria2 counters are polled every `poll_secs` and stored as daily deltas in SQLite, so usage survives server restarts. `monthly` = cycle starts on `reset_day` (1-28) each month (UTC) · `total` = one-time bucket until you press *Reset* · `none` = unlimited |
| **Date expiry** | `expiry` | Auth denied after the timestamp. `days_left` is shown everywhere |
| **Concurrent devices** | `concurrent_limit` | Compared against hysteria2's `GET /online` (real device count per user). A short in-memory "race window" covers the gap between two polls so two devices connecting at the same second can't slip past a limit of 1 |
| **Enabled flag** | `enabled` | Instantly blocks new auths |

### Auto-kick

Denying *new* auths stops reconnects, but an established QUIC connection lives on
(hysteria2 has no server-side session limit). With `auto_kick = true` the poller calls
hysteria2's `POST /kick` for any online user that is expired / capped / disabled, so
enforcement is immediate. There's also a manual **Kick** button per user.

### Subscriptions

Each user gets a token-protected URL:

| URL | Returns |
|---|---|
| `GET /sub/<token>` | sing-box JSON config (when fetched by a client/curl) — a styled **info page with QR code** when opened in a browser |
| `GET /sub/<token>?target=singbox` | force the sing-box config |
| `GET /sub/<token>?target=info` | force the info page |
| `GET /sub/<token>?target=links` | `hysteria2://` share link (add `&b64=1` for base64) |
| `GET /sub/<token>?target=singbox&dl=1` | config as a file download |

Client responses carry the standard headers sing-box clients understand:

```
subscription-userinfo: upload=…; download=…; total=…; expire=…
profile-title: base64:…
profile-update-interval: 6
profile-web-page-url: …
```

In sing-box (SFA/SFM/SFI/SFW, Hiddify, Karing…): **Profiles → New → URL** → paste the
link. The generated config mirrors `template.singbox.json` — the same structure as the
reference config it was built from (TUN inbound, AdGuard DoH via the tunnel, Iran
direct-routing rule-sets, `bbr_profile`) — with **only the credentials swapped in**
(server, port, password, obfs, TLS).

To customise what users receive, edit `template.singbox.json` (or point
`template_path` at your own); the panel finds the `hysteria2` outbound and fills in
per-user values at request time. **Re-token** rotates the URL (old one dies instantly).

### Auth log

The **Auth log** view shows the last connection attempts per user/IP with the deny
reason (`data cap reached`, `device limit reached (3/2 devices)`, …).

---

## 4. Configuration reference (`config.toml`)

| Key | Default | Meaning |
|---|---|---|
| `listen` | `127.0.0.1:8080` | Panel bind address. `0.0.0.0:8080` exposes subs/admin publicly |
| `base_url` | *(auto from Host header)* | Public URL used in subscription links |
| `auth_path` | `/auth` | Path hysteria2 posts auth requests to |
| `admin_password` | — | Password on first login; can be changed in the UI (then stored hashed) |
| `db_path` | `panel.db` | SQLite database file |
| `template_path` | `template.singbox.json` | sing-box config template (embedded fallback) |
| `auto_kick` | `true` | Kick online users when they become blocked |
| `profile_name` | `HY2` | Profile / page display name |
| `[hysteria2]` | — | `server`, `server_port`, `sni`, `insecure`, `obfs_type` (`salamander`/`gecko`/empty), `obfs_password` — used for generated subs; editable in the UI |
| `[traffic_stats]` | — | `enabled`, `url`, `secret`, `poll_secs` — must match hysteria2's `trafficStats:` block |

Settings changed in the UI are stored in the DB and override `config.toml` defaults;
`traffic_stats` and `listen` always come from the file.

## 5. Admin API

All endpoints are JSON; everything except `/api/login` requires the session cookie
from `POST /api/login {"password": …}`.

| Method & path | Purpose |
|---|---|
| `POST /api/login`, `POST /api/logout` | Session management (10 req/min limit) |
| `POST /api/password` | Change admin password `{old, new}` |
| `GET /api/overview` | Totals: users, active, devices online, today/month traffic, stats health |
| `GET /api/users?q=` | List users with usage, status, sub URLs |
| `POST /api/users` | Create: `{username, password?, data_limit_gb?, reset_mode?, reset_day?, expiry?, concurrent_limit?, note?, enabled?}` |
| `PUT /api/users/{name}` | Update (missing fields keep current; `expiry: ""` clears) |
| `DELETE /api/users/{name}` | Delete user + usage |
| `POST /api/users/{name}/reset-traffic` | Zero the usage bucket |
| `POST /api/users/{name}/regen-token` | Rotate subscription URL |
| `POST /api/users/{name}/kick` | Disconnect the user's clients now |
| `GET /api/users/{name}/usage?days=14` | Daily usage series |
| `GET /api/authlog?limit=150` | Recent auth attempts |
| `GET/PUT /api/settings` | Connection details used for subscriptions |

The auth endpoint itself (`POST /auth`) speaks hysteria2's protocol:
`{addr, auth, tx}` in → `{ok: true, id: "<username>"}` (HTTP 200) out, always 200,
`ok:false` + human-readable `msg` on denial.

## 6. Production notes

- **TLS**: put the panel behind nginx/Caddy (or an SSH tunnel) and set `base_url`
  accordingly; the hysteria2 → panel auth hop stays on localhost.
- **Bind sanity**: `listen = "127.0.0.1:8080"` + reverse proxy is the safe default.
  If you bind publicly, the admin UI is still password-protected, but HTTPS is on you.
- **systemd**: `deploy/hysteria2-panel.service` (expects the binary + config in
  `/opt/hy2panel`).
- **Clock**: quota cycles and expiry use UTC.
- **Backups**: everything is `panel.db` — one SQLite file.
- The auth endpoint matches users **by password**, so keep them reasonably random
  (generated ones are 10 chars, URL-safe).

### Known limitations

- A device limit check can be off by one for a few seconds around simultaneous
  connects; hysteria2's live `GET /online` corrects it on the next poll.
- Denying auth stops reconnects; established connections are ended via `/kick`
  (requires `trafficStats` enabled — disable `auto_kick` if you don't want that).
- Traffic deltas are computed from hysteria2's cumulative counters; deleting a user
  and re-creating the same name starts a fresh usage history.

## 7. Development / testing

`scripts/mock_stats.py` emulates the Traffic Stats API (bulk + legacy endpoints and
`/kick`) and `scripts/smoke_test.sh` runs a 39-assertion end-to-end suite against the
panel (auth, caps, expiry, device limits, kick, subscriptions, settings):

```bash
cargo build --release
bash scripts/smoke_test.sh
```
