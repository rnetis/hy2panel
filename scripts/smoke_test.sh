#!/bin/bash
# Smoke test for hy2panel: exercises auth, quota, expiry, concurrency, subs.
set -u
cd /home/z/my-project/hysteria2-panel
PANEL=127.0.0.1:8091
PASS=0; FAIL=0

set_state() { # atomic write so the mock never reads a torn file
  python3 -c "
import json, os, sys
p = '/tmp/mock_stats_state.json'
tmp = p + '.tmp'
with open(tmp, 'w') as f: json.dump(json.loads(sys.argv[1]), f)
os.replace(tmp, p)" "$1"
}

start() {
  MOCK_PORT=8092 nohup python3 /home/z/my-project/scripts/mock_stats.py >/tmp/mock.log 2>&1 &
  echo $! > /tmp/mock.pid
  nohup ./target/release/hy2panel test-config.toml >/tmp/panel.log 2>&1 &
  echo $! > /tmp/panel.pid
  sleep 1.5
}

stop() { kill $(cat /tmp/mock.pid) $(cat /tmp/panel.pid) 2>/dev/null; sleep 0.3; }

check() { # name, actual, expected_substr
  if echo "$2" | grep -q "$3"; then PASS=$((PASS+1)); echo "PASS: $1";
  else FAIL=$((FAIL+1)); echo "FAIL: $1  ->  got: $(echo "$2" | head -c 300)"; fi
}

api() { curl -s -H 'Content-Type: application/json' -b /tmp/cookie.txt "$@"; }

user_field() { # username, field
  api http://$PANEL/api/users | USER="$1" F="$2" python3 -c '
import json, os, sys
us = json.load(sys.stdin)["users"]
u = next((x for x in us if x["username"] == os.environ["USER"]), {})
print(u.get(os.environ["F"], ""))'
}

wait_field() { # username, field, expected_value  -> waits up to 15s
  for i in $(seq 1 30); do
    [ "$(user_field "$1" "$2")" = "$3" ] && return 0
    sleep 0.5
  done
  echo "  (timeout waiting $1.$2 = $3; got $(user_field "$1" "$2"))"
  return 1
}

wait_kick() { # waits until mock recorded a kick of alice
  for i in $(seq 1 30); do
    N=$(python3 -c 'import json;print(json.load(open("/tmp/mock_stats_state.json")).get("kicked",[]).count("alice"))' 2>/dev/null || echo 0)
    [ "$N" -ge 1 ] && return 0
    sleep 0.5
  done
  return 1
}

set_state '{"traffic":{},"online":{},"kicked":[]}'
rm -f test-panel.db*
start

# ---------- admin auth ----------
R=$(curl -s -o /dev/null -w '%{http_code}' http://$PANEL/api/overview)
check "overview requires auth (401)" "$R" "401"
R=$(curl -s -H 'Content-Type: application/json' -X POST http://$PANEL/api/login -d '{"password":"wrong"}' -o /dev/null -w '%{http_code}')
check "login rejects wrong password" "$R" "401"
R=$(curl -s -H 'Content-Type: application/json' -c /tmp/cookie.txt -X POST http://$PANEL/api/login -d '{"password":"test123"}')
check "login accepts admin password" "$R" '"ok":true'
R=$(api http://$PANEL/api/overview)
check "overview works with cookie" "$R" '"users":0'

# ---------- create user ----------
R=$(api -X POST http://$PANEL/api/users -d '{
  "username":"alice","data_limit_gb":1,"reset_mode":"monthly","reset_day":1,
  "concurrent_limit":2,"expiry":"2030-01-01T00:00:00Z","note":"test user"}')
check "create user alice" "$R" '"username":"alice"'
ALICE_PW=$(echo "$R" | python3 -c 'import json,sys;print(json.load(sys.stdin)["password"])')
ALICE_TOK=$(echo "$R" | python3 -c 'import json,sys;print(json.load(sys.stdin)["sub_token"])')
echo "  alice pw=$ALICE_PW token=$ALICE_TOK"

R=$(api -X POST http://$PANEL/api/users -d '{"username":"alice"}' -o /dev/null -w '%{http_code}')
check "duplicate username rejected" "$R" "409"

# ---------- hysteria2 auth endpoint ----------
R=$(curl -s -X POST http://$PANEL/auth -d "{\"addr\":\"1.1.1.1:1111\",\"auth\":\"nope\",\"tx\":1}")
check "auth with unknown password denied" "$R" '"ok":false'
R=$(curl -s -X POST http://$PANEL/auth -d "{\"addr\":\"1.1.1.1:1111\",\"auth\":\"$ALICE_PW\",\"tx\":1}")
check "auth with valid password allowed" "$R" "\"ok\":true,\"id\":\"alice\""
R=$(curl -s -X POST http://$PANEL/auth -d "{\"addr\":\"2.2.2.2:2222\",\"auth\":\"$ALICE_PW\",\"tx\":1}")
check "2nd device allowed (limit 2)" "$R" '"ok":true'
R=$(curl -s -X POST http://$PANEL/auth -d "{\"addr\":\"3.3.3.3:3333\",\"auth\":\"$ALICE_PW\",\"tx\":1}")
check "3rd device rejected (device limit)" "$R" "device limit reached"
# same IP reconnect: allowed once the poller confirms <= limit online (race window passes)
sleep 12
R=$(curl -s -X POST http://$PANEL/auth -d "{\"addr\":\"1.1.1.1:9999\",\"auth\":\"$ALICE_PW\",\"tx\":1}")
check "same-IP reconnect allowed after race window" "$R" '"ok":true'

# ---------- quota enforcement via stats API ----------
GB=1073741824
set_state "{\"traffic\":{\"alice\":{\"tx\":$((GB/2)),\"rx\":$((GB/2+500000))}},\"online\":{},\"kicked\":[]}"
wait_field alice status capped
R=$(curl -s -X POST http://$PANEL/auth -d "{\"addr\":\"1.1.1.1:1111\",\"auth\":\"$ALICE_PW\",\"tx\":1}")
check "auth denied after data cap" "$R" "data cap reached"
R=$(api http://$PANEL/api/users | python3 -c 'import json,sys;u=json.load(sys.stdin)["users"][0];print(u["status"],u["used_bytes"]>1000000000)')
check "user status=capped and usage recorded" "$R" "capped True"

# over-quota online user gets kicked
set_state "{\"traffic\":{\"alice\":{\"tx\":$((GB/2)),\"rx\":$((GB/2+500000))}},\"online\":{\"alice\":2},\"kicked\":[]}"
wait_kick && { echo "PASS: auto-kick fired for capped user"; PASS=$((PASS+1)); } || { echo "FAIL: auto-kick"; FAIL=$((FAIL+1)); }

# ---------- expiry ----------
api -X PUT http://$PANEL/api/users/alice -d '{"expiry":"2020-01-01T00:00:00Z"}' >/dev/null
R=$(curl -s -X POST http://$PANEL/auth -d "{\"addr\":\"1.1.1.1:1111\",\"auth\":\"$ALICE_PW\",\"tx\":1}")
check "auth denied after expiry" "$R" "subscription expired"

# ---------- disable ----------
api -X PUT http://$PANEL/api/users/alice -d '{"expiry":"","enabled":false}' >/dev/null
R=$(curl -s -X POST http://$PANEL/auth -d "{\"addr\":\"1.1.1.1:1111\",\"auth\":\"$ALICE_PW\",\"tx\":1}")
check "auth denied when disabled" "$R" "user disabled"

# restore + reset traffic -> allowed again
api -X PUT http://$PANEL/api/users/alice -d '{"enabled":true}' >/dev/null
api -X POST http://$PANEL/api/users/alice/reset-traffic >/dev/null
set_state '{"traffic":{"alice":{"tx":100,"rx":200}},"online":{"alice":1},"kicked":[]}'
wait_field alice status active
wait_field alice online 1
R=$(curl -s -X POST http://$PANEL/auth -d "{\"addr\":\"1.1.1.1:1111\",\"auth\":\"$ALICE_PW\",\"tx\":1}")
check "auth allowed after reset" "$R" '"ok":true'

# ---------- concurrency via stats API ----------
set_state '{"traffic":{"alice":{"tx":300,"rx":400}},"online":{"alice":5},"kicked":[]}'
wait_field alice online 5
R=$(curl -s -X POST http://$PANEL/auth -d "{\"addr\":\"9.9.9.9:1111\",\"auth\":\"$ALICE_PW\",\"tx\":1}")
check "device limit from stats API (5>=2)" "$R" "device limit reached"
set_state '{"traffic":{"alice":{"tx":300,"rx":400}},"online":{"alice":1},"kicked":[]}'
wait_field alice online 1

# ---------- subscriptions ----------
R=$(curl -s -D /tmp/subhdr.txt -A "sing-box/1.12" http://$PANEL/sub/$ALICE_TOK)
check "sub returns sing-box JSON" "$R" '"type": "hysteria2"'
check "sub injects per-user password" "$R" "\"password\": \"$ALICE_PW\""
check "sub has server+port+sni" "$R" '"server_name": "hy.ipkiana.space"'
check "sub has obfs salamander" "$R" '"salamander"'
check "sub keeps Iran route rules" "$R" "geosite-ir"
check "sub keeps user template fields" "$R" '"bbr_profile"'
grep -qi "subscription-userinfo: upload=" /tmp/subhdr.txt && echo "PASS: subscription-userinfo header" && PASS=$((PASS+1)) || { echo "FAIL: userinfo header"; FAIL=$((FAIL+1)); }
grep -qi "profile-title" /tmp/subhdr.txt && echo "PASS: profile-title header" && PASS=$((PASS+1)) || { echo "FAIL: profile-title"; FAIL=$((FAIL+1)); }

R=$(curl -s -A "Mozilla/5.0 (Windows NT 10.0) Firefox/128.0" http://$PANEL/sub/$ALICE_TOK)
check "browser UA gets info page" "$R" "hy2panel"
check "info page has QR svg" "$R" "<svg"
check "info page shows username" "$R" "alice"

R=$(curl -s -A curl "http://$PANEL/sub/$ALICE_TOK?target=links")
check "links target gives hysteria2 URI" "$R" "hysteria2://$ALICE_PW@45.146.241.140:443"
R=$(curl -s -A curl "http://$PANEL/sub/$ALICE_TOK?target=links&b64=1" | base64 -d)
check "b64 links decodes to URI" "$R" "obfs-password=rayan"

R=$(curl -s -o /dev/null -w '%{http_code}' http://$PANEL/sub/deadbeef)
check "invalid token 404" "$R" "404"

# ---------- usage endpoint & log & settings ----------
R=$(api "http://$PANEL/api/users/alice/usage?days=14")
check "usage endpoint returns days" "$R" '"day":"'
R=$(api "http://$PANEL/api/authlog?limit=20")
check "auth log has entries" "$R" '"addr":"1.1.1.1'
R=$(api http://$PANEL/api/settings)
check "settings endpoint" "$R" '"hy2_server":"45.146.241.140"'

api -X PUT http://$PANEL/api/settings -d '{"profile_name":"MyServer","hy2_server_port":8443}' >/dev/null
R=$(curl -s -A "sing-box" http://$PANEL/sub/$ALICE_TOK)
check "settings change reflected in sub (port)" "$R" '"server_port": 8443'

# ---------- regen token ----------
NEW=$(api -X POST http://$PANEL/api/users/alice/regen-token | python3 -c 'import json,sys;print(json.load(sys.stdin)["sub_token"])')
if [ -n "$NEW" ] && [ "$NEW" != "$ALICE_TOK" ]; then echo "PASS: token regenerated"; PASS=$((PASS+1)); else echo "FAIL: token unchanged"; FAIL=$((FAIL+1)); fi
R=$(curl -s -o /dev/null -w '%{http_code}' http://$PANEL/sub/$ALICE_TOK)
check "old token dead after regen" "$R" "404"

# ---------- root redirect ----------
R=$(curl -s -o /dev/null -w '%{http_code} %{redirect_url}' http://$PANEL/)
check "root redirects to /admin" "$R" "30[27].*/admin"

stop
echo "-------------------------------------"
echo "RESULTS: $PASS passed, $FAIL failed"
exit $FAIL
