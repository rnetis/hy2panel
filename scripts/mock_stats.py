#!/usr/bin/env python3
"""Mock of the Hysteria2 Traffic Stats API for testing hy2panel.

Endpoints: GET /traffic, GET /traffic?clear=1, GET /online, POST /kick,
plus legacy per-user GET /traffic/{id} and GET /online/{id}.
State is controlled by editing /tmp/mock_stats_state.json between polls.
"""
import json
import os
import threading
import traceback
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

STATE_FILE = "/tmp/mock_stats_state.json"
SECRET = "Se7en"
LOCK = threading.Lock()


def load_state():
    with LOCK:
        for attempt in range(5):
            try:
                with open(STATE_FILE) as f:
                    return json.load(f)
            except Exception:
                if attempt == 4:
                    traceback.print_exc()
                    return {"traffic": {}, "online": {}, "kicked": []}


def save_state(st):
    with LOCK:
        tmp = STATE_FILE + ".tmp"
        with open(tmp, "w") as f:
            json.dump(st, f)
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, STATE_FILE)


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        pass

    def _json(self, obj, code=200):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        try:
            path, _, query = self.path.partition("?")
            if self.headers.get("Authorization") != SECRET:
                self._json({"error": "unauthorized"}, 401)
                return
            st = load_state()
            if path == "/traffic":
                if "clear=1" in query:
                    st["traffic"] = {}
                    save_state(st)
                self._json(st["traffic"])
            elif path == "/online":
                self._json(st["online"])
            elif path.startswith("/traffic/"):
                u = path[len("/traffic/"):]
                self._json(st["traffic"].get(u, {"tx": 0, "rx": 0}), 200 if u in st["traffic"] else 404)
            elif path.startswith("/online/"):
                u = path[len("/online/"):]
                self._json({"online": st["online"].get(u, 0)})
            else:
                self._json({"error": "not found"}, 404)
        except Exception:
            traceback.print_exc()
            self._json({"error": "internal"}, 500)

    def do_POST(self):
        try:
            if self.headers.get("Authorization") != SECRET:
                self._json({"error": "unauthorized"}, 401)
                return
            n = int(self.headers.get("Content-Length") or 0)
            body = self.rfile.read(n)
            if self.path == "/kick":
                ids = json.loads(body or b"[]")
                st = load_state()
                st.setdefault("kicked", []).extend(ids)
                for i in ids:
                    st["online"][i] = 0
                save_state(st)
                self._json({"ok": True})
            else:
                self._json({"error": "not found"}, 404)
        except Exception:
            traceback.print_exc()
            self._json({"error": "internal"}, 500)


if __name__ == "__main__":
    port = int(os.environ.get("MOCK_PORT", "9999"))
    if not os.path.exists(STATE_FILE):
        save_state({"traffic": {}, "online": {}, "kicked": []})
    ThreadingHTTPServer(("127.0.0.1", port), H).serve_forever()
