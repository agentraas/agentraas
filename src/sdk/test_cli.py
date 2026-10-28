"""`agentraas approvals` / `approve` / `deny` / `audit` / `dlq` / `rules` against a stub server, no AgentRaaS needed."""
import contextlib
import io
import json
import os
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer

from agentraas.cli import main as cli

HELD = {"req_id": "req_1", "org_id": "org_a", "agent_id": "agent_1", "service": "stripe", "action": "charge.create",
        "payload": {"amount": 4999}, "run_id": None, "created_at": "2026-10-22T09:00:00Z"}
decided = []
AUDIT = [
    {"req_id": "req_2", "api_key": "ar_live_…", "org_id": "org_a", "agent_id": "agent_1", "service": "stripe", "action": "charge.create",
     "status": "blocked", "error_type": "policy", "duration_ms": 3, "created_at": "2026-10-22T09:01:00Z"},
    {"req_id": "req_1", "api_key": "ar_live_…", "org_id": "org_a", "agent_id": "agent_1", "service": "stripe", "action": "charge.create",
     "status": "success", "error_type": None, "duration_ms": 120, "created_at": "2026-10-22T09:00:00Z"},
]
CSV = 'Timestamp,Request ID\n"2026-10-22T09:01:00+00:00","req_2"'
DLQ = [{"id": 42, "req_id": "req_9", "org_id": "org_a", "agent_id": "agent_1", "service": "stripe", "action": "charge.create",
        "error_message": "HTTP 503", "created_at": "2026-10-28T09:00:00Z", "payload": {"amount": 100}}]
dlq_calls = []
# The servers' list answers, rows as stored (with server defaults filled in).
RULES = {
    "/api/v1/dedup-rules": [{"id": 1, "org_id": "org_a", "service": "stripe", "action": "charge.create", "fields": ["customer", "amount"],
                             "ttl_seconds": 300, "normalize": False, "semantic_enabled": False, "semantic_threshold": 0.85}],
    "/api/v1/validation-rules": [],
    "/api/v1/action-policies": {"policies": [{"id": 2, "org_id": "org_a", "agent_id": None, "service": "slack", "action": "message.post",
                                              "effect": "require", "fields": {"channel": {"enum": ["#support"]}}, "on_violation": "block", "hours": None}]},
    "/api/v1/spend-cap-rules": {"rules": [{"id": 3, "org_id": "org_b", "agent_id": "agent_1", "service": "razorpayx", "action": "payout.create",
                                           "window": "day", "max_calls": None, "on_exceed": "hitl", "amount_field": "amount", "max_amount": 5000000,
                                           "timezone": "Asia/Kolkata"}]},
}


class Stub(BaseHTTPRequestHandler):
    def reply(self, code, body, cookie=False):
        data = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        if cookie:
            self.send_header("Set-Cookie", "session=s1; Path=/")
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if "session=s1" not in self.headers.get("Cookie", ""):
            return self.reply(401, {"error": "not logged in"})
        if self.path.startswith("/api/v1/recent"):
            limit = int(self.path.split("limit=")[1])
            return self.reply(200, AUDIT[:limit])
        if self.path in RULES:
            return self.reply(200, RULES[self.path])
        if self.path == "/api/v1/dead-letter-queue":
            return self.reply(200, DLQ)
        if self.path == "/api/v1/export/csv":
            self.send_response(200)
            self.send_header("Content-Type", "text/csv")
            self.end_headers()
            return self.wfile.write(CSV.encode())
        self.reply(200, {"pending": [HELD]})

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))) or b"{}")
        if self.path == "/api/v1/auth/login":
            return self.reply(200, {"ok": True}, cookie=True)
        if self.path == "/api/v1/dead-letter-queue/42/replay":
            dlq_calls.append("replay")
            return self.reply(200, {"replayed": True, "result": {}, "reqId": "req_10"})
        if self.path == "/api/v1/dead-letter-queue/7/replay":
            return self.reply(503, {"error": "Service unavailable", "replayed": False})
        if self.path == "/api/v1/hitl/req_done/decide":
            return self.reply(409, {"error": "This request was already approved."})
        decided.append((self.path, body["decision"]))
        self.reply(200, {"reqId": "req_1", "status": "approved" if body["decision"] == "approve" else "denied"})

    def do_DELETE(self):
        if self.path == "/api/v1/dead-letter-queue/42":
            dlq_calls.append("dismiss")
            return self.reply(200, {"dismissed": True})
        self.reply(404, {"error": "Dead-letter entry not found (already replayed, dismissed, or not yours)."})

    def log_message(self, *args):
        pass


def run(*argv):
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        try:
            code = cli(list(argv))
        except SystemExit as e:  # argparse errors
            code = e.code
    return code, out.getvalue(), err.getvalue()


server = HTTPServer(("127.0.0.1", 0), Stub)
threading.Thread(target=server.serve_forever, daemon=True).start()
os.environ.update(AGENTRAAS_URL="http://127.0.0.1:{}".format(server.server_port), AGENTRAAS_EMAIL="ci@example.com", AGENTRAAS_PASSWORD="pw")

code, out, _ = run("approvals")
assert code == 0 and "req_1  stripe charge.create by agent_1" in out and '"amount": 4999' in out, out
code, out, _ = run("approvals", "--json")
assert code == 0 and json.loads(out) == [HELD], out

# Approve runs the call: refused without --yes when stdin is not a terminal, and nothing is sent.
code, _, err = run("approve", "req_1")
assert code == 2 and "--yes" in err and decided == [], (code, err, decided)
code, out, _ = run("approve", "req_1", "--yes")
assert code == 0 and out.strip() == "req_1: approved" and decided == [("/api/v1/hitl/req_1/decide", "approve")], (out, decided)
code, out, _ = run("deny", "req_1")
assert code == 0 and out.strip() == "req_1: denied", out
code, _, err = run("deny", "req_done")
assert code == 1 and "already approved" in err, err

code, out, _ = run("audit")
lines = out.strip().splitlines()
assert code == 0 and len(lines) == 2 and lines[0].startswith("2026-10-22T09:01:00Z  blocked          stripe") and lines[0].endswith("req_2  policy"), out
assert "120ms  req_1" in lines[1] and not lines[1].endswith("None"), out
code, out, _ = run("audit", "-n", "1", "--json")
assert code == 0 and json.loads(out) == AUDIT[:1], out
code, out, _ = run("audit", "--csv")
assert code == 0 and out.strip() == CSV, out
code, _, err = run("audit", "--json", "--csv")
assert code == 2 and "not allowed with" in err, err
code, _, err = run("audit", "-n", "0")
assert code == 2 and "at least 1" in err, err

code, out, _ = run("dlq")
assert code == 0 and out.startswith("42     2026-10-28T09:00:00Z  stripe charge.create by agent_1 (org_a)  req_9") and "HTTP 503" in out, out
code, out, _ = run("dlq", "--json")
assert code == 0 and json.loads(out) == DLQ, out
# Replay sends the call again: refused without --yes off a terminal, and nothing is sent.
code, _, err = run("dlq", "replay", "42")
assert code == 2 and "--yes" in err and dlq_calls == [], (code, err)
code, out, _ = run("dlq", "replay", "42", "--yes")
assert code == 0 and out.strip() == "42: replayed (req_10)" and dlq_calls == ["replay"], out
code, _, err = run("dlq", "replay", "7", "-y")
assert code == 1 and "error (503): Service unavailable" in err, err
code, out, _ = run("dlq", "dismiss", "42")
assert code == 0 and out.strip() == "42: dismissed" and dlq_calls == ["replay", "dismiss"], out
code, _, err = run("dlq", "dismiss", "8")
assert code == 1 and "not found" in err, err
code, _, err = run("dlq", "dismiss")
assert code == 2 and "needs the entry id" in err, err

# rules: what the server has, as a file apply accepts; applying it changes nothing.
import yaml
from agentraas import apply as ap
code, out, _ = run("rules")
docs = list(yaml.safe_load_all(out))
assert code == 0 and [d["kind"] for d in docs] == ["DedupRule", "ActionPolicy", "SpendCap"], out
assert docs[0]["spec"] == {"service": "stripe", "action": "charge.create", "fields": ["customer", "amount"], "ttlSeconds": 300}, docs[0]
assert docs[2]["metadata"] == {"name": "agent-1-razorpayx-payout-create-day", "org": "org_b"} and docs[2]["spec"]["onExceed"] == "hitl", docs[2]
desired = [(d["kind"], ap.normalize(d["kind"], d["metadata"]["org"], d["spec"]), d["spec"], "") for d in docs]
current = {k: [dict(r, norm=ap.normalize(k, r["org_id"], r)) for r in (v if isinstance(v, list) else next(iter(v.values())))]
           for k, v in zip(ap.KINDS, RULES.values())}
assert ap.plan(desired, current, prune=True) == ([], []), ap.plan(desired, current, prune=True)
code, out, _ = run("rules", "--org", "org_b")
assert code == 0 and [d["kind"] for d in yaml.safe_load_all(out)] == ["SpendCap"], out

server.shutdown()
print("cli: ok")
