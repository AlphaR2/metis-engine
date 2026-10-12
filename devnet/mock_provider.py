#!/usr/bin/env python3
"""MOCK football provider for the demo. NOT real match data.

Stands in for football-data.org (GET /v4/matches/<id>). Every response carries `"mock": true`
and an `X-Mock-Provider: true` header, so nothing it says can be mistaken for a real result.

usage: mock_provider.py <port> <state.json>
state.json: {"<match id>": {"status": "FINISHED", "duration": "REGULAR", "home": 2, "away": 1}, "default": {...}}
(a flat {"status": ...} object is also accepted and applies to every match). The file is re-read on
every request: change it while this runs to "play" a match.
"""
import json, sys
from http.server import BaseHTTPRequestHandler, HTTPServer

port, state_file = int(sys.argv[1]), sys.argv[2]


class H(BaseHTTPRequestHandler):
    def do_GET(self):
        if not self.path.startswith("/v4/matches/"):
            self.send_response(404); self.end_headers(); return
        match_id = self.path.split("?")[0].rstrip("/").split("/")[-1]
        s = json.load(open(state_file))
        if "status" not in s:
            s = s.get(match_id) or s.get("default") or {"status": "SCHEDULED"}
        body = {"mock": True, "status": s["status"], "score": {"duration": s.get("duration", "REGULAR"),
                "fullTime": {"home": s.get("home"), "away": s.get("away")},
                "halfTime": {"home": s.get("ht_home"), "away": s.get("ht_away")}}}
        if "reg_home" in s:
            body["score"]["regularTime"] = {"home": s["reg_home"], "away": s["reg_away"]}
        data = json.dumps(body).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("x-mock-provider", "true")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *a):
        pass


HTTPServer(("127.0.0.1", port), H).serve_forever()
