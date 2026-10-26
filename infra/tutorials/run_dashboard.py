"""Click through each feature in the real dashboard of a local server: one
screenshot per step plus a screen recording per tutorial.
Usage: run_dashboard.py <creds.txt> <videos dir> <out.json> [tutorial id ...]
Writes <videos dir>/dash-<id>.mp4 and dash-<id>-<n>.jpg, and the step captions to out.json.
Local stack only (it deletes rules and Redis keys of org_acme)."""
import glob, json, os, secrets, subprocess, sys, tempfile, requests
from playwright.sync_api import sync_playwright

B, ORG = 'http://localhost:13000', 'org_acme'
email, pw = open(sys.argv[1]).read().split()
VIDS, OUT, ONLY = sys.argv[2], sys.argv[3], sys.argv[4:]
EXE = glob.glob(os.path.expanduser('~/.cache/ms-playwright/chromium-*/*/chrome'))[0]
W, H = 1280, 800

def redis_del(pattern):
    keys = subprocess.run(['podman', 'exec', 'ar-redis', 'redis-cli', '--scan', '--pattern', pattern], capture_output=True, text=True).stdout.split()
    if keys: subprocess.run(['podman', 'exec', 'ar-redis', 'redis-cli', 'del', *keys], capture_output=True)

# A made-up Stripe test key: Stripe answers 401 to anything forwarded with it, so
# these tours only forward to Stripe where that failure is the point (Failures).
STRIPE_KEY = 'sk_test_' + secrets.token_hex(16)
dash = requests.Session()
assert dash.post(f'{B}/api/v1/auth/login', json={'email': email, 'password': pw}).status_code == 200
KEYS = {a: dash.post(f'{B}/api/v1/agents/connect', json={'org_id': ORG, 'agent_id': a, 'label': a}).json()['api_key']
        for a in ('billing-agent', 'support-agent')}

def agent_call(payload, agent='billing-agent', headers=None, key=None, service='mockpay', action='payment.create'):
    r = requests.post(f'{B}/v1/webhook/{ORG}/{agent}', json={'service': service, 'action': action, 'payload': payload},
                      headers={'Authorization': f'Bearer {key or KEYS[agent]}', **(headers or {})})
    return r.status_code

def clear_rules():
    for path, key in [('validation-rules', 'rules'), ('hitl-rules', 'rules'), ('spend-cap-rules', 'rules'), ('action-policies', 'policies')]:
        data = dash.get(f'{B}/api/v1/{path}', params={'org_id': ORG}).json()
        for it in (data.get(key) if isinstance(data, dict) else data) or []:
            if it.get('org_id', ORG) == ORG:
                dash.delete(f'{B}/api/v1/{path}/{it["id"]}')

# A visible pointer and a caption bar, so the recording shows where each click lands.
OVERLAY = """() => {
  if (document.getElementById('tut-cursor')) return;
  const c = document.createElement('div'); c.id = 'tut-cursor';
  c.style.cssText = 'position:fixed;left:640px;top:400px;width:22px;height:22px;margin:-11px 0 0 -11px;border-radius:50%;background:rgba(242,181,68,.55);border:2px solid #F2B544;z-index:99999;pointer-events:none;transition:left .6s ease,top .6s ease,transform .15s';
  const b = document.createElement('div'); b.id = 'tut-caption';
  b.style.cssText = 'position:fixed;left:50%;bottom:22px;transform:translateX(-50%);max-width:1000px;background:#14181F;color:#fff;font:600 19px Inter,system-ui,sans-serif;padding:12px 22px;border-radius:12px;z-index:99999;pointer-events:none;box-shadow:0 6px 24px rgba(0,0,0,.35);opacity:0;transition:opacity .3s';
  document.body.append(c, b);
}"""

class Tour:
    def __init__(self, browser, tid):
        self.tid, self.steps, self.dir = tid, [], tempfile.mkdtemp()
        self.ctx = browser.new_context(viewport={'width': W, 'height': H}, record_video_dir=self.dir, record_video_size={'width': W, 'height': H})
        assert self.ctx.request.post(f'{B}/api/v1/auth/login', data={'email': email, 'password': pw}).status == 200
        self.pg = self.ctx.new_page()
        self.pg.on('dialog', lambda d: d.accept())
        self.pg.add_init_script("try { localStorage.setItem('ar-control-tab', 'setup'); localStorage.setItem('agentraas-analytics-consent', 'no'); } catch (e) {}")
        self.pg.goto(f'{B}/dashboard', wait_until='load')
        self.pg.wait_for_timeout(3000)
        self.pg.evaluate(OVERLAY)

    def say(self, text, wait=2200):
        self.pg.evaluate("t => { const b = document.getElementById('tut-caption'); b.textContent = t; b.style.opacity = t ? 1 : 0; }", text)
        self.pg.wait_for_timeout(wait)

    def point(self, sel):
        el = self.pg.locator(sel).first
        el.scroll_into_view_if_needed()
        bb = el.bounding_box()
        self.pg.evaluate("([x, y]) => { const c = document.getElementById('tut-cursor'); c.style.left = x + 'px'; c.style.top = y + 'px'; }",
                         [bb['x'] + bb['width'] / 2, bb['y'] + bb['height'] / 2])
        self.pg.wait_for_timeout(750)
        return el

    def click(self, sel, wait=900):
        el = self.point(sel)
        self.pg.evaluate("() => { const c = document.getElementById('tut-cursor'); c.style.transform = 'scale(.7)'; setTimeout(() => c.style.transform = '', 160); }")
        el.click()
        self.pg.wait_for_timeout(wait)

    def type(self, sel, text):
        el = self.point(sel)
        el.click(); el.fill('')
        el.press_sequentially(str(text), delay=45)
        self.pg.wait_for_timeout(300)

    def pick(self, sel, value):
        self.point(sel).select_option(value)
        self.pg.wait_for_timeout(500)

    def shot(self, caption):
        """One step for the docs: the caption under a screenshot (caption bar hidden, pointer kept)."""
        self.say(caption)
        n = len(self.steps) + 1
        self.pg.evaluate("() => document.getElementById('tut-caption').style.visibility = 'hidden'")
        self.pg.screenshot(path=f'{VIDS}/dash-{self.tid}-{n}.jpg', type='jpeg', quality=82)
        self.pg.evaluate("() => document.getElementById('tut-caption').style.visibility = ''")
        self.steps.append({'caption': caption, 'img': f'/videos/dash-{self.tid}-{n}.jpg'})
        self.pg.wait_for_timeout(1200)

    def tab(self, name):
        self.click(f'button[data-tab="{name}"]')

    def close_modal(self):
        self.click('.modal-overlay.open .close-btn, .modal-overlay[style*="flex"] .close-btn', wait=700)

    def refresh(self):
        self.click('#refresh-btn', wait=1500)
        self.pg.evaluate("() => { document.getElementById('activity-search').scrollIntoView({block: 'start'}); window.scrollBy(0, -90); }")
        self.pg.wait_for_timeout(600)

    def open_row(self, cls, caption):
        self.refresh()
        self.click(f'#activity-body .log-row.{cls}', wait=1500)
        self.shot(caption)

    def finish(self):
        self.say('', 600)
        video = self.pg.video.path()
        self.ctx.close()
        subprocess.run(['ffmpeg', '-y', '-loglevel', 'error', '-i', video, '-c:v', 'libx264', '-preset', 'slow', '-crf', '20',
                        '-pix_fmt', 'yuv420p', '-r', '30', '-movflags', '+faststart', '-an', f'{VIDS}/dash-{self.tid}.mp4'], check=True)
        subprocess.run(['ffmpeg', '-y', '-loglevel', 'error', '-ss', '3', '-i', f'{VIDS}/dash-{self.tid}.mp4', '-frames:v', '1', f'{VIDS}/dash-{self.tid}.jpg'], check=True)
        return self.steps

def connect(t):
    t.click('#service-connect-btn')
    t.pick('#svc-pick', 'stripe')
    t.type('#svc-key', STRIPE_KEY)
    t.shot('Setup tab, Connect a service. Pick the service (here Stripe) and paste its API key. It is encrypted and never shown again.')
    t.click('#svc-submit', wait=1800)
    t.pg.evaluate("() => { const k = document.getElementById('svc-done-key'); const v = k.textContent; document.querySelectorAll('#svc-step-done code').forEach(c => c.textContent = c.textContent.split(v).join(v.slice(0, 7) + '••••••••')); }")
    t.shot('Give your agent (or your developer) the AgentRaaS URL and key shown here. The key is shown only once.')
    t.close_modal()
    t.click('#connect-agent-btn')
    t.pick('#connect-platform', 'nocode')
    t.type('#connect-label', 'invoice bot')
    t.shot('One key per agent: + Connect agent, pick where it runs (here Make, Zapier or another no-code tool) and name it.')
    t.click('#connect-submit', wait=1800)
    key = t.pg.inner_text('#result-key').strip()
    # the key is a throwaway on a local server, but keep it off the published screenshots
    t.pg.evaluate("k => document.querySelectorAll('#connect-result-view code').forEach(c => c.textContent = c.textContent.split(k).join(k.slice(0, 7) + '••••••••'))", key)
    t.shot('Paste the setup text into your tool. When the agent makes its first call, it shows up here.')
    agent_call({'customer': 'cus_100', 'amount': 1200, 'fail': False}, agent='invoice-bot', key=key)
    t.pg.wait_for_timeout(3500)
    t.close_modal()
    t.refresh()
    t.point('#activity-body .log-row')
    t.shot('Every call is listed under Recent activity. Click a row for its details.')

def retries(t):
    t.click('#live-test-btn', wait=4000)
    t.shot('Click Run live dedup test: 8 identical calls go to a sandbox and only 1 runs.')
    t.open_row('deduplicated', 'Click a Deduplicated row to see why it was not sent again.')

def validation(t):
    t.tab('rules')
    t.click('#validation-rules-btn')
    t.type('#vr-service', 'stripe'); t.type('#vr-action', 'charge.create')
    t.type('.vrf-name', 'amount'); t.pick('.vrf-type', 'number'); t.type('.vrf-min', 1); t.type('.vrf-max', 10000)
    t.shot('Rules tab, Validation rules. Name the service and action (Stripe, charge.create), then add a field: amount must be a number from 1 to 10000.')
    t.type('#vr-test-payload', '{"amount": 50000}')
    t.click('#vr-test-btn', wait=1500)
    t.shot('Try it before saving: paste a sample and click Test. 50000 is too big, so it would be refused.')
    t.click('#vr-submit', wait=1500)
    t.shot('Click Save rule. It is listed under Saved rules and applies right away.')
    agent_call({'customer': 'cus_101', 'amount': 50000, 'currency': 'usd'}, service='stripe', action='charge.create')
    t.close_modal()
    t.open_row('blocked', 'When an agent tries to charge 50000, the call is blocked and never reaches Stripe. The row says why.')

def approvals(t):
    redis_del('hitl_usage:org_acme*')
    for p in dash.get(f'{B}/api/v1/hitl/pending').json().get('pending', []):  # leftovers from earlier runs
        dash.post(f'{B}/api/v1/hitl/{p["req_id"]}/decide', json={'decision': 'deny'})
    t.tab('rules')
    t.click('#hitl-rules-btn')
    t.type('#hitl-service', 'stripe'); t.type('#hitl-action', 'charge.create')
    t.type('#hitl-field', 'amount'); t.pick('#hitl-operator', 'gt'); t.type('#hitl-threshold', 5000)
    t.shot('Rules tab, Approval rules. Hold Stripe charges when the amount is greater than 5000.')
    t.click('#hitl-submit', wait=1500)
    t.shot('Click Save rule.')
    t.close_modal()
    agent_call({'customer': 'cus_102', 'amount': 7500, 'currency': 'usd'}, service='stripe', action='charge.create')
    t.say('Your agent now asks Stripe to charge 7500. It is held: nothing has reached Stripe yet.', 2800)
    t.tab('monitor')
    t.click('#approvals-btn', wait=1500)
    t.shot('Monitor tab, Waiting for approval: the held call, with exactly what it wants to do.')
    t.point('[data-approve]')
    t.shot('Approve runs the call once. The first decision wins, here or from Slack, Teams, Telegram, Discord or an email link.')
    t.click('[data-deny]', wait=2500)
    t.shot('Deny drops it: the charge never happens and the agent is told it was denied.')

def spend_caps(t):
    redis_del('spendcap:org_acme*')
    t.tab('rules')
    t.click('#spendcap-rules-btn')
    t.type('#spendcap-agent', 'billing-agent'); t.type('#spendcap-service', 'stripe'); t.type('#spendcap-action', 'charge.create')
    t.pick('#spendcap-window', 'day'); t.type('#spendcap-amount-field', 'amount'); t.type('#spendcap-amount-max', 10000); t.pick('#spendcap-exceed', 'block')
    t.shot('Rules tab, Spend caps. billing-agent may charge at most 10000 in total per day on Stripe. Max calls caps the number of calls instead.')
    t.click('#spendcap-submit', wait=1500)
    t.shot('Click Save rule. Leave Agent ID empty to cap every agent. "Route to approval" holds the call for a person instead of blocking it.')
    t.close_modal()
    agent_call({'customer': 'cus_201', 'amount': 12000, 'currency': 'usd'}, service='stripe', action='charge.create')
    t.open_row('blocked', 'The agent tries to charge 12000 today: over the daily total, so it is blocked before Stripe. The row says which cap stopped it.')

def policies(t):
    t.tab('rules')
    t.click('#policy-rules-btn')
    t.type('#policy-agent', 'support-agent'); t.type('#policy-service', 'resend'); t.type('#policy-action', 'email.send')
    t.pick('#policy-effect', 'require')
    t.type('#policy-fields', '{"to": {"domains": ["acme.com"]}}')
    t.shot('Rules tab, Action policies. support-agent may send email (Resend) only to addresses at acme.com.')
    t.click('#policy-submit', wait=1500)
    t.shot('Click Save policy. "Never allow" blocks an action outright; allowed hours limit when it may run.')
    t.close_modal()
    agent_call({'from': 'help@acme.com', 'to': 'someone@gmail.com', 'subject': 'Your refund', 'text': 'Hi'}, agent='support-agent', service='resend', action='email.send')
    t.open_row('blocked', 'The agent tries to email someone@gmail.com: blocked, the email is never sent. The row names the policy.')

def failures(t):
    redis_del('circuit:stripe')
    dlq = dash.get(f'{B}/api/v1/dead-letter-queue').json()
    for e in (dlq.get('entries', dlq.get('items', [])) if isinstance(dlq, dict) else dlq):  # leftovers from earlier runs
        dash.delete(f'{B}/api/v1/dead-letter-queue/{e["id"]}')
    agent_call({'customer': 'cus_401', 'amount': 2500, 'currency': 'usd'}, service='stripe', action='charge.create')
    t.say('Your agent charges a customer, and Stripe refuses: the saved key is wrong.', 2800)
    t.tab('monitor')
    t.click('#dlq-btn', wait=1500)
    t.shot('Monitor tab, Failed requests: every call the service itself refused, with its error (here Stripe: invalid API key).')
    t.click('.dlq-edit', wait=900)
    t.point('.dlq-replay-edited')
    t.shot('Fix the cause first (here: paste the right key in Setup, Credentials). Then click Replay, or Edit & Replay to change the payload first.')
    t.point('.dlq-dismiss')
    t.shot('A replayed call that succeeds leaves the list. Dismiss removes one you do not want to send.')

def undo(t):
    agent_call({'customer': 'cus_501', 'amount': 4500, 'fail': False})
    t.say('Your agent charges a customer 4500.', 2400)
    t.tab('monitor')
    t.click('#undo-btn', wait=1500)
    t.shot('Monitor tab, Undo actions: what each one will run to reverse it (here a refund).')
    t.click('.undo-go', wait=700)
    t.shot('Click Undo, then Confirm undo.')
    t.click('.undo-go', wait=2500)
    t.shot('The refund runs once. The entry is marked undone and cannot be undone twice.')

def record_replay(t):
    for name in ('checkout-test', 'checkout-flow'):
        dash.delete(f'{B}/api/v1/recordings/{name}', params={'org_id': ORG})
    for i, amt in enumerate((1500, 2500)):
        agent_call({'customer': f'cus_60{i}', 'amount': amt, 'fail': False}, headers={'X-AgentRaaS-Record': 'checkout-test'})
    t.say('Your developer runs the agent once with recording on (name: checkout-test).', 2600)
    t.tab('monitor')
    t.click('#recordings-btn', wait=1500)
    t.shot('Monitor tab, Recordings: each recorded run, how many calls it made and when.')
    t.click('.rec-view', wait=1500)
    t.shot('Click View to see every call and its answer. A replay is answered from here, so nothing reaches the real service.')

FLOWS = {'connect': connect, 'retries': retries, 'validation': validation, 'approvals': approvals, 'spend-caps': spend_caps,
         'policies': policies, 'failures': failures, 'undo': undo, 'record-replay': record_replay}

out = json.load(open(OUT)) if os.path.exists(OUT) else {}
with sync_playwright() as p:
    browser = p.chromium.launch(executable_path=EXE)
    for tid, flow in FLOWS.items():
        if ONLY and tid not in ONLY: continue
        clear_rules()
        t = Tour(browser, tid)
        flow(t)
        out[tid] = t.finish()
        print(tid, len(out[tid]), 'steps')
    browser.close()
json.dump(out, open(OUT, 'w'), indent=1)
