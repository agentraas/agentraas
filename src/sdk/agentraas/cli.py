"""`agentraas <command>` (short form `agtrs`): entry point for the CLI."""
import argparse
import json
import os
import sys
from urllib.parse import quote

USAGE = """usage: agentraas <command> [options]     (short form: agtrs <command>)

commands:
  call   send one action through AgentRaaS (dedup, policies, approvals, audit)
         agentraas call stripe charge.create '{"amount": 4999, "currency": "usd"}'
  approvals  list calls waiting for approval; approve or deny one by id
         agentraas approvals
         agentraas approve req_abc123      (runs the call; asks first, or --yes)
         agentraas deny req_abc123
  audit  the audit log, newest first (--limit N, --json, --csv)
         agentraas audit -n 20
  dlq    failed calls that were kept; send one again or dismiss it
         agentraas dlq
         agentraas dlq replay 42           (sends the call again; asks first, or --yes)
         agentraas dlq dismiss 42
  rules  print an org's rules as a YAML file for `apply` (start rules as code from the dashboard)
         agentraas rules --org org_acme > agentraas.yaml
  wrap   exactly-once tool calls + audit log for any MCP server
         agentraas wrap -- npx -y @modelcontextprotocol/server-github
  scan   list which MCP tools can spend money, delete data or send messages
         agentraas scan        (or: agentraas scan -- npx -y @modelcontextprotocol/server-github)
  chaos  find API calls your agent would execute twice
         agentraas chaos --mock -- python my_agent.py
  tunnel forward webhooks from a public URL to a local port
         agentraas tunnel --port 3000 --org org_acme --agent agent_1 --key ar_live_...
  apply  make an org's rules match a YAML file kept in Git
         agentraas apply -f agentraas.yaml [--dry-run] [--prune]
"""


def call(argv):
    p = argparse.ArgumentParser(
        prog="agentraas call",
        description="Send one action through AgentRaaS and print the response as JSON. "
        "Reads AGENTRAAS_KEY, AGENTRAAS_ORG, AGENTRAAS_AGENT and AGENTRAAS_URL when the flags are left out.",
    )
    p.add_argument("service", help='service name, e.g. stripe, or "custom" for a Custom Action')
    p.add_argument("action", help="action name, e.g. charge.create")
    p.add_argument("payload", nargs="?", default="{}", help="JSON body, or - to read it from stdin (default {})")
    p.add_argument("--key", default=os.environ.get("AGENTRAAS_KEY"), help="the agent's API key")
    p.add_argument("--org", default=os.environ.get("AGENTRAAS_ORG"))
    p.add_argument("--agent", default=os.environ.get("AGENTRAAS_AGENT"))
    p.add_argument("--url", default=os.environ.get("AGENTRAAS_URL"), help="AgentRaaS base URL (default http://localhost:13000)")
    p.add_argument("--idempotency-key", help="your own dedup key instead of the payload hash")
    args = p.parse_args(argv)

    if not args.key:
        p.error("an API key is required: --key or AGENTRAAS_KEY")
    try:
        payload = json.loads(sys.stdin.read() if args.payload == "-" else args.payload)
    except ValueError as err:
        p.error("payload is not valid JSON: {}".format(err))

    from . import AgentRaaSError, Client

    client = Client(args.key, org_id=args.org, agent_id=args.agent, base_url=args.url)
    try:
        result = client.call(args.service, args.action, payload, idempotency_key=args.idempotency_key)
    except AgentRaaSError as err:
        print(err, file=sys.stderr)
        return 1
    print(json.dumps(result, indent=2))
    return 0


def approvals(argv):
    p = argparse.ArgumentParser(prog="agentraas approvals", description="List calls waiting for approval (newest first, up to 100). "
                                "Logs in with AGENTRAAS_EMAIL / AGENTRAAS_PASSWORD against AGENTRAAS_URL (default https://agentraas.io).")
    p.add_argument("--json", action="store_true", help="print the raw list as JSON")
    args = p.parse_args(argv)

    from .apply import login

    s, base = login()
    r = s.get(base + "/api/v1/hitl/pending", timeout=30)
    if r.status_code != 200:
        print("error ({}): {}".format(r.status_code, r.text[:300]), file=sys.stderr)
        return 1
    pending = r.json()["pending"]
    if args.json:
        print(json.dumps(pending, indent=2))
    elif not pending:
        print("Nothing is waiting.")
    for item in [] if args.json else pending:
        print("{req_id}  {service} {action} by {agent_id}  ({org_id}, {created_at})".format(**item))
        print("  " + json.dumps(item["payload"]))
    return 0


def audit(argv):
    p = argparse.ArgumentParser(prog="agentraas audit", description="Show the audit log of every org you belong to, newest first. "
                                "Logs in with AGENTRAAS_EMAIL / AGENTRAAS_PASSWORD against AGENTRAAS_URL (default https://agentraas.io).")
    p.add_argument("-n", "--limit", type=int, default=50, help="how many entries (default 50, the server returns at most 1,000)")
    fmt = p.add_mutually_exclusive_group()
    fmt.add_argument("--json", action="store_true", help="print the entries as JSON (adds run, step, dedup hash, payload preview)")
    fmt.add_argument("--csv", action="store_true", help="print the dashboard's CSV export (newest 10,000, ignores --limit)")
    args = p.parse_args(argv)
    if args.limit < 1:
        p.error("--limit must be at least 1")

    from .apply import login

    s, base = login()
    r = s.get(base + "/api/v1/export/csv", timeout=60) if args.csv else s.get(base + "/api/v1/recent", params={"limit": args.limit}, timeout=30)
    if r.status_code != 200:
        print("error ({}): {}".format(r.status_code, r.text[:300]), file=sys.stderr)
        return 1
    if args.csv:
        print(r.text)
        return 0
    entries = r.json()
    if args.json:
        print(json.dumps(entries, indent=2))
    elif not entries:
        print("No calls recorded yet.")
    for e in [] if args.json else entries:
        print("{created_at}  {status:<16} {service} {action} by {agent_id} ({org_id}) {duration_ms}ms  {req_id}".format(**e)
              + ("  " + e["error_type"] if e.get("error_type") else ""))
    return 0


def dlq(argv):
    p = argparse.ArgumentParser(prog="agentraas dlq", description="Failed calls kept in the dead-letter queue, newest first (up to 100). "
                                "Replay one to send it again, or dismiss it. Logs in with AGENTRAAS_EMAIL / AGENTRAAS_PASSWORD "
                                "against AGENTRAAS_URL (default https://agentraas.io).")
    p.add_argument("command", nargs="?", choices=("list", "replay", "dismiss"), default="list")
    p.add_argument("id", nargs="?", type=int, help="entry id, from `agentraas dlq`")
    p.add_argument("--json", action="store_true", help="list: print the raw entries as JSON")
    p.add_argument("-y", "--yes", action="store_true", help="replay: don't ask before sending the call")
    args = p.parse_args(argv)
    if args.command != "list" and args.id is None:
        p.error("{} needs the entry id".format(args.command))

    # Replaying sends the call to the service again, so ask first (like approve).
    if args.command == "replay" and not args.yes:
        if not sys.stdin.isatty():
            p.error("replaying sends the call again; pass --yes when not at a terminal")
        if input("Send entry {} again now? [y/N] ".format(args.id)).strip().lower() not in ("y", "yes"):
            print("Not replayed.")
            return 1

    from .apply import login

    s, base = login()
    url = "{}/api/v1/dead-letter-queue".format(base)
    if args.command == "list":
        r = s.get(url, timeout=30)
    elif args.command == "replay":
        r = s.post("{}/{}/replay".format(url, args.id), timeout=60)
    else:
        r = s.delete("{}/{}".format(url, args.id), timeout=30)
    data = r.json() if r.headers.get("Content-Type", "").startswith("application/json") else {}
    if r.status_code != 200:
        print("error ({}): {}".format(r.status_code, (data.get("error") if isinstance(data, dict) else None) or r.text[:300]), file=sys.stderr)
        return 1
    if args.command == "replay":
        print("{}: replayed ({})".format(args.id, data["reqId"]))
    elif args.command == "dismiss":
        print("{}: dismissed".format(args.id))
    elif args.json:
        print(json.dumps(data, indent=2))
    elif not data:
        print("No failed calls waiting.")
    for e in [] if args.command != "list" or args.json else data:
        print("{id:<6} {created_at}  {service} {action} by {agent_id} ({org_id})  {req_id}".format(**e))
        print("  " + (e.get("error_message") or "no error message"))
    return 0


def decide(decision, argv):
    p = argparse.ArgumentParser(prog="agentraas " + decision, description="{} a held call by its request id (from `agentraas approvals`).".format(decision.capitalize()))
    p.add_argument("req_id")
    if decision == "approve":
        p.add_argument("-y", "--yes", action="store_true", help="don't ask before running the call")
    args = p.parse_args(argv)

    # Approving runs the call for real, so ask first (like the dashboard's Confirm click).
    if decision == "approve" and not args.yes:
        if not sys.stdin.isatty():
            p.error("approving runs the call; pass --yes when not at a terminal")
        if input("Approve {} and run the call now? [y/N] ".format(args.req_id)).strip().lower() not in ("y", "yes"):
            print("Not approved.")
            return 1

    from .apply import login

    s, base = login()
    r = s.post("{}/api/v1/hitl/{}/decide".format(base, quote(args.req_id, safe="")), json={"decision": decision}, timeout=60)
    data = r.json() if r.headers.get("Content-Type", "").startswith("application/json") else {}
    if r.status_code != 200:
        print("error ({}): {}".format(r.status_code, data.get("error") or r.text[:300]), file=sys.stderr)
        return 1
    print("{}: {}".format(args.req_id, data["status"]))
    # Approved but the provider call failed: it is in Failed requests (the DLQ).
    return 1 if data["status"] == "failed" else 0


def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    if not argv or argv[0] in ("-h", "--help"):
        print(USAGE)
        return 0
    cmd, rest = argv[0], argv[1:]
    if cmd == "call":
        return call(rest)
    elif cmd == "approvals":
        return approvals(rest)
    elif cmd in ("approve", "deny"):
        return decide(cmd, rest)
    elif cmd == "audit":
        return audit(rest)
    elif cmd == "dlq":
        return dlq(rest)
    elif cmd == "rules":
        from .apply import export as run
    elif cmd == "wrap":
        from .mcp_wrap import main as run
    elif cmd == "scan":
        from .mcp_scan import main as run
    elif cmd == "chaos":
        from .chaos import main as run
    elif cmd == "tunnel":
        from .tunnel import main as run
    elif cmd == "apply":
        from .apply import main as run
    else:
        print(USAGE, file=sys.stderr)
        return 2
    return run(rest)


if __name__ == "__main__":
    sys.exit(main())
