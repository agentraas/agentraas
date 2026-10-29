#!/usr/bin/env python3
"""Write the API reference from the server's own routes.

Reads every `.route("...", get(..).post(..))` in crates/api/src, and writes
  - src/api-gateway-rs/public/openapi.json (OpenAPI 3.1, import it into Postman
    or Insomnia; served at agentraas.io/openapi.json by the Worker)
  - the "API reference" section of public/docs.html, between api-ref markers.
Every route needs a line in SUMMARY below, and every line there a route: the
script stops otherwise, so the reference can't fall behind the code. Pages,
assets and internal routes are left out (SKIP). A second run changes nothing.

    python3 infra/scripts/build-openapi.py
"""
import glob, html, json, os, re, sys

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..")
SRC = os.path.join(ROOT, "src/api-gateway-rs/crates/api/src")
PUBLIC = os.path.join(ROOT, "src/api-gateway-rs/public")
VERSION = re.search(r'^version = "([^"]+)"', open(os.path.join(ROOT, "src/api-gateway-rs/crates/api/Cargo.toml")).read(), re.M).group(1)

# Website pages, files and internals: not part of the API.
SKIP = re.compile(r"^/(|about|approve|chaos|cloud|community|dashboard/?|docs|guide|license|logo\.svg|mcp-scan|og-image\.png|pricing|privacy|readme"
                  r"|robots\.txt|security|self-host|sitemap\.xml|status|terms|trading|vs-[a-z]+|webhook-audit|js/:file|vendor/.*|internal/.*)$")

# (method, path) -> (group, summary, auth). auth: session (dashboard login), agent (agent key), none, metrics, token.
SUMMARY = {
    # Calling services: what an agent uses
    ("POST", "/v1/webhook/:org_id/:agent_id"): ("Calling services", "Run one action through AgentRaaS: body {service, action, payload}. The main call for webhooks, n8n, Make, Zapier and plain HTTP.", "agent"),
    ("POST", "/v1/sdk/:service/:action"): ("Calling services", "The same call, SDK style: the service and action in the path, the payload as the body.", "agent"),
    ("GET", "/v1/sdk/whoami"): ("Calling services", "Which org and agent an agent key belongs to.", "agent"),
    ("POST", "/mcp"): ("Calling services", "MCP endpoint (JSON-RPC): every connected service as MCP tools, through the same checks.", "agent"),
    ("GET", "/v1/hitl/:req_id"): ("Calling services", "Status of a call held for approval: pending, approved (with its result) or denied.", "agent"),
    ("GET", "/v1/inbound/:token"): ("Calling services", "Check an inbound webhook URL is live.", "token"),
    ("POST", "/v1/inbound/:token"): ("Calling services", "Receive a third-party webhook, deduplicated, and pass it on.", "token"),
    # Approvals
    ("GET", "/api/v1/hitl/pending"): ("Approvals", "Calls waiting for approval.", "session"),
    ("POST", "/api/v1/hitl/:req_id/decide"): ("Approvals", "Approve or deny a held call: {decision: approve | deny}.", "session"),
    ("POST", "/v1/hitl/link"): ("Approvals", "Read an approval link's call (the one-time link sent to Slack, Teams, Telegram, Discord or email).", "token"),
    ("POST", "/v1/hitl/link/decide"): ("Approvals", "Approve or deny through an approval link.", "token"),
    ("POST", "/v1/hitl/interactions/:org_id"): ("Approvals", "Slack's interactive button callback (Approve/Deny in Slack).", "token"),
    ("GET", "/api/v1/hitl-rules"): ("Approvals", "List approval rules.", "session"),
    ("POST", "/api/v1/hitl-rules"): ("Approvals", "Create an approval rule: {org_id, service, action, field?, operator?, threshold?}.", "session"),
    ("DELETE", "/api/v1/hitl-rules/:id"): ("Approvals", "Delete an approval rule.", "session"),
    ("GET", "/api/v1/hitl-slack-config"): ("Approvals", "The Slack destination for approval cards.", "session"),
    ("POST", "/api/v1/hitl-slack-config"): ("Approvals", "Set the Slack destination for approval cards.", "session"),
    ("GET", "/api/v1/hitl-escalation-config"): ("Approvals", "What happens to a call nobody decides on in time.", "session"),
    ("POST", "/api/v1/hitl-escalation-config"): ("Approvals", "Set the escalation for undecided calls.", "session"),
    # Rules
    ("GET", "/api/v1/validation-rules"): ("Rules", "List validation rules.", "session"),
    ("POST", "/api/v1/validation-rules"): ("Rules", "Create or replace a validation rule for a service action.", "session"),
    ("DELETE", "/api/v1/validation-rules/:id"): ("Rules", "Delete a validation rule.", "session"),
    ("POST", "/api/v1/validation-rules/test"): ("Rules", "Test a rule against a sample payload without saving it.", "session"),
    ("GET", "/api/v1/dedup-rules"): ("Rules", "List dedup rules (which fields make two calls the same).", "session"),
    ("POST", "/api/v1/dedup-rules"): ("Rules", "Create a dedup rule: {org_id, service, action, fields}.", "session"),
    ("DELETE", "/api/v1/dedup-rules/:id"): ("Rules", "Delete a dedup rule.", "session"),
    ("GET", "/api/v1/spend-cap-rules"): ("Rules", "List spend caps.", "session"),
    ("POST", "/api/v1/spend-cap-rules"): ("Rules", "Create a spend cap: calls or a money total per hour or day, then block or ask a person.", "session"),
    ("DELETE", "/api/v1/spend-cap-rules/:id"): ("Rules", "Delete a spend cap.", "session"),
    ("GET", "/api/v1/action-policies"): ("Rules", "List action policies.", "session"),
    ("POST", "/api/v1/action-policies"): ("Rules", "Create an action policy: deny, or allow only within limits, domains or hours.", "session"),
    ("DELETE", "/api/v1/action-policies/:id"): ("Rules", "Delete an action policy.", "session"),
    ("GET", "/api/v1/output-pruning"): ("Rules", "Whether tool output pruning is on for an org.", "session"),
    ("PUT", "/api/v1/output-pruning"): ("Rules", "Turn tool output pruning on or off.", "session"),
    ("GET", "/api/v1/output-sanitization"): ("Rules", "Whether output sanitization is on for an org.", "session"),
    ("PUT", "/api/v1/output-sanitization"): ("Rules", "Turn output sanitization on or off.", "session"),
    ("GET", "/api/v1/chaos"): ("Rules", "Chaos mode settings (inject failures to test an agent).", "session"),
    ("PUT", "/api/v1/chaos"): ("Rules", "Change chaos mode settings.", "session"),
    # Agents and services
    ("POST", "/api/v1/agents/connect"): ("Agents and services", "Connect an agent: returns its key (shown once) and its URLs.", "session"),
    ("GET", "/api/v1/agents"): ("Agents and services", "Agents seen recently.", "session"),
    ("GET", "/api/v1/agents/keys"): ("Agents and services", "List agent keys.", "session"),
    ("DELETE", "/api/v1/agents/keys/:id"): ("Agents and services", "Revoke an agent key.", "session"),
    ("POST", "/api/v1/agents/keys/:id/regenerate"): ("Agents and services", "Replace an agent key; the old one stops working.", "session"),
    ("PUT", "/api/v1/agents/keys/:id/services"): ("Agents and services", "Choose which connected services an agent key may use.", "session"),
    ("GET", "/api/v1/agent-identity-tokens"): ("Agents and services", "List agent identity tokens.", "session"),
    ("POST", "/api/v1/agent-identity-tokens"): ("Agents and services", "Issue an agent identity token.", "session"),
    ("DELETE", "/api/v1/agent-identity-tokens/:id"): ("Agents and services", "Revoke an agent identity token.", "session"),
    ("GET", "/api/v1/services"): ("Agents and services", "The service catalog: every service and action AgentRaaS knows.", "session"),
    ("GET", "/api/v1/credentials"): ("Agents and services", "List saved service keys (never the values).", "session"),
    ("POST", "/api/v1/credentials"): ("Agents and services", "Save a service key, encrypted.", "session"),
    ("DELETE", "/api/v1/credentials/:id"): ("Agents and services", "Delete a saved service key.", "session"),
    ("GET", "/api/v1/custom-actions"): ("Agents and services", "List custom actions (your own HTTP APIs).", "session"),
    ("POST", "/api/v1/custom-actions"): ("Agents and services", "Register a custom action.", "session"),
    ("DELETE", "/api/v1/custom-actions/:id"): ("Agents and services", "Delete a custom action.", "session"),
    ("GET", "/api/v1/mcp-servers"): ("Agents and services", "List registered MCP servers.", "session"),
    ("POST", "/api/v1/mcp-servers"): ("Agents and services", "Register an MCP server.", "session"),
    ("DELETE", "/api/v1/mcp-servers/:id"): ("Agents and services", "Delete an MCP server.", "session"),
    ("GET", "/api/v1/inbound-webhooks"): ("Agents and services", "List inbound webhook URLs.", "session"),
    ("POST", "/api/v1/inbound-webhooks"): ("Agents and services", "Create an inbound webhook URL.", "session"),
    ("DELETE", "/api/v1/inbound-webhooks/:id"): ("Agents and services", "Delete an inbound webhook URL.", "session"),
    ("GET", "/api/v1/agentgateway-targets"): ("Agents and services", "List agentgateway targets.", "session"),
    ("POST", "/api/v1/agentgateway-targets"): ("Agents and services", "Add an agentgateway target.", "session"),
    ("DELETE", "/api/v1/agentgateway-targets/:id"): ("Agents and services", "Delete an agentgateway target.", "session"),
    ("GET", "/api/v1/tunnel/connect/:org_id/:agent_id"): ("Agents and services", "Dev tunnel: the CLI's WebSocket to a local service.", "agent"),
    ("GET", "/api/v1/tunnel/stream"): ("Agents and services", "Dev tunnel: live requests (server-sent events).", "session"),
    # Monitoring
    ("GET", "/api/v1/recent"): ("Monitoring", "Recent calls with their outcome (the dashboard's activity list).", "session"),
    ("GET", "/api/v1/runs/:run_id"): ("Monitoring", "Every step of one agent run.", "session"),
    ("GET", "/api/v1/dashboard/runs/:run_id"): ("Monitoring", "One run, as the dashboard shows it.", "session"),
    ("GET", "/api/v1/dashboard/stats"): ("Monitoring", "Totals for a range (24h, 7d, 30d, 90d), optionally one org.", "session"),
    ("GET", "/api/v1/dashboard/timeseries"): ("Monitoring", "Calls over time for a range.", "session"),
    ("GET", "/api/v1/dashboard/by-service"): ("Monitoring", "Calls per service for a range.", "session"),
    ("GET", "/api/v1/stats"): ("Monitoring", "Overall counts.", "session"),
    ("GET", "/api/v1/reliability-report"): ("Monitoring", "Uptime, success rate and duplicates prevented per service.", "session"),
    ("GET", "/api/v1/dead-letter-queue"): ("Monitoring", "Failed requests (the service itself refused them).", "session"),
    ("POST", "/api/v1/dead-letter-queue/:id/replay"): ("Monitoring", "Send a failed request again, optionally with an edited payload.", "session"),
    ("DELETE", "/api/v1/dead-letter-queue/:id"): ("Monitoring", "Dismiss a failed request.", "session"),
    ("GET", "/api/v1/undo-log"): ("Monitoring", "Actions that can be undone, for 30 days.", "session"),
    ("POST", "/api/v1/undo-log/:id/undo"): ("Monitoring", "Undo an action once (for example refund a charge).", "session"),
    ("GET", "/api/v1/recordings"): ("Monitoring", "Recorded runs (X-AgentRaaS-Record).", "session"),
    ("GET", "/api/v1/recordings/:name"): ("Monitoring", "Every call and answer in one recording.", "session"),
    ("DELETE", "/api/v1/recordings/:name"): ("Monitoring", "Delete a recording.", "session"),
    ("GET", "/api/v1/shadow-rules"): ("Rules", "Agents in shadow mode (writes recorded, never sent).", "session"),
    ("POST", "/api/v1/shadow-rules"): ("Rules", "Put an agent, service or action in shadow mode.", "session"),
    ("DELETE", "/api/v1/shadow-rules/:id"): ("Rules", "Remove one shadow rule.", "session"),
    ("POST", "/api/v1/shadow-rules/promote"): ("Rules", "Take an agent out of shadow mode; nothing is replayed.", "session"),
    ("GET", "/api/v1/shadow-calls"): ("Monitoring", "Writes agents in shadow mode would have made, with the decision.", "session"),
    ("GET", "/api/v1/reports"): ("Monitoring", "Savings and reliability report as JSON, CSV or HTML.", "session"),
    ("GET", "/api/v1/reports/settings"): ("Monitoring", "Monthly report email settings.", "session"),
    ("PUT", "/api/v1/reports/settings"): ("Monitoring", "Turn the monthly report email on or off.", "session"),
    ("GET", "/api/v1/schema-drift-events"): ("Monitoring", "Times a service's response shape changed.", "session"),
    ("GET", "/api/v1/health-checks"): ("Monitoring", "Services under active monitoring.", "session"),
    ("POST", "/api/v1/health-checks"): ("Monitoring", "Turn on active monitoring for a service.", "session"),
    ("DELETE", "/api/v1/health-checks/:org_id/:service"): ("Monitoring", "Turn off active monitoring for a service.", "session"),
    ("GET", "/api/v1/notification-webhooks"): ("Monitoring", "Alert channels (Slack, Discord, Telegram, Teams, email, Jira).", "session"),
    ("POST", "/api/v1/notification-webhooks"): ("Monitoring", "Add an alert channel.", "session"),
    ("DELETE", "/api/v1/notification-webhooks/:id"): ("Monitoring", "Delete an alert channel.", "session"),
    ("POST", "/api/v1/notification-webhooks/:id/test"): ("Monitoring", "Send a test alert.", "session"),
    ("GET", "/api/v1/export/csv"): ("Monitoring", "Download the audit log as CSV.", "session"),
    ("GET", "/api/v1/usage"): ("Monitoring", "Actions used this month.", "session"),
    ("GET", "/api/v1/usage/stream"): ("Monitoring", "Live usage (server-sent events).", "session"),
    ("GET", "/health"): ("Monitoring", "Server health: Postgres, Redis and the version.", "none"),
    ("GET", "/metrics"): ("Monitoring", "Prometheus metrics.", "metrics"),
    ("GET", "/api/v1/public/status"): ("Monitoring", "Public status of each service (the status page).", "none"),
    ("GET", "/api/v1/public/execution-ledger"): ("Monitoring", "Public, hash-chained count of executions.", "none"),
    # Account
    ("POST", "/api/v1/auth/register"): ("Account", "Create an account.", "none"),
    ("POST", "/api/v1/auth/login"): ("Account", "Log in; sets the session cookie.", "none"),
    ("POST", "/api/v1/auth/logout"): ("Account", "Log out this session.", "session"),
    ("POST", "/api/v1/auth/logout-all"): ("Account", "Log out every session.", "session"),
    ("GET", "/api/v1/auth/me"): ("Account", "The logged-in user, org and plan.", "session"),
    ("POST", "/api/v1/auth/password"): ("Account", "Change the password.", "session"),
    ("POST", "/api/v1/auth/resend-verification"): ("Account", "Send the email confirmation link again.", "none"),
    ("POST", "/api/v1/auth/forgot-password"): ("Account", "Email a reset link.", "none"),
    ("POST", "/api/v1/auth/reset-password"): ("Account", "Set a new password from a reset link.", "none"),
    ("GET", "/api/v1/auth/verify-email"): ("Account", "Confirm an email address (link from the email).", "none"),
    ("POST", "/api/v1/auth/invites/accept"): ("Account", "Accept an invite to an org.", "none"),
    ("GET", "/api/v1/billing/usage"): ("Account", "Cloud usage and the monthly cap.", "session"),
    ("PUT", "/api/v1/billing/usage"): ("Account", "Change the monthly cap.", "session"),
    ("GET", "/api/v1/billing/checkout-info"): ("Account", "What the checkout needs (Cloud).", "session"),
    ("POST", "/api/v1/webhooks/paddle"): ("Account", "Paddle's billing webhook (signed by Paddle).", "token"),
    ("GET", "/api/v1/licensing/token"): ("Account", "The self-host license key for a paid plan.", "session"),
    ("GET", "/api/v1/download/self-host"): ("Account", "Download the self-host bundle.", "session"),
    ("POST", "/api/v1/download/self-host/request"): ("Account", "Ask for a self-host download link.", "session"),
    ("GET", "/api/v1/org-branding/:org_id"): ("Account", "An org's branding (name, logo) for approval pages.", "session"),
    ("PUT", "/api/v1/org-branding/:org_id"): ("Account", "Change an org's branding.", "session"),
    # Single sign-on and members
    ("GET", "/api/v1/auth/sso/:org_id/login"): ("Single sign-on and members", "Start SSO login for an org.", "none"),
    ("GET", "/api/v1/auth/sso/callback"): ("Single sign-on and members", "SSO callback from the identity provider.", "none"),
    ("GET", "/api/v1/auth/sso/:org_id/configs"): ("Single sign-on and members", "List identity providers.", "session"),
    ("POST", "/api/v1/auth/sso/:org_id/configs"): ("Single sign-on and members", "Add an identity provider (OIDC).", "session"),
    ("PUT", "/api/v1/auth/sso/:org_id/configs/:config_id"): ("Single sign-on and members", "Change an identity provider.", "session"),
    ("DELETE", "/api/v1/auth/sso/:org_id/configs/:config_id"): ("Single sign-on and members", "Remove an identity provider.", "session"),
    ("GET", "/api/v1/auth/sso/:org_id/members"): ("Single sign-on and members", "List members and roles.", "session"),
    ("PUT", "/api/v1/auth/sso/:org_id/members/:user_id"): ("Single sign-on and members", "Change a member's role.", "session"),
    ("DELETE", "/api/v1/auth/sso/:org_id/members/:user_id"): ("Single sign-on and members", "Remove a member.", "session"),
    ("GET", "/api/v1/auth/sso/:org_id/invites"): ("Single sign-on and members", "List open invites.", "session"),
    ("POST", "/api/v1/auth/sso/:org_id/invites"): ("Single sign-on and members", "Invite someone by email, with a role.", "session"),
    ("DELETE", "/api/v1/auth/sso/:org_id/invites/:id"): ("Single sign-on and members", "Cancel an invite.", "session"),
    # Admin (server admin only)
    ("GET", "/api/v1/admin/overview"): ("Server admin", "Server-wide overview.", "session"),
    ("GET", "/api/v1/admin/users"): ("Server admin", "All users.", "session"),
    ("GET", "/api/v1/admin/maintenance"): ("Server admin", "Whether maintenance mode is on.", "session"),
    ("POST", "/api/v1/admin/maintenance/pause"): ("Server admin", "Pause forwarding (calls are held, not lost).", "session"),
    ("POST", "/api/v1/admin/maintenance/resume"): ("Server admin", "Resume forwarding.", "session"),
    ("GET", "/api/v1/admin/audit/verify-integrity"): ("Server admin", "Check the audit log's hash chain.", "session"),
    ("GET", "/api/v1/admin/audit/siem-export"): ("Server admin", "Export the audit log for a SIEM.", "session"),
    # Demo and tools
    ("GET", "/api/v1/demo/session"): ("Demo and tools", "Open the read-only demo dashboard.", "none"),
    ("POST", "/api/v1/demo/live-test"): ("Demo and tools", "Send 8 identical calls to the sandbox and show that 1 ran.", "session"),
    ("POST", "/api/v1/demo/seed"): ("Demo and tools", "Fill an org with demo data.", "session"),
    ("POST", "/api/v1/demo/reset"): ("Demo and tools", "Clear an org's demo data.", "session"),
    ("POST", "/api/v1/tools/webhook-audit"): ("Demo and tools", "The free webhook audit tool.", "none"),
}

AUTH = {
    "session": [{"session": []}, {"sessionBearer": []}],
    "agent": [{"agentKey": []}],
    "metrics": [{"metricsToken": []}],
    "none": [], "token": [],
}
AUTH_WORDS = {"session": "Dashboard login", "agent": "Agent key", "metrics": "METRICS_TOKEN", "none": "None", "token": "Token in the URL or body"}

EXAMPLES = {
    ("POST", "/v1/webhook/:org_id/:agent_id"): {"service": "stripe", "action": "charge.create", "payload": {"amount": 2500, "currency": "usd", "customer": "cus_123"}},
    ("POST", "/api/v1/hitl/:req_id/decide"): {"decision": "approve"},
    ("POST", "/api/v1/agents/connect"): {"org_id": "org_acme", "agent_id": "billing-agent", "label": "billing agent"},
    ("POST", "/api/v1/hitl-rules"): {"org_id": "org_acme", "service": "stripe", "action": "charge.create", "field": "amount", "operator": "gt", "threshold": 50000},
    ("POST", "/api/v1/spend-cap-rules"): {"org_id": "org_acme", "agent_id": "billing-agent", "service": "stripe", "action": "charge.create", "window": "day", "amount_field": "amount", "max_amount": 100000, "on_exceed": "block"},
    ("POST", "/api/v1/action-policies"): {"org_id": "org_acme", "service": "resend", "action": "email.send", "effect": "require", "fields": {"to": {"domains": ["acme.com"]}}},
    ("POST", "/api/v1/auth/login"): {"email": "you@example.com", "password": "..."},
}


def routes():
    found = set()
    for f in glob.glob(os.path.join(SRC, "**", "*.rs"), recursive=True):
        for m in re.finditer(r'\.route\(\s*"([^"]+)",\s*((?:[a-z_:]+\([^()]*\)\s*\.?\s*)+),?\s*\)', open(f).read()):
            for meth in re.findall(r"\b(get|post|put|delete|patch)\([a-z_:]+\)", m.group(2)):
                if not SKIP.match(m.group(1)):
                    found.add((meth.upper(), m.group(1)))
    return found


def main():
    found = routes()
    missing = sorted(found - SUMMARY.keys())
    gone = sorted(SUMMARY.keys() - found)
    if missing or gone:
        for m, p in missing: print(f"route without a summary: {m} {p}")
        for m, p in gone: print(f"summary for a route that no longer exists: {m} {p}")
        sys.exit("build-openapi: add or remove the lines in SUMMARY")

    paths, groups = {}, {}
    for (method, path), (group, summary, auth) in sorted(SUMMARY.items(), key=lambda kv: (kv[0][1], kv[0][0])):
        oa_path = re.sub(r":([a-z_]+)", r"{\1}", path)
        op = {"tags": [group], "summary": summary, "security": AUTH[auth],
              "responses": {"200": {"description": "OK"}}}
        params = re.findall(r":([a-z_]+)", path)
        if params:
            op["parameters"] = [{"name": n, "in": "path", "required": True, "schema": {"type": "string"}} for n in params]
        if (method, path) in EXAMPLES:
            op["requestBody"] = {"content": {"application/json": {"example": EXAMPLES[(method, path)]}}}
        paths.setdefault(oa_path, {})[method.lower()] = op
        groups.setdefault(group, []).append((method, path, summary, auth))

    spec = {
        "openapi": "3.1.0",
        "info": {"title": "AgentRaaS API", "version": VERSION,
                 "description": "Generated from the server's routes by infra/scripts/build-openapi.py. Guides: https://agentraas.io/docs"},
        "servers": [{"url": "https://agentraas.io", "description": "Cloud"}, {"url": "http://localhost:13000", "description": "Self-hosted"}],
        "components": {"securitySchemes": {
            "agentKey": {"type": "http", "scheme": "bearer", "description": "An agent key (ar_live_...) from Connect agent or Connect a service."},
            "session": {"type": "apiKey", "in": "cookie", "name": "ar_session", "description": "Set by POST /api/v1/auth/login."},
            "sessionBearer": {"type": "http", "scheme": "bearer", "description": "The same session token as a bearer token."},
            "metricsToken": {"type": "http", "scheme": "bearer", "description": "METRICS_TOKEN, when set on the server."}}},
        "paths": paths,
    }
    write(os.path.join(PUBLIC, "openapi.json"), json.dumps(spec, indent=1) + "\n")

    E = html.escape
    order = ["Calling services", "Approvals", "Rules", "Agents and services", "Monitoring", "Account", "Single sign-on and members", "Server admin", "Demo and tools"]
    body = "".join(
        f'      <h3>{E(g)}</h3>\n      <table class="api-ref">\n        <tr><th>Call</th><th>What it does</th><th>Auth</th></tr>\n'
        + "".join(f'        <tr><td><code>{m}&nbsp;{E(p)}</code></td><td>{E(s)}</td><td>{E(AUTH_WORDS[a])}</td></tr>\n' for m, p, s, a in sorted(groups[g], key=lambda r: (r[1], r[0])))
        + "      </table>\n" for g in order)
    section = ('<!-- api-ref:start (generated by infra/scripts/build-openapi.py; edit there) -->\n    <section id="api-reference">\n      <h2>API reference</h2>\n'
               f'      <p>Every endpoint of the server ({len(SUMMARY)} in all), read from its code. The same list as an OpenAPI file, to import into Postman, Insomnia or a code generator: '
               '<a href="https://agentraas.io/openapi.json">openapi.json</a>. <b>Dashboard login</b> means the session cookie from <code>/api/v1/auth/login</code> '
               '(or the same token as <code>Authorization: Bearer</code>); <b>Agent key</b> is <code>Authorization: Bearer ar_live_...</code>.</p>\n'
               + body + '    </section>\n    <!-- api-ref:end -->')
    docs_path = os.path.join(PUBLIC, "docs.html")
    doc = open(docs_path).read()
    pat = re.compile(r"<!-- api-ref:start.*?<!-- api-ref:end -->", re.S)
    new = pat.sub(lambda m: section, doc) if pat.search(doc) else doc.replace('<section id="support">', section + '\n\n    <section id="support">', 1)
    write(docs_path, new)


changed = []
def write(path, text):
    if not os.path.exists(path) or open(path).read() != text:
        open(path, "w").write(text)
        changed.append(os.path.basename(path))


main()
print("updated: " + ", ".join(changed) if changed else "nothing to change")
