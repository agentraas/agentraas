// Approval links (ee/hitl.rs): a held call gets a one-time link, delivered
// to the org's alert channels. Opening the page never decides; only the
// POST to /v1/hitl/link/decide does, once.
//
// The real token only exists in the sent message, so the test swaps the
// stored hash for one of a token it knows (same shortcut as hitl.test.js's
// setPlan). Needs TEST_BASE_URL and DATABASE_URL; see test/run-each.sh.

const test = require('node:test');
const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const axios = require('axios');
const { Client } = require('pg');

const BASE_URL = process.env.TEST_BASE_URL || 'http://localhost:3000';
const client = axios.create({ baseURL: BASE_URL, validateStatus: () => true });
const RUN_ID = Date.now();
const sha256 = (s) => crypto.createHash('sha256').update(s).digest('hex');

let pg, cookie, orgId, agentId, apiKey;

test.before(async () => {
  pg = new Client({ connectionString: process.env.DATABASE_URL || 'postgres://agentraas:agentraas@localhost:5432/agentraas' });
  await pg.connect();
  orgId = `org_links_${RUN_ID}`;
  agentId = `agent_links_${RUN_ID}`;
  const reg = await client.post('/api/v1/auth/register', { email: `links-${RUN_ID}@internal.test`, password: 'validpassword123', org_id: orgId });
  assert.equal(reg.status, 200, JSON.stringify(reg.data));
  const verify = await client.get(`/api/v1/auth/verify-email?token=${new URL(reg.data.dev_verify_url).searchParams.get('verify_token')}`);
  cookie = verify.headers['set-cookie'][0].split(';')[0];
  const rule = await client.post('/api/v1/hitl-rules', { org_id: orgId, service: 'mockpay', action: 'payment.create' }, { headers: { Cookie: cookie } });
  assert.equal(rule.status, 200, JSON.stringify(rule.data));
  const conn = await client.post('/api/v1/agents/connect', { org_id: orgId, agent_id: agentId, label: 'links test' }, { headers: { Cookie: cookie } });
  assert.equal(conn.status, 200, JSON.stringify(conn.data));
  apiKey = conn.data.api_key;
});
test.after(async () => { await pg.end(); });

// Holds a call and returns [reqId, a token that opens its link].
async function holdCall(amount) {
  const res = await client.post(`/v1/webhook/${orgId}/${agentId}`,
    { service: 'mockpay', action: 'payment.create', payload: { amount, fail: false } },
    { headers: { Authorization: `Bearer ${apiKey}` } });
  assert.equal(res.status, 202, JSON.stringify(res.data));
  const { rows } = await pg.query('SELECT approval_token_hash FROM hitl_requests WHERE req_id = $1', [res.data.reqId]);
  assert.match(rows[0].approval_token_hash || '', /^[0-9a-f]{64}$/, 'a held call stores a link token hash');
  const token = crypto.randomBytes(32).toString('hex');
  await pg.query('UPDATE hitl_requests SET approval_token_hash = $1 WHERE req_id = $2', [sha256(token), res.data.reqId]);
  return [res.data.reqId, token];
}

test('email and Teams are alert channels; a bad address is refused', async () => {
  const ok = await client.post('/api/v1/notification-webhooks', { org_id: orgId, type: 'email', target: 'Oncall@Example.com' }, { headers: { Cookie: cookie } });
  assert.equal(ok.status, 200, JSON.stringify(ok.data));
  const bad = await client.post('/api/v1/notification-webhooks', { org_id: orgId, type: 'email', target: 'not-an-email' }, { headers: { Cookie: cookie } });
  assert.equal(bad.status, 422);
  const teamsLocal = await client.post('/api/v1/notification-webhooks', { org_id: orgId, type: 'teams', target: 'http://127.0.0.1/hook' }, { headers: { Cookie: cookie } });
  assert.equal(teamsLocal.status, 422, 'a Teams URL goes through the same SSRF check as Slack and Discord');
});

test('the page loads without deciding; approve runs the action once', async () => {
  const [reqId, token] = await holdCall(4200);

  const page = await client.get('/approve');
  assert.equal(page.status, 200);
  assert.match(page.data, /js\/approve\.js/);

  const view = await client.post('/v1/hitl/link', { token });
  assert.equal(view.status, 200, JSON.stringify(view.data));
  assert.equal(view.data.status, 'pending');
  assert.equal(view.data.service, 'mockpay');
  assert.equal(view.data.payload.amount, 4200);

  const yes = await client.post('/v1/hitl/link/decide', { token, decision: 'approve' });
  assert.equal(yes.status, 200, JSON.stringify(yes.data));
  assert.equal(yes.data.status, 'approved');
  const { rows } = await pg.query('SELECT resolved_by FROM hitl_requests WHERE req_id = $1', [reqId]);
  assert.equal(rows[0].resolved_by, 'approval link');

  const again = await client.post('/v1/hitl/link/decide', { token, decision: 'approve' });
  assert.equal(again.status, 409, 'a used link cannot approve twice');
});

test('deny leaves the action unrun', async () => {
  const [, token] = await holdCall(4300);
  const no = await client.post('/v1/hitl/link/decide', { token, decision: 'deny' });
  assert.equal(no.status, 200, JSON.stringify(no.data));
  assert.equal(no.data.status, 'denied');
});

test('unknown, malformed and expired links all answer 404', async () => {
  const [reqId, token] = await holdCall(4400);
  assert.equal((await client.post('/v1/hitl/link', { token: 'ab'.repeat(32) })).status, 404);
  assert.equal((await client.post('/v1/hitl/link', { token: 'short' })).status, 404);
  assert.equal((await client.post('/v1/hitl/link/decide', { token, decision: 'maybe' })).status, 422);
  await pg.query("UPDATE hitl_requests SET created_at = NOW() - INTERVAL '8 days' WHERE req_id = $1", [reqId]);
  assert.equal((await client.post('/v1/hitl/link', { token })).status, 404);
  assert.equal((await client.post('/v1/hitl/link/decide', { token, decision: 'approve' })).status, 404);
});

test('the dashboard lists held calls and decides them, once, for members only', async () => {
  const res = await client.post(`/v1/webhook/${orgId}/${agentId}`,
    { service: 'mockpay', action: 'payment.create', payload: { amount: 4500, fail: false } },
    { headers: { Authorization: `Bearer ${apiKey}` } });
  assert.equal(res.status, 202, JSON.stringify(res.data));
  const reqId = res.data.reqId;

  const pending = await client.get('/api/v1/hitl/pending', { headers: { Cookie: cookie } });
  assert.equal(pending.status, 200);
  assert.ok(pending.data.pending.some((p) => p.req_id === reqId && p.payload.amount === 4500));

  const outsider = await client.post('/api/v1/auth/register', { email: `links-out-${RUN_ID}@internal.test`, password: 'validpassword123', org_id: `org_links_out_${RUN_ID}` });
  const outVerify = await client.get(`/api/v1/auth/verify-email?token=${new URL(outsider.data.dev_verify_url).searchParams.get('verify_token')}`);
  const outCookie = outVerify.headers['set-cookie'][0].split(';')[0];
  assert.equal((await client.post(`/api/v1/hitl/${reqId}/decide`, { decision: 'approve' }, { headers: { Cookie: outCookie } })).status, 404);
  assert.ok(!(await client.get('/api/v1/hitl/pending', { headers: { Cookie: outCookie } })).data.pending.some((p) => p.req_id === reqId));

  const yes = await client.post(`/api/v1/hitl/${reqId}/decide`, { decision: 'approve' }, { headers: { Cookie: cookie } });
  assert.equal(yes.status, 200, JSON.stringify(yes.data));
  assert.equal(yes.data.status, 'approved');
  const { rows } = await pg.query('SELECT resolved_by FROM hitl_requests WHERE req_id = $1', [reqId]);
  assert.match(rows[0].resolved_by, /^dashboard: links-/);
  assert.equal((await client.post(`/api/v1/hitl/${reqId}/decide`, { decision: 'deny' }, { headers: { Cookie: cookie } })).status, 409);
});
