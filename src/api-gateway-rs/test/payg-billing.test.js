// Cloud billing (billing.rs): one-time trial, Team/Enterprise plans, overage. Needs a cloud-mode server:
// DEPLOYMENT_MODE=cloud, and for the BILLING_ON cases also
// BILLING_PAYG_ENABLED=true with no PADDLE_API_KEY (so the monthly job can
// never reach Paddle: a due charge must end up "failed", not charged).
//   BILLING_ON=1 node --test test/payg-billing.test.js   (billing switched on)
//   node --test test/payg-billing.test.js                (billing off)
// The charge cases seed last month's audit rows, then need the job to run:
// it runs once at startup, so restart the container after the first pass
// and run again with CHARGE_CHECK=1.

const test = require('node:test');
const assert = require('node:assert/strict');
const axios = require('axios');
const { Client } = require('pg');

const BASE_URL = process.env.TEST_BASE_URL || 'http://localhost:3000';
const client = axios.create({ baseURL: BASE_URL, validateStatus: () => true, maxRedirects: 0 });
const BILLING_ON = process.env.BILLING_ON === '1';
const CHARGE_CHECK = process.env.CHARGE_CHECK === '1';
const RUN_ID = Date.now();

async function registerAndVerify(email, password, orgId) {
  const registerRes = await client.post('/api/v1/auth/register', { email, password, org_id: orgId });
  assert.equal(registerRes.status, 200, JSON.stringify(registerRes.data));
  const token = new URL(registerRes.data.dev_verify_url).searchParams.get('verify_token');
  const verifyRes = await client.get(`/api/v1/auth/verify-email?token=${token}`);
  assert.equal(verifyRes.status, 200);
  return verifyRes.headers['set-cookie'][0].split(';')[0];
}

function lastMonth() {
  const d = new Date();
  const first = new Date(Date.UTC(d.getUTCFullYear(), d.getUTCMonth() - 1, 1, 12));
  return { key: first.toISOString().slice(0, 7), at: first.toISOString() };
}

let pg;
test.before(async () => {
  pg = new Client({ connectionString: process.env.DATABASE_URL || 'postgres://agentraas:agentraas@localhost:5432/agentraas' });
  await pg.connect();
});
test.after(async () => {
  await pg.end();
});

test('usage endpoint, cap validation, trial and plan limits', { skip: CHARGE_CHECK }, async (t) => {
  const orgId = `org_payg_${RUN_ID}`;
  const cookie = await registerAndVerify(`payg-${RUN_ID}@internal.test`, 'validpassword123', orgId);
  const headers = { Cookie: cookie };
  const mode = (await client.get('/api/v1/auth/me', { headers })).data.user.deployment_mode;
  if (mode !== 'cloud') {
    t.skip('Cloud billing only; self-hosted servers have no usage limit');
    return;
  }

  let u = await client.get(`/api/v1/billing/usage?org_id=${orgId}`, { headers });
  assert.equal(u.status, 200, JSON.stringify(u.data));
  assert.equal(u.data.enabled, BILLING_ON);
  assert.equal(u.data.cloud, true);
  assert.equal(u.data.plan, 'free');
  assert.equal(u.data.trial_used, 0);
  assert.equal(u.data.monthly_cap_usd, 10);

  const other = await client.get('/api/v1/billing/usage?org_id=org_someone_else', { headers });
  assert.equal(other.status, 403);

  assert.equal((await client.put('/api/v1/billing/usage', { org_id: orgId, monthly_cap_usd: 0 }, { headers })).status, 422);
  assert.equal((await client.put('/api/v1/billing/usage', { org_id: orgId, monthly_cap_usd: 3 }, { headers })).status, 200);

  assert.equal((await client.get('/api/v1/billing/checkout-info?plan=payg', { headers })).status, 422, 'payg is no longer a plan');
  const checkout = await client.get('/api/v1/billing/checkout-info?plan=enterprise', { headers });
  assert.equal(checkout.status, 503, 'no Paddle keys locally: never a checkout');

  // Free: the one-time trial. Team: what it includes + what $3 buys (3 x 500).
  let usage = await client.get('/api/v1/usage', { headers });
  assert.equal(usage.data.limit, u.data.trial_actions);
  await pg.query("UPDATE users SET plan = 'team' WHERE org_id = $1", [orgId]);
  u = await client.get(`/api/v1/billing/usage?org_id=${orgId}`, { headers });
  assert.equal(u.data.plan, 'team');
  assert.equal(u.data.estimated_cents, 0);
  usage = await client.get('/api/v1/usage', { headers });
  assert.equal(usage.data.limit, u.data.included_actions + 1500);

  const me = await client.get('/api/v1/auth/me', { headers });
  assert.equal(me.data.user.plan, 'team');
});

// The trial is one-time: a free org stops with 402 once it is used up, and
// a plan lifts it. Needs CLOUD_TRIAL_ACTIONS <= 5 on the server.
test('free trial stops at its limit, a plan lifts it', { skip: CHARGE_CHECK }, async (t) => {
  const orgId = `org_trial_${RUN_ID}`;
  const agentId = 'agent_trial';
  const cookie = await registerAndVerify(`trial-${RUN_ID}@internal.test`, 'validpassword123', orgId);
  const headers = { Cookie: cookie };
  const u = (await client.get(`/api/v1/billing/usage?org_id=${orgId}`, { headers })).data;
  if (!u.cloud || u.trial_actions > 5) {
    t.skip('needs a cloud-mode server with CLOUD_TRIAL_ACTIONS <= 5');
    return;
  }
  const apiKey = (await client.post('/api/v1/agents/connect', { org_id: orgId, agent_id: agentId }, { headers })).data.api_key;
  const call = (i) => client.post(`/v1/webhook/${orgId}/${agentId}`, { service: 'mockpay', action: 'payment.create', payload: { amount: i, fail: false } }, { headers: { Authorization: `Bearer ${apiKey}` } });
  for (let i = 1; i <= u.trial_actions; i++) assert.equal((await call(i)).status, 200, `call ${i} is inside the trial`);
  const over = await call(999);
  assert.equal(over.status, 402, JSON.stringify(over.data));
  assert.match(JSON.stringify(over.data), /trial is used up/);
  await pg.query("UPDATE users SET plan = 'team' WHERE org_id = $1", [orgId]);
  assert.equal((await call(1000)).status, 200, 'a Team plan has its own monthly volume');
});

// Seeds two Team orgs with last month's actions: one under what the plan
// includes (skipped, nothing owed), one over it (an overage charge is due).
test('seed last month for the charge job', { skip: !BILLING_ON || CHARGE_CHECK }, async () => {
  const { at } = lastMonth();
  for (const [name, actions] of [['under', 9000], ['over', 10600]]) {
    const orgId = `org_paygjob_${name}`;
    await pg.query('DELETE FROM billing_charges WHERE org_id = $1', [orgId]);
    await pg.query('DELETE FROM audit_log WHERE org_id = $1', [orgId]);
    const email = `paygjob-${name}@internal.test`;
    let user = await pg.query('SELECT id FROM users WHERE email = $1', [email]);
    if (!user.rows.length) {
      await registerAndVerify(email, 'validpassword123', orgId);
      user = await pg.query('SELECT id FROM users WHERE email = $1', [email]);
    }
    const userId = user.rows[0].id;
    await pg.query("UPDATE users SET plan = 'team' WHERE id = $1", [userId]);
    await pg.query(
      `INSERT INTO subscriptions (user_id, paddle_subscription_id, paddle_customer_id, status)
       VALUES ($1, $2, 'ctm_test', 'active') ON CONFLICT (paddle_subscription_id) DO UPDATE SET status = 'active'`,
      [userId, `sub_paygjob_${name}`],
    );
    await pg.query(
      `INSERT INTO audit_log (req_id, api_key, org_id, agent_id, service, action, status, duration_ms, created_at)
       SELECT 'r' || g, 'k', $1, 'a', 'stripe', 'charge', 'success', 1, $2 FROM generate_series(1, $3) g`,
      [orgId, at, actions],
    );
    // A blocked call and a duplicate are never billed.
    await pg.query(
      `INSERT INTO audit_log (req_id, api_key, org_id, agent_id, service, action, status, duration_ms, created_at)
       VALUES ('rb', 'k', $1, 'a', 'stripe', 'charge', 'blocked', 1, $2), ('rd', 'k', $1, 'a', 'stripe', 'charge', 'deduplicated', 1, $2)`,
      [orgId, at],
    );
  }
});

test('charge job: claims each month once, skips under a block, never charges without Paddle', { skip: !CHARGE_CHECK }, async () => {
  const { key } = lastMonth();
  const rows = await pg.query("SELECT org_id, actions, blocks, carried_out, amount_cents, status, error FROM billing_charges WHERE org_id LIKE 'org_paygjob_%' AND month = $1 ORDER BY org_id", [key]);
  const by = Object.fromEntries(rows.rows.map((r) => [r.org_id, r]));
  const over = by.org_paygjob_over;
  const under = by.org_paygjob_under;
  assert.ok(over && under, `expected both rows, got ${JSON.stringify(rows.rows)}`);
  assert.equal(Number(under.actions), 9000);
  assert.equal(under.status, 'skipped');
  assert.equal(Number(under.carried_out), 0);
  // 10,600 ran, Team includes 10,000: 600 over = 1 block of 500 ($1), 100 carried.
  assert.equal(Number(over.actions), 10600);
  assert.equal(Number(over.blocks), 1);
  assert.equal(Number(over.amount_cents), 100);
  assert.equal(Number(over.carried_out), 100);
  assert.equal(over.status, 'failed');
  assert.match(over.error, /PADDLE_API_KEY/);
});
