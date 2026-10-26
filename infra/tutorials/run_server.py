"""Run every tutorial case against a local AgentRaaS server and record the real
requests and responses to tutorials.json. Usage: run_server.py <creds.txt> <out.json>"""
import json, os, sys, time, subprocess, requests

# This laptop's stack by default; CI sets AGENTRAAS_URL and REDIS_URL (check_docs.py).
REDIS = ['redis-cli', '-u', os.environ['REDIS_URL']] if os.environ.get('REDIS_URL') else ['podman', 'exec', 'ar-redis', 'redis-cli']

def redis_del(pattern):
    # test stacks only: reset cached results and counters so a re-run starts clean
    keys = subprocess.run([*REDIS, '--scan', '--pattern', pattern], capture_output=True, text=True).stdout.split()
    for i in range(0, len(keys), 500):  # in batches: a load test leaves too many for one command line
        subprocess.run([*REDIS, 'del', *keys[i:i + 500]], capture_output=True)

for p in ['dedup:*', 'spendcap:org_acme*', 'replay:org_acme*', 'circuit:mockpay', 'circuit-notified:org_acme*', 'hitl_usage:org_acme*']: redis_del(p)

B = os.environ.get('AGENTRAAS_URL', 'http://localhost:13000')
ORG, AGENT, OTHER = 'org_acme', 'billing-agent', 'support-agent'
email, pw = open(sys.argv[1]).read().split()
dash = requests.Session()
assert dash.post(f'{B}/api/v1/auth/login', json={'email': email, 'password': pw}).status_code == 200

def key_for(agent):
    r = dash.post(f'{B}/api/v1/agents/connect', json={'org_id': ORG, 'agent_id': agent, 'label': agent})
    return r.json()['api_key']

KEYS = {AGENT: key_for(AGENT), OTHER: key_for(OTHER)}
SHOWN = {'Authorization': 'Bearer $AGENTRAAS_KEY'}
DROP = {'timestamp', 'idempotency_key', 'processor', 'upstream_status', 'service', 'action', 'status_url'}

def trim(v):
    if isinstance(v, dict):
        return {k: trim(x) for k, x in v.items() if k not in DROP}
    return v

def clear_rules():
    for path, key in [('validation-rules', 'rules'), ('hitl-rules', 'rules'), ('spend-cap-rules', 'rules'), ('action-policies', 'policies')]:
        data = dash.get(f'{B}/api/v1/{path}', params={'org_id': ORG}).json()
        items = data.get(key) if isinstance(data, dict) else data
        for it in items or []:
            if it.get('org_id', ORG) == ORG:
                dash.delete(f'{B}/api/v1/{path}/{it["id"]}')

tuts = []
def tut(id, title, summary, setup):
    clear_rules()
    t = {'id': id, 'title': title, 'summary': summary, 'setup': setup, 'cases': []}
    tuts.append(t)
    return t

def rule(t, path, body, label):
    r = dash.post(f'{B}/api/v1/{path}', json={'org_id': ORG, **body})
    assert r.status_code == 200, (path, r.text)
    t['setup'].setdefault('rules', []).append({'label': label, 'method': 'POST', 'path': f'/api/v1/{path}', 'body': {'org_id': ORG, **body}})
    return r.json()

def case(t, title, why, payload, action='payment.create', agent=AGENT, headers=None, key=None, show_headers=None, expect=None):
    body = {'service': 'mockpay', 'action': action, 'payload': payload}
    h = {'Authorization': f'Bearer {key if key is not None else KEYS[agent]}', **(headers or {})}
    r = requests.post(f'{B}/v1/webhook/{ORG}/{agent}', json=body, headers=h)
    resp = r.json()
    if expect: assert r.status_code == expect, (title, r.status_code, resp)
    t['cases'].append({'title': title, 'why': why, 'agent': agent,
                       'headers': {**(show_headers or SHOWN), **(headers or {})},
                       'body': body, 'status': r.status_code, 'response': trim(resp)})
    print(f'  {r.status_code} {title}')
    return resp

def note(t, title, why, method, path, status, response, body=None, shown=None):
    t['cases'].append({'title': title, 'why': why, 'method': method, 'path': path, 'body': body,
                       'status': status, 'response': trim(response), 'shown': shown})
    print(f'  {status} {title}')

# 1. Connect and send
t = tut('connect', 'Connect an agent and send an action',
        'Every agent gets its own key. It sends each action to AgentRaaS, which checks it, runs it once and records it.',
        {'dashboard': ['Click + Connect agent', 'Pick where the agent runs and give it a name', 'Click Connect, then copy its key and URL'],
         'api': {'method': 'POST', 'path': '/api/v1/agents/connect', 'body': {'org_id': ORG, 'agent_id': AGENT, 'label': 'Billing agent'}},
         'returns': {'api_key': 'ar_live_...', 'webhook_url': f'$AGENTRAAS_URL/v1/webhook/{ORG}/{AGENT}', 'mcp_url': '$AGENTRAAS_URL/mcp'}})
case(t, 'A normal call runs', 'The key matches the agent, so the call is checked, sent to the payment API once and recorded.', {'customer': 'cus_101', 'amount': 1500, 'fail': False}, expect=200)
case(t, 'A wrong key is refused', 'Nothing is sent. Check the key in Connect agent, or regenerate it.', {'customer': 'cus_101', 'amount': 1500}, key='ar_live_wrong', show_headers={'Authorization': 'Bearer ar_live_wrong'}, expect=401)
case(t, "A key can't act as another agent", "Each key belongs to one agent. The support agent's key is refused on the billing agent's URL.", {'customer': 'cus_102', 'amount': 900}, key=KEYS[OTHER], show_headers={'Authorization': 'Bearer $SUPPORT_AGENT_KEY'})
case(t, 'An unknown action is refused', 'Only actions the service has, or custom actions you registered, can run.', {'customer': 'cus_103'}, action='payment.teleport')

# 2. Safe retries
t = tut('retries', 'Safe retries (exactly once)',
        'An agent retries after a timeout, a crash or a loop. AgentRaaS recognises the same call and answers from its record, so the action runs once.',
        {'dashboard': ['Nothing to set up: every call is deduplicated', 'Monitor › Recent activity shows retries as deduplicated'],
         'notes': ['Same agent, same service and action, same payload = the same call', 'Or send x-agentraas-idempotency-key: one key per action, never reused for a different payload']})
first = case(t, 'First call runs', 'The call is new, so it runs and the result is stored.', {'customer': 'cus_201', 'amount': 2500, 'fail': False}, expect=200)
case(t, 'The retry is answered from the record', '"cached": true and the same payment id: the payment API was not called again.', {'customer': 'cus_201', 'amount': 2500, 'fail': False}, expect=200)
case(t, 'A different payload is a new call', 'Another amount is another action, so it runs.', {'customer': 'cus_201', 'amount': 3000, 'fail': False}, expect=200)
case(t, 'Your own idempotency key', 'With x-agentraas-idempotency-key you name the action yourself, for example by your order id.', {'customer': 'cus_202', 'amount': 700, 'fail': False}, headers={'x-agentraas-idempotency-key': 'order-5531'}, expect=200)
case(t, 'Same key, same body: answered from the record', 'The retry carries the same key, so it is the same action.', {'customer': 'cus_202', 'amount': 700, 'fail': False}, headers={'x-agentraas-idempotency-key': 'order-5531'}, expect=200)
case(t, 'Same key, different body: refused', 'A key stands for one exact action. Reusing it for something else is a bug in the caller, so it gets 422 and nothing runs.', {'customer': 'cus_202', 'amount': 9900, 'fail': False}, headers={'x-agentraas-idempotency-key': 'order-5531'}, expect=422)

# 3. Validation rules
t = tut('validation', 'Validation rules',
        'Check every payload before it is sent. A call that breaks a rule is refused with 422, nothing reaches the provider, and the answer tells the agent how to fix it.',
        {'dashboard': ['Rules › Validation rules', 'Pick the service and action', 'Add a rule per field: type, min, max, or not more than another field']})
rule(t, 'validation-rules', {'service': 'mockpay', 'action': 'payment.create', 'fields': {'amount': {'type': 'number', 'min': 100, 'max': 10000, 'maxField': 'balance'}}}, 'amount: a number from 100 to 10000, never above balance')
case(t, 'A good payload runs', 'Every field passes.', {'customer': 'cus_301', 'amount': 4000, 'balance': 9000, 'fail': False}, expect=200)
case(t, 'Too big', 'Above max: refused, nothing sent.', {'customer': 'cus_302', 'amount': 50000, 'balance': 90000}, expect=422)
case(t, 'Too small', 'Below min.', {'customer': 'cus_303', 'amount': 5, 'balance': 9000}, expect=422)
case(t, 'Wrong type', 'A string where a number belongs. The hint says what to send instead.', {'customer': 'cus_304', 'amount': 'forty', 'balance': 9000}, expect=422)
case(t, 'More than the balance', 'amount may not be above the payload\'s own balance field.', {'customer': 'cus_305', 'amount': 6000, 'balance': 2000}, expect=422)

# 4. Approvals
t = tut('approvals', 'Human approval',
        'Hold risky calls until a person says yes. The agent gets 202 and a status URL; the call runs once when approved, or never when denied.',
        {'dashboard': ['Rules › Approval rules', 'Pick the service and action', 'Optional: only when a field passes a threshold (amount > 5000)', 'Approve in Monitor › Waiting for approval, Slack, Teams, Telegram, Discord, email, or agtrs approve']})
rule(t, 'hitl-rules', {'service': 'mockpay', 'action': 'payment.create', 'field': 'amount', 'operator': 'gt', 'threshold': 5000}, 'hold payment.create when amount > 5000')
case(t, 'Under the threshold runs at once', 'amount 3000 is not above 5000, so no approval is needed.', {'customer': 'cus_401', 'amount': 3000, 'fail': False}, expect=200)
held = case(t, 'Over the threshold is held', 'The agent gets 202 and a status URL. Nothing has run yet.', {'customer': 'cus_402', 'amount': 8000, 'fail': False}, expect=202)
case(t, 'A retry while it waits is not a second request', 'The same call again gets 409 until someone decides.', {'customer': 'cus_402', 'amount': 8000, 'fail': False})
r = dash.post(f'{B}/api/v1/hitl/{held["reqId"]}/decide', json={'decision': 'approve'})
note(t, 'Approve: the call runs once', 'In the dashboard, Slack, an approval link, or agtrs approve. The held call runs now.', 'POST', f'/api/v1/hitl/{held["reqId"]}/decide', r.status_code, r.json(), {'decision': 'approve'}, shown='or: agtrs approve ' + held['reqId'])
time.sleep(1)
r = requests.get(f'{B}/v1/hitl/{held["reqId"]}', headers={'Authorization': f'Bearer {KEYS[AGENT]}'})
note(t, 'The agent checks its status URL', 'The status URL now carries the result.', 'GET', f'/v1/hitl/{held["reqId"]}', r.status_code, r.json())
held2 = case(t, 'Another big charge is held', 'A second request over the threshold.', {'customer': 'cus_403', 'amount': 9500, 'fail': False}, expect=202)
r = dash.post(f'{B}/api/v1/hitl/{held2["reqId"]}/decide', json={'decision': 'deny'})
note(t, 'Deny: it never runs', 'The call is dropped. Nothing reaches the payment API.', 'POST', f'/api/v1/hitl/{held2["reqId"]}/decide', r.status_code, r.json(), {'decision': 'deny'}, shown='or: agtrs deny ' + held2['reqId'])
r = requests.get(f'{B}/v1/hitl/{held2["reqId"]}', headers={'Authorization': f'Bearer {KEYS[AGENT]}'})
note(t, 'The status URL says denied', 'The agent can stop or ask for a smaller amount.', 'GET', f'/v1/hitl/{held2["reqId"]}', r.status_code, r.json())

# 5. Spend caps
t = tut('spend-caps', 'Spend caps and money caps',
        'Limit how often an agent may act, or how much money it may move per day. Over the limit, the call is refused (429) or held for approval.',
        {'dashboard': ['Rules › Spend caps', 'Count calls (max calls per hour or day), or add up a money field (max amount per day, in your time zone)', 'Over the limit: block, or hold for approval']})
rule(t, 'spend-cap-rules', {'agent_id': AGENT, 'service': 'mockpay', 'action': 'payment.create', 'window': 'hour', 'max_calls': 2, 'on_exceed': 'block'}, 'billing-agent: at most 2 payments an hour')
case(t, 'Call 1 of 2 runs', 'Inside the limit.', {'customer': 'cus_501', 'amount': 100, 'fail': False}, expect=200)
case(t, 'Call 2 of 2 runs', 'Still inside.', {'customer': 'cus_502', 'amount': 100, 'fail': False}, expect=200)
case(t, 'Call 3 is refused', 'The cap is reached: 429, nothing sent, until the hour rolls over.', {'customer': 'cus_503', 'amount': 100, 'fail': False}, expect=429)
case(t, 'Other agents are not affected', "The cap names billing-agent, so the support agent's call runs.", {'customer': 'cus_504', 'amount': 100, 'fail': False}, agent=OTHER, show_headers={'Authorization': 'Bearer $SUPPORT_AGENT_KEY'}, expect=200)
clear_rules()
rule(t, 'spend-cap-rules', {'service': 'mockpay', 'action': 'payment.create', 'window': 'day', 'amount_field': 'amount', 'max_amount': 10000, 'timezone': 'Asia/Kolkata', 'on_exceed': 'block'}, 'all agents: at most 10000 in amount per day (Asia/Kolkata)')
case(t, 'A money cap adds up the amounts', '6000 of the 10000 for today.', {'customer': 'cus_505', 'amount': 6000, 'fail': False}, expect=200)
case(t, 'Going over the daily total is refused', '6000 + 5000 would be 11000, above 10000. The refused call uses none of the budget.', {'customer': 'cus_506', 'amount': 5000, 'fail': False}, expect=429)
case(t, 'A smaller amount still fits', '6000 + 3000 = 9000.', {'customer': 'cus_507', 'amount': 3000, 'fail': False}, expect=200)
clear_rules()
rule(t, 'spend-cap-rules', {'agent_id': OTHER, 'service': 'mockpay', 'action': 'payment.create', 'window': 'hour', 'max_calls': 1, 'on_exceed': 'hitl'}, 'support-agent: 1 payment an hour, then ask a person')
case(t, 'Inside the cap runs', 'First payment this hour.', {'customer': 'cus_508', 'amount': 100, 'fail': False}, agent=OTHER, show_headers={'Authorization': 'Bearer $SUPPORT_AGENT_KEY'}, expect=200)
case(t, 'Over the cap waits for a person', 'on_exceed hitl: held for approval instead of refused.', {'customer': 'cus_509', 'amount': 100, 'fail': False}, agent=OTHER, show_headers={'Authorization': 'Bearer $SUPPORT_AGENT_KEY'})

# 6. Action policies
t = tut('policies', 'Action policies and allowed hours',
        'Decide what each agent may do at all: deny an action, require a field to match (an email domain), or allow it only during working hours. A refused call gets 403 naming the policy.',
        {'dashboard': ['Rules › Action policies', 'Pick an agent (or all), a service and an action (* for every action)', 'Effect: deny, or require fields / hours']})
rule(t, 'action-policies', {'agent_id': OTHER, 'service': 'mockpay', 'action': '*', 'effect': 'deny'}, 'support-agent may not use mockpay at all')
rule(t, 'action-policies', {'service': 'mockpay', 'action': 'payment.create', 'effect': 'require', 'fields': {'receipt_email': {'domains': ['acme.com']}}}, 'receipts only go to acme.com addresses')
case(t, 'Denied for this agent', 'The support agent may not take payments. The answer names the policy.', {'customer': 'cus_601', 'amount': 100, 'receipt_email': 'pat@acme.com', 'fail': False}, agent=OTHER, show_headers={'Authorization': 'Bearer $SUPPORT_AGENT_KEY'}, expect=403)
case(t, 'Allowed for the billing agent', 'The deny names only the support agent.', {'customer': 'cus_602', 'amount': 100, 'receipt_email': 'pat@acme.com', 'fail': False}, expect=200)
case(t, 'A subdomain is allowed too', 'billing.acme.com is inside acme.com.', {'customer': 'cus_603', 'amount': 100, 'receipt_email': 'pat@billing.acme.com', 'fail': False}, expect=200)
case(t, 'A look-alike domain is refused', 'acme.com.evil.io is not acme.com: a prompt-injected address is stopped.', {'customer': 'cus_604', 'amount': 100, 'receipt_email': 'pat@acme.com.evil.io', 'fail': False}, expect=403)
DAYS = ['mon', 'tue', 'wed', 'thu', 'fri', 'sat', 'sun']
today = DAYS[time.gmtime().tm_wday]
rule(t, 'action-policies', {'service': 'mockpay', 'action': 'payment.refund', 'effect': 'require', 'hours': {'tz': 'UTC', 'days': [d for d in DAYS if d != today], 'from': '09:00', 'to': '17:00'}}, f'refunds only on {", ".join(d for d in DAYS if d != today)}, 09:00 to 17:00 UTC')
case(t, 'Outside the allowed hours', 'Refunds are not allowed today, so the call is refused with the hours it is allowed in.', {'payment': 'mockpay_x1', 'fail': False}, action='payment.refund', expect=403)

# 7. Failures and the dead-letter queue
t = tut('failures', 'Failures and the dead-letter queue',
        "When the provider fails, the agent gets the error and the call is kept in the dead-letter queue, with its payload, to replay later. Nothing is silently lost.",
        {'dashboard': ['Monitor › Failed requests lists every call that failed', 'Replay sends it again with the same Idempotency-Key, so it still runs at most once']})
redis_del('circuit:mockpay')
case(t, 'The provider fails', 'mockpay with fail: true always errors (it stands in for a provider outage). The agent gets the error.', {'customer': 'cus_701', 'amount': 1200, 'fail': True})
time.sleep(1)
dl = dash.get(f'{B}/api/v1/dead-letter-queue', params={'org_id': ORG}).json()
items = dl.get('entries', dl.get('items', dl)) if isinstance(dl, dict) else dl
mine = [e for e in items if 'cus_701' in json.dumps(e)][:1]
note(t, 'It is in the dead-letter queue', 'Monitor › Failed requests, or the API, lists it with the error.', 'GET', f'/api/v1/dead-letter-queue?org_id={ORG}', 200, {'entries': mine})
if mine:
    r = dash.post(f'{B}/api/v1/dead-letter-queue/{mine[0]["id"]}/replay')
    note(t, 'Replay it', 'Sent again with the same Idempotency-Key, so it still runs at most once. This payload still says fail: true, so it fails again ("replayed": false); once a real provider is back, the replay runs it.', 'POST', f'/api/v1/dead-letter-queue/{mine[0]["id"]}/replay', r.status_code, r.json())

for i in range(8):   # keep failing until the breaker opens
    if 'Circuit breaker open' in json.dumps(case(t, 'more failures', '', {'customer': f'cus_7{20 + i}', 'amount': 1200, 'fail': True})): break
t['cases'] = [c for c in t['cases'] if c['title'] != 'more failures']
case(t, 'Repeated failures open the circuit breaker', 'Calls to mockpay now get 503 at once, without being sent, so a broken provider is not hammered and the agent can back off.', {'customer': 'cus_710', 'amount': 1200, 'fail': False}, expect=503)
time.sleep(31)
case(t, 'After 30 seconds, one call is let through', 'It works, so the breaker closes and calls flow again.', {'customer': 'cus_711', 'amount': 1200, 'fail': False}, expect=200)
case(t, 'Back to normal', 'The next call runs as usual.', {'customer': 'cus_712', 'amount': 1200, 'fail': False}, expect=200)

# 8. Undo
t = tut('undo', 'Undo an action',
        'Reverse something an agent did: a charge is refunded, a Slack message deleted, your own action reversed by the action you named. Each one can be undone once, for 30 days.',
        {'dashboard': ['Monitor › Undo actions', 'Click Undo next to the action']})
c = case(t, 'A charge runs', 'It has a known reverse (payment.refund), so it is recorded as undoable.', {'customer': 'cus_801', 'amount': 4500, 'fail': False}, expect=200)
time.sleep(1)
ul = dash.get(f'{B}/api/v1/undo-log', params={'org_id': ORG}).json()['entries']
entry = [e for e in ul if e['req_id'] == c['reqId']][0]
note(t, 'It appears in Undo actions', 'With what undoing it will run.', 'GET', f'/api/v1/undo-log?org_id={ORG}', 200, {'entries': [{k: entry[k] for k in ('id', 'req_id', 'undo_action', 'undo_payload', 'status', 'expires_at')}]})
r = dash.post(f'{B}/api/v1/undo-log/{entry["id"]}/undo', json={})
note(t, 'Undo: the charge is refunded', 'The reverse runs once, with its own idempotency key.', 'POST', f'/api/v1/undo-log/{entry["id"]}/undo', r.status_code, r.json())
r = dash.post(f'{B}/api/v1/undo-log/{entry["id"]}/undo', json={})
note(t, 'A second undo is refused', 'Already undone: 409, the refund never runs twice.', 'POST', f'/api/v1/undo-log/{entry["id"]}/undo', r.status_code, r.json())

# 9. Record and replay
dash.delete(f'{B}/api/v1/recordings/checkout-flow', params={'org_id': ORG})
t = tut('record-replay', 'Record and replay',
        'Record a run against the real providers once, then replay it in tests: every call is answered from the recording and nothing reaches the provider.',
        {'dashboard': ['Add X-AgentRaaS-Record: <name> to the calls of one run', 'Replay with X-AgentRaaS-Replay: <name> and a run id', 'Monitor › Recordings lists them'],
         'notes': ['Python: Client(..., record="refund-flow") or replay="refund-flow", run_id=...', 'JS: record / replay / runId in the Client options']})
case(t, 'Record call 1', 'Runs for real and is kept under the name checkout-flow.', {'customer': 'cus_901', 'amount': 2100, 'fail': False}, headers={'X-AgentRaaS-Record': 'checkout-flow'}, expect=200)
case(t, 'Record call 2', 'Also kept, in order.', {'customer': 'cus_901', 'amount': 2200, 'fail': False}, headers={'X-AgentRaaS-Record': 'checkout-flow'}, expect=200)
time.sleep(1.5)
rh = {'X-AgentRaaS-Replay': 'checkout-flow', 'X-AgentRaaS-Run-Id': 'ci-run-1'}
case(t, 'Replay call 1', 'Answered from the recording: same payment id, nothing sent.', {'customer': 'cus_901', 'amount': 2100, 'fail': False}, headers=rh, expect=200)
case(t, 'Replay call 2 with a changed payload', 'Still answered, but payload_matches is false, so a test can fail on it.', {'customer': 'cus_901', 'amount': 9999, 'fail': False}, headers=rh, expect=200)
case(t, 'A call the recording does not have', '404: the agent now makes more calls than it did when recorded.', {'customer': 'cus_901', 'amount': 2300, 'fail': False}, headers=rh, expect=404)

# 10. A support agent's refunds (online shopping, quick commerce)
for p in ['spendcap:org_acme*', 'hitl_usage:org_acme*']: redis_del(p)
t = tut('refund-agent', 'A support agent that refunds orders',
        'A customer says an item was missing. The support agent refunds it, but never more than half of what the order really cost, and never more than a day\'s budget. AgentRaaS reads the order itself, so the agent can\'t talk its way past the limit.',
        {'dashboard': ['Rules › Action policies: support-agent, the refund action, "Only allow when", limits {"amount": {"max_from": {...}}}, When broken: Route to approval',
                       'Rules › Spend caps: support-agent, the refund action, per day, add up amount, max total 1500, time zone Asia/Kolkata',
                       'Monitor › Waiting for approval shows the refunds that went over'],
         'notes': ['The examples use the sandbox: an order id ends in its amount (order_1200 is an order of 1200). With Cashfree the read is cashfree order.get and the field is order_amount; with Razorpay it is payment.get, field amount, minus amount_refunded',
                   'A refund with no amount, a missing order id or an order the provider can\'t find is never sent unchecked: it goes to approval too']})
rule(t, 'action-policies', {'agent_id': OTHER, 'service': 'mockpay', 'action': 'payment.refund', 'effect': 'require',
     'fields': {'amount': {'max_from': {'action': 'order.get', 'with': {'order_id': 'order_id'}, 'field': 'order_amount', 'factor': 0.5}}}, 'on_violation': 'hitl'},
     'support-agent: up to half the order runs; more goes to a person')
rule(t, 'spend-cap-rules', {'agent_id': OTHER, 'service': 'mockpay', 'action': 'payment.refund', 'window': 'day', 'amount_field': 'amount', 'max_amount': 1500, 'timezone': 'Asia/Kolkata', 'on_exceed': 'block'},
     'support-agent: at most 1500 in refunds a day (India time)')
R = {'action': 'payment.refund', 'agent': OTHER}
first = {'payment': 'mockpay_ord1200', 'order_id': 'order_1200', 'amount': 150}
case(t, 'A missing item: refund 150 of a 1200 order', 'AgentRaaS reads the order itself (1200). 150 is under half, so the refund runs.', first, expect=200, **R)
case(t, 'The same refund, retried', 'The agent timed out and asked again: answered from the record, the customer is refunded once.', first, expect=200, **R)
case(t, 'More than half the order waits for a person', '700 of a 1200 order is over the limit: held, 202 and a status URL. Nothing is refunded until someone approves.', {'payment': 'mockpay_ord1200', 'order_id': 'order_1200', 'amount': 700}, expect=202, **R)
case(t, 'No amount is not a full refund', 'Leaving the amount out means "refund everything" to many providers. It can\'t be checked, so it is held too.', {'payment': 'mockpay_ord800', 'order_id': 'order_800'}, expect=202, **R)
case(t, 'An order that does not exist', 'The read fails, so the refund is held instead of sent unchecked.', {'payment': 'mockpay_ordx', 'order_id': 'order_unknown', 'amount': 50}, expect=202, **R)
case(t, 'A second refund today', '600 of a 1300 order is under half; 750 refunded today so far.', {'payment': 'mockpay_ord1300', 'order_id': 'order_1300', 'amount': 600}, expect=200, **R)
case(t, "Over the day's refund total", '900 is under half of a 1900 order, but 750 + 900 passes the 1500 a day, so it is blocked.', {'payment': 'mockpay_ord1900', 'order_id': 'order_1900', 'amount': 900}, expect=429, **R)

clear_rules()
json.dump(tuts, open(sys.argv[2], 'w'), indent=1)
print('wrote', len(tuts), 'tutorials')
