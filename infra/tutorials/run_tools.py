"""Run the no-server tutorials (rules as code, library mode, LangChain, MCP) with
the published agentraas package and record real commands and output.
Usage: run_tools.py <venv bin dir> <creds.txt> <workdir> <out.json>"""
import json, os, subprocess, sys, time, threading

BIN, CREDS, WD, OUT = sys.argv[1:5]
os.makedirs(WD, exist_ok=True)
PY = os.path.join(BIN, 'python')
email, pw = open(CREDS).read().split()
ENV = {**os.environ, 'PATH': BIN + ':' + os.environ['PATH'], 'AGENTRAAS_URL': 'http://localhost:13000',
       'AGENTRAAS_EMAIL': email, 'AGENTRAAS_PASSWORD': pw, 'PYTHONUNBUFFERED': '1'}

def sh(cmd, shown=None):
    p = subprocess.run(cmd, shell=True, cwd=WD, env=ENV, capture_output=True, text=True, timeout=120)
    out = (p.stdout + p.stderr).rstrip()
    print(f'$ {shown or cmd}  [exit {p.returncode}]\n{out}\n')
    return out, p.returncode

def write(name, text):
    open(os.path.join(WD, name), 'w').write(text)

tuts = []
def tut(id, title, summary, setup):
    t = {'id': id, 'title': title, 'summary': summary, 'setup': setup, 'cases': [], 'kind': 'cli'}
    tuts.append(t)
    return t

def step(t, title, why, cmd, shown=None, file=None, expect=None):
    out, code = sh(cmd, shown)
    if expect is not None: assert code == expect, (title, code, out)
    t['cases'].append({'title': title, 'why': why, 'cmd': shown or cmd, 'output': out, 'exit': code, 'file': file})
    return out

# 10. Rules as code
t = tut('apply', 'Rules as code',
        'Keep rules in a YAML file in your repo, review changes like code, and apply them from CI. agentraas apply makes the server match the file; running it twice changes nothing.',
        {'install': 'pip install "agentraas[apply]"', 'env': ['AGENTRAAS_URL=http://localhost:13000', 'AGENTRAAS_EMAIL / AGENTRAAS_PASSWORD: a user who can change the org']})
yaml1 = """apiVersion: agentraas.io/v1alpha1
kind: ValidationRule
metadata: {name: sane-amounts, org: org_acme}
spec: {service: mockpay, action: payment.create, fields: {amount: {type: number, max: 10000}}}
---
apiVersion: agentraas.io/v1alpha1
kind: SpendCap
metadata: {name: refund-budget, org: org_acme}
spec: {service: mockpay, action: payment.refund, window: day, maxCalls: 20, onExceed: block}
"""
write('agentraas.yaml', yaml1)
t['setup']['file'] = {'name': 'agentraas.yaml', 'text': yaml1}
step(t, 'Preview the plan', '--dry-run prints what would change and changes nothing.', 'agtrs apply -f agentraas.yaml --dry-run', expect=0)
step(t, 'Apply it', 'Creates what is missing.', 'agtrs apply -f agentraas.yaml', expect=0)
step(t, 'Apply again: nothing to do', 'The server already matches the file.', 'agtrs apply -f agentraas.yaml', expect=0)
yaml2 = yaml1.replace('max: 10000', 'max: 5000')
write('agentraas.yaml', yaml2)
step(t, 'Change a limit in the file', 'max 10000 becomes 5000; apply replaces the rule that changed.', 'agtrs apply -f agentraas.yaml', shown='agtrs apply -f agentraas.yaml   # after changing max: 10000 to max: 5000', expect=0)
write('agentraas.yaml', yaml2.split('---')[0])
step(t, 'Remove a rule with --prune', 'Without --prune, rules missing from the file are left alone. With it, they are deleted.', 'agtrs apply -f agentraas.yaml --prune', shown='agtrs apply -f agentraas.yaml --prune   # after deleting the SpendCap from the file', expect=0)
write('agentraas.yaml', 'apiVersion: agentraas.io/v1alpha1\nkind: ValidationRule\nmetadata: {org: org_acme}\nspec: {service: mockpay, action: payment.create, fields: {amount: {type: number, max: 5000}}}\n---\napiVersion: agentraas.io/v1alpha1\nkind: SpendLimit\nmetadata: {org: org_acme}\nspec: {service: mockpay}\n')
step(t, 'A mistake fails the CI job', 'An unknown kind is reported and the command exits non-zero, so the pipeline stops.', 'agtrs apply -f agentraas.yaml', shown='agtrs apply -f agentraas.yaml   # with a typo: kind: SpendLimit')
write('agentraas.yaml', 'apiVersion: agentraas.io/v1alpha1\nkind: ValidationRule\nmetadata: {org: org_acme}\nspec: {service: mockpay, action: payment.create, fields: {amount: {type: number, max: 5000}}}\n')
sh('agtrs apply -f agentraas.yaml --prune', 'cleanup')
import requests
s = requests.Session(); s.post('http://localhost:13000/api/v1/auth/login', json={'email': email, 'password': pw})
_v = s.get('http://localhost:13000/api/v1/validation-rules', params={'org_id': 'org_acme'}).json()
for r in (_v if isinstance(_v, list) else _v['rules']):
    s.delete(f'http://localhost:13000/api/v1/validation-rules/{r["id"]}')

# 11. Library mode + chaos tester
t = tut('library', 'Library mode and the chaos tester',
        'No server at all: a decorator makes a function with side effects run once, even when it is retried, and the chaos tester finds the calls that would run twice.',
        {'install': 'pip install agentraas'})
lib = '''from agentraas.local import exactly_once

calls = 0

@exactly_once()                 # state in a local SQLite file
def charge(customer, amount):
    global calls
    calls += 1
    print(f"  charging {customer} {amount}")
    return f"ch_{customer}_{amount}"

print(charge("cus_42", 4200))   # runs
print(charge("cus_42", 4200))   # retry: cached, no second charge
print(charge("cus_42", 900))    # different arguments: runs
print("times charged:", calls)
'''
write('lib_demo.py', lib)
if os.path.exists(os.path.join(WD, 'agentraas-dedup.db')): os.remove(os.path.join(WD, 'agentraas-dedup.db'))
step(t, 'Wrap a function: a retry runs it once', 'The second call with the same arguments returns the stored result; the function body does not run.', 'python lib_demo.py', file={'name': 'lib_demo.py', 'text': lib}, expect=0)
err = '''from agentraas.local import exactly_once

attempts = 0

@exactly_once()
def send_invoice(customer):
    global attempts
    attempts += 1
    if attempts == 1:
        raise TimeoutError("provider timed out")
    return f"inv_{customer}"

for _ in range(2):
    try:
        print(send_invoice("cus_77"))
    except TimeoutError as e:
        print("  failed:", e, "(not cached, a retry runs again)")
print("attempts:", attempts)
'''
write('err_demo.py', err)
step(t, 'A failure is not cached', 'If the function raises, nothing is stored, so the retry runs for real.', 'python err_demo.py', file={'name': 'err_demo.py', 'text': err}, expect=0)
agent = open(os.path.join(os.path.dirname(os.path.abspath(__file__)), 'demo', 'agent.py')).read()
fixed = open(os.path.join(os.path.dirname(os.path.abspath(__file__)), 'demo', 'agent_fixed.py')).read()
write('agent.py', agent); write('agent_fixed.py', fixed)
step(t, 'The chaos tester finds a double charge', 'It lets each write run, then drops the response. The agent retries without an idempotency key, and the charge runs twice.', 'agentraas chaos --mock -- python agent.py', file={'name': 'agent.py', 'text': agent})
os.remove(os.path.join(WD, 'agentraas-dedup.db'))
step(t, 'With exactly_once and an idempotency key it passes', 'The retry carries the same key, so nothing doubles. Exit code 0, so CI passes.', 'agentraas chaos --mock -- python agent_fixed.py', file={'name': 'agent_fixed.py', 'text': fixed}, expect=0)

# 12. LangChain
t = tut('langchain', 'LangChain and LangGraph tools',
        'Wrap the tools that have side effects. When the model calls one again with the same arguments, or the graph re-runs after a crash, the stored result comes back instead of a second action.',
        {'install': 'pip install "agentraas[langchain]"', 'notes': ['CrewAI: from agentraas.crewai import protect_tool, same idea', 'Vercel AI SDK: protectTools in the npm package']})
lc = '''from langchain_core.tools import tool
from agentraas.langchain import protect_tool

sent = []

@tool
def send_refund(customer: str, amount: int) -> str:
    """Refund a customer."""
    sent.append((customer, amount))
    return f"refund {len(sent)} sent to {customer}"

safe = protect_tool(send_refund)

print(safe.invoke({"customer": "cus_9", "amount": 500}))   # runs
print(safe.invoke({"customer": "cus_9", "amount": 500}))   # model repeats itself: stored result
print(safe.invoke({"customer": "cus_9", "amount": 700}))   # different arguments: runs
print("refunds actually sent:", len(sent))
'''
write('lc_demo.py', lc)
if os.path.exists(os.path.join(WD, 'agentraas-dedup.db')): os.remove(os.path.join(WD, 'agentraas-dedup.db'))
step(t, 'Protect a tool', 'Use the protected tool wherever the agent takes its tools: create_react_agent, ToolNode, an AgentExecutor.', 'python lc_demo.py', file={'name': 'lc_demo.py', 'text': lc}, expect=0)

# 13. MCP
srv = r'''import json, sys, collections
# A small demo billing MCP server (stdio), written for this tutorial.
count = collections.Counter()
TOOLS = [
    {"name": "get_invoice", "description": "Look up an invoice", "inputSchema": {"type": "object"}},
    {"name": "update_note", "description": "Edit a customer note", "inputSchema": {"type": "object"}},
    {"name": "send_receipt", "description": "Email a receipt", "inputSchema": {"type": "object"}},
    {"name": "create_refund", "description": "Refund a payment", "inputSchema": {"type": "object"}},
    {"name": "delete_customer", "description": "Delete a customer", "inputSchema": {"type": "object"}},
]
for line in sys.stdin:
    msg = json.loads(line)
    if "id" not in msg:
        continue
    m, rid = msg["method"], msg["id"]
    if m == "initialize":
        res = {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}, "serverInfo": {"name": "billing-demo", "version": "1"}}
    elif m == "tools/list":
        res = {"tools": TOOLS}
    elif m == "tools/call":
        name = msg["params"]["name"]; count[name] += 1
        res = {"content": [{"type": "text", "text": f"{name} ran (time #{count[name]})"}], "isError": False}
    else:
        res = {}
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": rid, "result": res}) + "\n"); sys.stdout.flush()
'''
write('billing_server.py', srv)
t = tut('mcp', 'MCP: scan and wrap a server',
        'agentraas scan lists which tools of an MCP server can move money, delete data or send messages. agentraas wrap puts exactly-once in front of the server, so a retrying or looping model cannot run the same write twice.',
        {'install': 'pip install agentraas', 'file': {'name': 'claude_desktop_config.json', 'text': json.dumps({'mcpServers': {'billing': {'command': 'uvx', 'args': ['agentraas', 'wrap', '--', 'python', 'billing_server.py']}}}, indent=2)}})
step(t, 'Scan the server', 'Starts it, asks only for its tool list, and sorts the tools by risk. Nothing is called.', 'agentraas scan -- python billing_server.py', file={'name': 'billing_server.py', 'text': srv})
step(t, 'Scan in CI', '--fail-on high exits non-zero when a high-risk tool is present, so a new dangerous tool needs a review.', 'agentraas scan --fail-on high -- python billing_server.py; echo "exit code: $?"')

class Wrap:
    def __init__(self):
        for f in ('wrap.db', 'wrap-audit.jsonl'):
            if os.path.exists(os.path.join(WD, f)): os.remove(os.path.join(WD, f))
        self.p = subprocess.Popen([os.path.join(BIN, 'agentraas'), 'wrap', '--db', 'wrap.db', '--log', 'wrap-audit.jsonl', '--', PY, 'billing_server.py'],
                                  cwd=WD, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1)
        self.n = 0
    def rpc(self, method, params=None):
        self.n += 1
        self.p.stdin.write(json.dumps({'jsonrpc': '2.0', 'id': self.n, 'method': method, 'params': params or {}}) + '\n'); self.p.stdin.flush()
        return json.loads(self.p.stdout.readline())
w = Wrap()
w.rpc('initialize', {'protocolVersion': '2025-06-18', 'capabilities': {}, 'clientInfo': {'name': 'demo', 'version': '1'}})
w.p.stdin.write(json.dumps({'jsonrpc': '2.0', 'method': 'notifications/initialized'}) + '\n'); w.p.stdin.flush()
def tcall(title, why, name, args):
    r = w.rpc('tools/call', {'name': name, 'arguments': args})['result']
    out = r['content'][0]['text'] + ('   _meta: {"agentraas/deduplicated": true}' if (r.get('_meta') or {}).get('agentraas/deduplicated') else '')
    t['cases'].append({'title': title, 'why': why, 'cmd': f'tools/call {name} {json.dumps(args)}', 'output': out, 'exit': 0})
    print(title, '->', out)
tcall('A write tool runs', 'Through agentraas wrap, the first refund runs.', 'create_refund', {'payment': 'pay_1', 'amount': 500})
tcall('The model repeats it: answered from the record', 'Same tool, same arguments inside the window (10 minutes by default): the stored result, marked deduplicated.', 'create_refund', {'payment': 'pay_1', 'amount': 500})
tcall('Different arguments run', 'Another refund is another action.', 'create_refund', {'payment': 'pay_2', 'amount': 500})
tcall('Read tools always run', 'get_* and other read-only tools are never deduplicated, so lookups stay fresh.', 'get_invoice', {'id': 'inv_1'})
tcall('...every time', '', 'get_invoice', {'id': 'inv_1'})
w.p.stdin.close(); w.p.wait(timeout=10)
log = open(os.path.join(WD, 'wrap-audit.jsonl')).read().strip().splitlines()
t['cases'].append({'title': 'Every call is in the audit log', 'why': '~/.agentraas/mcp-audit.jsonl by default (--log to change it).', 'cmd': 'tail -n 3 ~/.agentraas/mcp-audit.jsonl', 'output': '\n'.join(log[-3:]), 'exit': 0})

json.dump(tuts, open(OUT, 'w'), indent=1)
print('wrote', len(tuts))
