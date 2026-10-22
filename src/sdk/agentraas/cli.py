"""`agentraas <command>` (short form `agtrs`): entry point for the CLI."""
import argparse
import json
import os
import sys

USAGE = """usage: agentraas <command> [options]     (short form: agtrs <command>)

commands:
  call   send one action through AgentRaaS (dedup, policies, approvals, audit)
         agentraas call stripe charge.create '{"amount": 4999, "currency": "usd"}'
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


def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    if not argv or argv[0] in ("-h", "--help"):
        print(USAGE)
        return 0
    cmd, rest = argv[0], argv[1:]
    if cmd == "call":
        return call(rest)
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
