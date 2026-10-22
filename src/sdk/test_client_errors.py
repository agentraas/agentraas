"""A 422 from AgentRaaS raises AgentRaaSError carrying every failing field,
and its text includes each hint (what an agent framework shows the model).
Runs against a stub server, no AgentRaaS needed."""
import json
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer

import agentraas

BODY = {
    "error": "amount must be number, got string",
    "reqId": "req_1",
    "validation_errors": [
        {"field": "amount", "rule": "type", "message": "amount must be number, got string", "hint": 'Send "amount" as a JSON number: 49.99, without quotes.'},
        {"field": "currency", "rule": "enum", "message": "currency must be one of: usd, eur", "hint": 'Send "usd" (exact spelling) in "currency".'},
    ],
    "next_step": "Nothing was sent. Fix every field listed in validation_errors, following each hint, and send the call again.",
}


class Stub(BaseHTTPRequestHandler):
    def do_POST(self):
        self.rfile.read(int(self.headers.get("Content-Length", 0)))
        body = json.dumps(BODY if self.path.startswith("/v1/sdk/stripe") else {"error": "nope", "reqId": "req_2"}).encode()
        self.send_response(422 if self.path.startswith("/v1/sdk/stripe") else 401)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


server = HTTPServer(("127.0.0.1", 0), Stub)
threading.Thread(target=server.serve_forever, daemon=True).start()
client = agentraas.Client(agentraas_key="k", org_id="o", agent_id="a", base_url=f"http://127.0.0.1:{server.server_port}")

try:
    client.call("stripe", "charge.create", {"amount": "49.99", "currency": "USD"})
    raise SystemExit("expected AgentRaaSError")
except agentraas.AgentRaaSError as err:
    assert err.status_code == 422 and len(err.validation_errors) == 2, err.validation_errors
    text = str(err)
    assert "49.99, without quotes" in text and '"usd" (exact spelling)' in text and "Nothing was sent" in text, text

try:
    client.call("slack", "message.post", {})
except agentraas.AgentRaaSError as err:
    assert err.validation_errors == [] and str(err) == "[401] nope (reqId=req_2)", str(err)

# The CLI (`agentraas call` / `agtrs call`) prints the same hints and exits 1.
import contextlib
import io

from agentraas.cli import main as cli

err_out = io.StringIO()
with contextlib.redirect_stderr(err_out):
    code = cli(["call", "stripe", "charge.create", '{"amount": "49.99"}', "--key", "k", "--url", client.base_url])
assert code == 1 and "49.99, without quotes" in err_out.getvalue(), (code, err_out.getvalue())

server.shutdown()
print("client errors: ok")
