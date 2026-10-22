"""`agentraas approvals` / `approve` / `deny` against a stub server, no AgentRaaS needed."""
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
        self.reply(200, {"pending": [HELD]})

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))) or b"{}")
        if self.path == "/api/v1/auth/login":
            return self.reply(200, {"ok": True}, cookie=True)
        if self.path == "/api/v1/hitl/req_done/decide":
            return self.reply(409, {"error": "This request was already approved."})
        decided.append((self.path, body["decision"]))
        self.reply(200, {"reqId": "req_1", "status": "approved" if body["decision"] == "approve" else "denied"})

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

server.shutdown()
print("cli approvals: ok")
