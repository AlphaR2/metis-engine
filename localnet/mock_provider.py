#!/usr/bin/env python3
"""A stand-in for football-data.org (GET /v4/matches/<id>), driven by a state file.
usage: mock_provider.py <port> <state.json>
state.json: {"status": "FINISHED", "duration": "REGULAR", "home": 2, "away": 1}
Change the file while it runs to play the match."""
import json, sys
from http.server import BaseHTTPRequestHandler, HTTPServer

port, state_file = int(sys.argv[1]), sys.argv[2]

class H(BaseHTTPRequestHandler):
    def do_GET(self):
        if not self.path.startswith("/v4/matches/"):
            self.send_response(404); self.end_headers(); return
        s = json.load(open(state_file))
        body = {"status": s["status"], "score": {"duration": s.get("duration", "REGULAR"),
                "fullTime": {"home": s.get("home"), "away": s.get("away")},
                "halfTime": {"home": s.get("ht_home"), "away": s.get("ht_away")}}}
        if "reg_home" in s:
            body["score"]["regularTime"] = {"home": s["reg_home"], "away": s["reg_away"]}
        data = json.dumps(body).encode()
        self.send_response(200); self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data))); self.end_headers(); self.wfile.write(data)
    def log_message(self, *a): pass

HTTPServer(("127.0.0.1", port), H).serve_forever()
