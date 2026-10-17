"""
agentraas scan: list what each MCP server's tools can do to the real world.

Starts a stdio MCP server the way your client would, asks it for its tools
and sorts every tool by what it can do: move money, delete things, send
messages, run code, change data, or only read. Nothing is called; only
`initialize` and `tools/list` are sent.

    agentraas scan                                  # every server in your Claude Desktop, Cursor, Claude Code configs
    agentraas scan --config .mcp.json               # one config file
    agentraas scan -- npx -y @modelcontextprotocol/server-github

Exit code: 0, or 1 with --fail-on when a tool at that risk level is found
(for CI), 2 when no server could be scanned.
"""

import argparse
import json
import os
import queue
import re
import subprocess
import sys
import threading
from collections import deque

from .mcp_wrap import READ_PREFIXES

# Name words per category, checked in this order: the first match wins.
CATEGORIES = [
    ("money", "high", {"pay", "payment", "payments", "charge", "refund", "transfer", "payout", "invoice", "subscription",
                       "purchase", "checkout", "transaction", "withdraw", "withdrawal", "billing", "price", "order", "buy"}),
    ("deletes", "high", {"delete", "remove", "drop", "destroy", "purge", "truncate", "revoke", "wipe", "erase", "terminate",
                         "kill", "reset", "unlink", "rm", "trash", "cancel"}),
    ("runs code", "high", {"exec", "execute", "run", "shell", "command", "eval", "script", "terminal", "bash", "sql", "evaluate"}),
    ("sends", "high", {"send", "email", "mail", "message", "post", "publish", "notify", "reply", "comment", "tweet", "sms",
                       "invite", "broadcast", "dm", "chat", "forward", "share"}),
    ("writes", "medium", {"create", "update", "write", "edit", "set", "put", "add", "insert", "upload", "push", "merge",
                          "commit", "deploy", "move", "rename", "patch", "modify", "save", "assign", "close", "fork",
                          "approve", "upsert", "replace", "append", "submit", "install", "enable", "disable", "release"}),
]
LEVELS = {"high": 2, "medium": 1, "low": 0}


def words(name):
    name = re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", name)
    return [w for w in re.split(r"[^a-zA-Z0-9]+", name.lower()) if w]


def classify(tool):
    """Return (category, risk, reason) for one tool from tools/list."""
    name = tool.get("name", "")
    ann = tool.get("annotations") or {}
    if ann.get("readOnlyHint") is True:
        return "reads", "low", "readOnlyHint"
    ws = words(name)
    if ws and ws[0] in READ_PREFIXES and ann.get("destructiveHint") is not True:
        return "reads", "low", "name: " + ws[0]
    for category, risk, vocab in CATEGORIES:
        hit = next((w for w in ws if w in vocab), None)
        if hit:
            return category, risk, f"name: {hit}"
    if ann.get("destructiveHint") is True:
        return "deletes", "high", "destructiveHint"
    # ponytail: unknown verbs count as writes; a word list misses some, an annotation beats it
    return "writes", "medium", "unknown verb"


class StdioServer:
    def __init__(self, cmd, env=None):
        self.p = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                  text=True, bufsize=1, env={**os.environ, **(env or {})})
        self.msgs, self.stderr = queue.Queue(), deque(maxlen=15)
        threading.Thread(target=self._read_out, daemon=True).start()
        threading.Thread(target=lambda: [self.stderr.append(l.rstrip()) for l in self.p.stderr], daemon=True).start()
        self.next_id = 0

    def _read_out(self):
        for line in self.p.stdout:
            try:
                self.msgs.put(json.loads(line))
            except ValueError:
                pass  # servers that print banners to stdout
        self.msgs.put(None)

    def request(self, method, params, timeout):
        self.next_id += 1
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params}) + "\n")
        self.p.stdin.flush()
        while True:
            msg = self.msgs.get(timeout=timeout)
            if msg is None:
                raise RuntimeError("server exited")
            if msg.get("id") == self.next_id and ("result" in msg or "error" in msg):
                if "error" in msg:
                    raise RuntimeError(f"{method}: {msg['error'].get('message')}")
                return msg["result"]

    def notify(self, method):
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "method": method}) + "\n")
        self.p.stdin.flush()

    def close(self):
        try:
            self.p.stdin.close()
            self.p.wait(timeout=3)
        except (OSError, subprocess.TimeoutExpired):
            self.p.kill()


def list_tools(cmd, env=None, timeout=60):
    srv = StdioServer(cmd, env)
    try:
        srv.request("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                                   "clientInfo": {"name": "agentraas-scan", "version": "1"}}, timeout)
        srv.notify("notifications/initialized")
        tools, cursor = [], None
        while True:
            res = srv.request("tools/list", {"cursor": cursor} if cursor else {}, timeout)
            tools += res.get("tools") or []
            cursor = res.get("nextCursor")
            if not cursor:
                return tools
    except queue.Empty:
        raise RuntimeError(f"no answer in {timeout}s" + _tail(srv))
    except (RuntimeError, OSError) as e:
        raise RuntimeError(str(e) + _tail(srv))
    finally:
        srv.close()


def _tail(srv):
    return ("\n      " + "\n      ".join(srv.stderr)) if srv.stderr else ""


def config_paths():
    home = os.path.expanduser("~")
    return [p for p in [
        os.path.join(home, "Library", "Application Support", "Claude", "claude_desktop_config.json"),
        os.path.join(os.environ.get("APPDATA", ""), "Claude", "claude_desktop_config.json") if os.environ.get("APPDATA") else "",
        os.path.join(home, ".config", "Claude", "claude_desktop_config.json"),
        os.path.join(home, ".cursor", "mcp.json"),
        os.path.join(home, ".claude.json"),
        os.path.join(os.getcwd(), ".mcp.json"),
        os.path.join(os.getcwd(), ".cursor", "mcp.json"),
    ] if p and os.path.isfile(p)]


def servers_from_config(path):
    """Yield (name, command list, env, skip reason) for each server in an mcpServers config."""
    with open(path) as f:
        cfg = json.load(f)
    blocks = [cfg.get("mcpServers") or {}]
    blocks += [(p or {}).get("mcpServers") or {} for p in (cfg.get("projects") or {}).values()]  # ~/.claude.json
    for block in blocks:
        for name, s in block.items():
            if not s.get("command"):
                yield name, None, None, "remote server (url), not scanned"
                continue
            yield name, [s["command"], *(s.get("args") or [])], s.get("env") or {}, None


def is_wrapped(cmd):
    return "agentraas" in " ".join(cmd) and "wrap" in cmd


def scan(name, cmd, env, timeout):
    entry = {"server": name, "command": cmd, "wrapped": is_wrapped(cmd)}
    try:
        tools = list_tools(cmd, env, timeout)
    except Exception as e:
        return {**entry, "error": str(e)}
    entry["tools"] = []
    for t in tools:
        category, risk, reason = classify(t)
        entry["tools"].append({"name": t.get("name"), "category": category, "risk": risk, "reason": reason})
    entry["tools"].sort(key=lambda t: (-LEVELS[t["risk"]], t["category"], t["name"] or ""))
    return entry


def render(results):
    out = []
    for r in results:
        out.append(f"\n{r['server']}  ({' '.join(r['command']) if r.get('command') else 'remote'})")
        if r.get("skipped"):
            out.append(f"  skipped: {r['skipped']}")
            continue
        if r.get("error"):
            out.append(f"  could not scan: {r['error']}")
            continue
        counts = {lvl: sum(t["risk"] == lvl for t in r["tools"]) for lvl in LEVELS}
        out.append(f"  {len(r['tools'])} tools: {counts['high']} high risk, {counts['medium']} write, {counts['low']} read-only"
                   + ("  [already wrapped by agentraas]" if r["wrapped"] else ""))
        for t in r["tools"]:
            if t["risk"] != "low":
                out.append(f"    {t['risk'].upper():6}  {t['category']:9}  {t['name']}  ({t['reason']})")
        if counts["high"] + counts["medium"] and not r["wrapped"]:
            out.append(f"  Fix duplicates and get an audit log: agentraas wrap -- {' '.join(r['command'])}")
    risky = [r for r in results if any(t["risk"] == "high" for t in r.get("tools") or [])]
    if risky:
        out.append("\nHigh-risk tools can spend money, delete data or reach people. To block, cap or hold them for")
        out.append("human approval before they run, route them through an AgentRaaS server: https://agentraas.io/mcp-scan")
    return "\n".join(out)


def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    cmd = []
    if "--" in argv:
        i = argv.index("--")
        argv, cmd = argv[:i], argv[i + 1:]
    p = argparse.ArgumentParser(prog="agentraas scan", description=__doc__.split("\n\n")[1],
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--config", action="append", default=[], metavar="PATH", help="an MCP config file (mcpServers); repeatable")
    p.add_argument("--json", action="store_true", help="print the result as JSON")
    p.add_argument("--timeout", type=float, default=60, help="seconds to wait for each server (default 60, npx downloads are slow)")
    p.add_argument("--fail-on", choices=["high", "medium"], help="exit 1 if a tool at this risk level or above is found")
    a = p.parse_args(argv)

    targets = []  # (name, cmd, env, skip reason)
    if cmd:
        targets.append((next((x for x in cmd if "mcp" in x or "server" in x), cmd[0]), cmd, {}, None))
    else:
        paths = a.config or config_paths()
        if not paths:
            p.error("no MCP config found; pass --config PATH or a server command after --")
        for path in paths:
            print(f"reading {path}", file=sys.stderr)
            try:
                targets += list(servers_from_config(path))
            except (OSError, ValueError, AttributeError) as e:
                print(f"  cannot read {path}: {e}", file=sys.stderr)
        if not targets:
            print("no MCP servers configured in " + ", ".join(paths), file=sys.stderr)
            return 2

    results = []
    for name, c, env, skip in targets:
        if skip:
            results.append({"server": name, "command": None, "skipped": skip})
            continue
        print(f"scanning {name} ...", file=sys.stderr)
        results.append(scan(name, c, env, a.timeout))

    print(json.dumps(results, indent=2) if a.json else render(results))
    if targets and all(r.get("error") or r.get("skipped") for r in results):
        return 2
    if a.fail_on and any(LEVELS[t["risk"]] >= LEVELS[a.fail_on] for r in results for t in r.get("tools") or []):
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
