// Automatic record locks (crates/api/src/record_locks.rs), on by default:
// two agents changing the same record (mockpay "customer") at the same
// moment: one runs, the other gets 409 naming the holder, or waits for it
// with lock_wait_ms. Different records, the switch off, or a payload without
// the record all run as before. Shadow calls report a held lock and never
// take one. A held approval learns that its record changed meanwhile.
// Custom actions can name their record. Needs migrations 062-063 and
// REDIS_URL (mockpay hit counter).

const test = require('node:test');
const assert = require('node:assert/strict');
const axios = require('axios');
const Redis = require('ioredis');

const BASE_URL = process.env.TEST_BASE_URL || 'http://localhost:3000';
const client = axios.create({ baseURL: BASE_URL, validateStatus: () => true, maxRedirects: 0 });
const redis = new Redis(process.env.REDIS_URL || 'redis://localhost:6379');
const RUN_ID = Date.now();

async function registerAndVerify(email, orgId) {
  const reg = await client.post('/api/v1/auth/register', { email, password: 'validpassword123', org_id: orgId });
  assert.equal(reg.status, 200, JSON.stringify(reg.data));
  const token = new URL(reg.data.dev_verify_url).searchParams.get('verify_token');
  const res = await client.get(`/api/v1/auth/verify-email?token=${token}`);
  return res.headers['set-cookie'][0].split(';')[0];
}

let orgId, cookie;
const keys = {};
const asUser = () => ({ headers: { Cookie: cookie } });
const hits = async (tag) => Number((await redis.get(`mockpay:hits:${tag}`)) || 0);
const pay = (agent, payload, extra = {}) => client.post(`/v1/webhook/${orgId}/${agent}`,
  { service: 'mockpay', action: 'payment.create', payload: { fail: false, ...payload }, ...extra },
  { headers: { Authorization: `Bearer ${keys[agent]}` } });
const setLocks = (enabled) => client.put('/api/v1/record-locks', { org_id: orgId, enabled }, asUser());

test.before(async () => {
  orgId = `org_locks_${RUN_ID}`;
  cookie = await registerAndVerify(`locks-${RUN_ID}@internal.test`, orgId);
  for (const a of ['billing', 'support', 'trial']) {
    keys[a] = (await client.post('/api/v1/agents/connect', { org_id: orgId, agent_id: a, label: a }, asUser())).data.api_key;
  }
});
test.after(() => redis.disconnect());

test('on by default, and the setting lists the locked actions', async () => {
  const s = await client.get(`/api/v1/record-locks?org_id=${orgId}`, asUser());
  assert.equal(s.status, 200, JSON.stringify(s.data));
  assert.equal(s.data.enabled, true);
  assert.ok(s.data.actions.includes('stripe.charge.create'));
});

test('two agents, same customer, at once: one runs, the other gets 409', async () => {
  const customer = `cus_same_${RUN_ID}`;
  const slow = pay('billing', { amount: 20, customer, delay_ms: 1500, hit_tag: `a_${RUN_ID}` });
  await new Promise((r) => setTimeout(r, 300));
  const second = await pay('support', { amount: 21, customer, hit_tag: `b_${RUN_ID}` });
  const first = await slow;
  assert.equal(first.status, 200, JSON.stringify(first.data));
  assert.equal(second.status, 409, JSON.stringify(second.data));
  assert.equal(second.data.code, 'resource_locked');
  assert.equal(second.data.held_by_agent, 'billing');
  assert.ok(second.data.retry_after_ms > 0);
  assert.match(second.data.error, /mockpay customer/);
  assert.equal(await hits(`b_${RUN_ID}`), 0, 'the refused call never reached the provider');

  // The lock is gone once the first call finished, so a retry runs.
  const retry = await pay('support', { amount: 21, customer, hit_tag: `b_${RUN_ID}` });
  assert.equal(retry.status, 200, JSON.stringify(retry.data));
});

test('lock_wait_ms waits for the record instead of refusing', async () => {
  const customer = `cus_wait_${RUN_ID}`;
  const slow = pay('billing', { amount: 25, customer, delay_ms: 800 });
  await new Promise((r) => setTimeout(r, 200));
  const waited = await pay('support', { amount: 26, customer, hit_tag: `w_${RUN_ID}` }, { lock_wait_ms: 4000 });
  assert.equal((await slow).status, 200);
  assert.equal(waited.status, 200, JSON.stringify(waited.data));
  assert.equal(await hits(`w_${RUN_ID}`), 1);
});

test('different customers, or no customer named, run side by side', async () => {
  const [a, b] = await Promise.all([
    pay('billing', { amount: 30, customer: `cus_x_${RUN_ID}`, delay_ms: 600 }),
    pay('support', { amount: 31, customer: `cus_y_${RUN_ID}` }),
  ]);
  assert.deepEqual([a.status, b.status], [200, 200]);
  const [c, d] = await Promise.all([pay('billing', { amount: 32, delay_ms: 600 }), pay('support', { amount: 33 })]);
  assert.deepEqual([c.status, d.status], [200, 200]);
});

test('a shadow call reports the held lock and takes none', async () => {
  const shadow = await client.post('/api/v1/shadow-rules', { org_id: orgId, agent_id: 'trial' }, asUser());
  assert.equal(shadow.status, 200, JSON.stringify(shadow.data));
  const customer = `cus_shadow_${RUN_ID}`;
  const slow = pay('billing', { amount: 40, customer, delay_ms: 1200 });
  await new Promise((r) => setTimeout(r, 300));
  const s = await pay('trial', { amount: 41, customer });
  assert.equal(s.data.shadow, true);
  assert.equal(s.data.decision, 'would_wait_for_lock');
  assert.equal((await slow).status, 200);

  const idle = await pay('trial', { amount: 42, customer: `cus_idle_${RUN_ID}`, delay_ms: 2000 });
  assert.equal(idle.data.decision, 'would_execute');
  const live = await pay('support', { amount: 43, customer: `cus_idle_${RUN_ID}` });
  assert.equal(live.status, 200, 'the shadow call left no lock behind');
});

test('a held approval shows that its record changed after it was held', async () => {
  const rule = await client.post('/api/v1/hitl-rules',
    { org_id: orgId, service: 'mockpay', action: 'payment.create', field: 'amount', operator: 'gt', threshold: 1000 }, asUser());
  assert.equal(rule.status, 200, JSON.stringify(rule.data));
  const customer = `cus_held_${RUN_ID}`;
  const held = await pay('billing', { amount: 5000, customer });
  assert.equal(held.status, 202, JSON.stringify(held.data));
  const row = async () => (await client.get('/api/v1/hitl/pending', asUser())).data.pending.find((p) => p.req_id === held.data.reqId);
  assert.equal((await row()).record_changed, false);
  assert.equal((await pay('support', { amount: 12, customer })).status, 200, 'a held call holds no lock');
  assert.equal((await row()).record_changed, true);
  await client.post(`/api/v1/hitl/${held.data.reqId}/decide`, { decision: 'deny' }, asUser());
});

test('a custom action can name its record', async () => {
  const base = { org_id: orgId, method: 'POST', target_url: 'https://example.com/orders', auth_type: 'none' };
  const ok = await client.post('/api/v1/custom-actions', { ...base, name: `order_update_${RUN_ID}`, lock: { record: 'order', from: ['order_id'] } }, asUser());
  assert.equal(ok.status, 200, JSON.stringify(ok.data));
  const listed = (await client.get('/api/v1/custom-actions', asUser())).data.find((a) => a.name === `order_update_${RUN_ID}`);
  assert.deepEqual(listed.lock, { record: 'order', from: ['order_id'] });
  const bad = await client.post('/api/v1/custom-actions', { ...base, name: `order_bad_${RUN_ID}`, lock: { record: 'an order', from: [] } }, asUser());
  assert.equal(bad.status, 422);
});

test('turned off, two calls on one customer both run', async () => {
  assert.equal((await setLocks(false)).status, 200);
  assert.equal((await client.get(`/api/v1/record-locks?org_id=${orgId}`, asUser())).data.enabled, false);
  const customer = `cus_off_${RUN_ID}`;
  const [a, b] = await Promise.all([
    pay('billing', { amount: 10, customer, delay_ms: 600 }),
    pay('support', { amount: 11, customer }),
  ]);
  assert.deepEqual([a.status, b.status], [200, 200]);
});

test('only members change the setting', async () => {
  const other = await registerAndVerify(`locks-other-${RUN_ID}@internal.test`, `org_locks_other_${RUN_ID}`);
  const r = await client.put('/api/v1/record-locks', { org_id: orgId, enabled: false }, { headers: { Cookie: other } });
  assert.equal(r.status, 403);
});
