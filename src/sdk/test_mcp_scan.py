"""Run: python test_mcp_scan.py  (no dependencies; spawns a fake stdio MCP server)"""
import json
import os
import subprocess
import sys
import tempfile

from agentraas.mcp_scan import classify

FAKE_SERVER = r'''
import json, sys
print("banner on stdout, not JSON")
TOOLS = [
    {"name": "create_refund"}, {"name": "deleteRepository"}, {"name": "send_email"}, {"name": "run_query"},
    {"name": "create_issue"}, {"name": "get_order"}, {"name": "list_messages"}, {"name": "checkout_cart"},
    {"name": "snapshot", "annotations": {"readOnlyHint": True}}, {"name": "frobnicate"},
    {"name": "tidy", "annotations": {"destructiveHint": True}},
]
for line in sys.stdin:
    msg = json.loads(line)
    if "id" not in msg:
        continue
    if msg["method"] == "initialize":
        res = {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}, "serverInfo": {"name": "fake", "version": "1"}}
    elif msg["method"] == "tools/list":  # two pages, to exercise nextCursor
        cur = (msg.get("params") or {}).get("cursor")
        res = {"tools": TOOLS[6:]} if cur else {"tools": TOOLS[:6], "nextCursor": "p2"}
    else:
        res = {"content": [], "isError": True}  # the scanner must never call a tool
        sys.stderr.write("TOOL CALLED\n")
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": res}) + "\n")
    sys.stdout.flush()
'''


def run(*args):
    return subprocess.run([sys.executable, "-m", "agentraas.cli", "scan", *args], capture_output=True, text=True,
                          cwd=os.path.dirname(os.path.abspath(__file__)), timeout=60)


def test_classify():
    want = {"create_refund": "money", "deleteRepository": "deletes", "send_email": "sends", "run_query": "runs code",
            "create_issue": "writes", "get_order": "reads", "list_messages": "reads", "checkout_cart": "money",
            "frobnicate": "writes"}
    for name, cat in want.items():
        assert classify({"name": name})[0] == cat, (name, classify({"name": name}))
    assert classify({"name": "delete_x", "annotations": {"readOnlyHint": True}})[1] == "low"
    assert classify({"name": "tidy", "annotations": {"destructiveHint": True}})[0] == "deletes"


def test_scan_cli():
    d = tempfile.mkdtemp()
    server = os.path.join(d, "server.py")
    open(server, "w").write(FAKE_SERVER)

    r = run("--json", "--", sys.executable, server)
    assert r.returncode == 0, r.stderr
    [res] = json.loads(r.stdout)
    by = {t["name"]: t for t in res["tools"]}
    assert len(by) == 11 and by["create_refund"]["risk"] == "high" and by["snapshot"]["risk"] == "low"
    assert "TOOL CALLED" not in r.stderr

    r = run("--fail-on", "high", "--", sys.executable, server)
    assert r.returncode == 1 and "agentraas wrap -- " in r.stdout and "HIGH" in r.stdout, r.stdout

    cfg = os.path.join(d, "mcp.json")
    json.dump({"mcpServers": {
        "fake": {"command": sys.executable, "args": [server]},
        "remote": {"url": "https://example.com/mcp"},
        "broken": {"command": sys.executable, "args": ["-c", "import sys; sys.stderr.write('boom'); sys.exit(1)"]},
    }}, open(cfg, "w"))
    r = run("--json", "--config", cfg)
    assert r.returncode == 0, r.stderr
    res = {x["server"]: x for x in json.loads(r.stdout)}
    assert len(res["fake"]["tools"]) == 11 and "skipped" in res["remote"] and "boom" in res["broken"]["error"], res

    r = run("--config", os.path.join(d, "nothing.json"))
    assert r.returncode != 0


if __name__ == "__main__":
    test_classify()
    print("ok test_classify")
    test_scan_cli()
    print("ok test_scan_cli")
