// Savings and reliability reports (crates/api/src/reports.rs): counts,
// money protected per currency (never converted), per-agent rates, top
// duplicated actions, and the CSV/HTML exports. Needs migration 061.

const test = require('node:test');
const assert = require('node:assert/strict');
const axios = require('axios');

const BASE_URL = process.env.TEST_BASE_URL || 'http://localhost:3000';
const client = axios.create({ baseURL: BASE_URL, validateStatus: () => true, maxRedirects: 0 });
const RUN_ID = Date.now();

async function registerAndVerify(email, orgId) {
  const reg = await client.post('/api/v1/auth/register', { email, password: 'validpassword123', org_id: orgId });
  assert.equal(reg.status, 200, JSON.stringify(reg.data));
  const token = new URL(reg.data.dev_verify_url).searchParams.get('verify_token');
  const res = await client.get(`/api/v1/auth/verify-email?token=${token}`);
  return res.headers['set-cookie'][0].split(';')[0];
}

let orgId, cookie;
const agents = {};
const asUser = () => ({ headers: { Cookie: cookie } });
const call = (agent, payload, action = 'payment.create') => client.post(`/v1/webhook/${orgId}/${agent}`,
  { service: 'mockpay', action, payload }, { headers: { Authorization: `Bearer ${agents[agent]}` } });

test.before(async () => {
  orgId = `org_report_${RUN_ID}`;
  cookie = await registerAndVerify(`report-${RUN_ID}@internal.test`, orgId);
  for (const a of ['billing', 'support']) {
    agents[a] = (await client.post('/api/v1/agents/connect', { org_id: orgId, agent_id: a, label: a }, asUser())).data.api_key;
  }
  // billing: 2 payments, one sent three times (2 duplicates in USD), one in INR sent twice.
  const usd = { amount: 25, currency: 'usd', fail: false, n: RUN_ID };
  const inr = { amount: 1200, currency: 'INR', fail: false, n: RUN_ID };
  for (const p of [usd, usd, usd, inr, inr]) assert.equal((await call('billing', p)).status, 200);
  // support: one call refused by a deny policy, one upstream error.
  const deny = await client.post('/api/v1/action-policies',
    { org_id: orgId, agent_id: 'support', service: 'mockpay', action: 'payment.refund', effect: 'deny' }, asUser());
  assert.equal(deny.status, 200, JSON.stringify(deny.data));
  assert.equal((await call('support', { payment: 'mockpay_x' }, 'payment.refund')).status, 403);
  assert.equal((await call('support', { amount: 5, fail: true, n: RUN_ID })).status >= 500, true);
});

const report = (q = '') => client.get(`/api/v1/reports?org_id=${orgId}&period=7d${q}`, asUser());

test('the report counts what was caught, per currency and per agent', async () => {
  const r = await report();
  assert.equal(r.status, 200, JSON.stringify(r.data));
  const { totals, money_protected: money, agents: rows, top_duplicated: top } = r.data;
  assert.equal(totals.actions, 7);
  assert.equal(totals.duplicates_caught, 3);
  assert.equal(totals.blocked_by_policy, 1);
  assert.equal(money.estimate, true);
  assert.deepEqual(money.by_currency, [
    { currency: 'INR', amount: 1200, duplicates: 1 },
    { currency: 'USD', amount: 50, duplicates: 2 },
  ]);
  const billing = rows.find((a) => a.agent_id === 'billing');
  assert.equal(billing.actions, 5);
  assert.equal(billing.duplicate_rate, 60);
  const support = rows.find((a) => a.agent_id === 'support');
  assert.equal(support.block_rate, 50);
  assert.equal(support.error_rate, 100);
  assert.deepEqual(top[0], { service: 'mockpay', action: 'payment.create', duplicates: 3 });
  const svc = r.data.services.find((s) => s.service === 'mockpay');
  assert.equal(svc.actions, 7);
});

test('a calendar month and the exports', async () => {
  const month = new Date().toISOString().slice(0, 7);
  const m = await client.get(`/api/v1/reports?org_id=${orgId}&period=month&month=${month}`, asUser());
  assert.equal(m.status, 200);
  assert.equal(m.data.totals.duplicates_caught, 3);
  const old = await client.get(`/api/v1/reports?org_id=${orgId}&period=month&month=2020-01`, asUser());
  assert.equal(old.data.totals.actions, 0);

  const csv = await report('&format=csv');
  assert.equal(csv.status, 200);
  assert.match(csv.headers['content-type'], /text\/csv/);
  assert.match(csv.headers['content-disposition'], /attachment; filename="agentraas-report-/);
  assert.match(csv.data, /"money_protected_estimate","USD","amount","50"/);
  assert.match(csv.data, /"agents","billing","duplicate_rate","60"/);

  const html = await report('&format=html');
  assert.equal(html.status, 200);
  assert.match(html.headers['content-type'], /text\/html/);
  assert.match(html.data, /^<!doctype html>/);
  assert.doesNotMatch(html.data, /<script/i, 'no scripts in the page');
  assert.match(html.data, /Money protected \(estimate\)/);
  assert.match(html.data, /AgentRaaS/);
});

test('bad input and other orgs are refused', async () => {
  assert.equal((await client.get(`/api/v1/reports?org_id=${orgId}&period=1y`, asUser())).status, 400);
  assert.equal((await report('&format=pdf')).status, 400);
  const other = await registerAndVerify(`report-other-${RUN_ID}@internal.test`, `org_report_other_${RUN_ID}`);
  assert.equal((await client.get(`/api/v1/reports?org_id=${orgId}`, { headers: { Cookie: other } })).status, 403);
});

test('the monthly email is opt-in with real addresses', async () => {
  const off = await client.get(`/api/v1/reports/settings?org_id=${orgId}`, asUser());
  assert.equal(off.status, 200);
  assert.equal(off.data.enabled, false);
  const empty = await client.put('/api/v1/reports/settings', { org_id: orgId, enabled: true, recipients: [] }, asUser());
  assert.equal(empty.status, 422);
  const bad = await client.put('/api/v1/reports/settings', { org_id: orgId, enabled: true, recipients: ['not-an-address'] }, asUser());
  assert.equal(bad.status, 422);
  const on = await client.put('/api/v1/reports/settings', { org_id: orgId, enabled: true, recipients: ['client@example.com'] }, asUser());
  assert.equal(on.status, 200, JSON.stringify(on.data));
  const now = await client.get(`/api/v1/reports/settings?org_id=${orgId}`, asUser());
  assert.deepEqual([now.data.enabled, now.data.recipients], [true, ['client@example.com']]);
});
