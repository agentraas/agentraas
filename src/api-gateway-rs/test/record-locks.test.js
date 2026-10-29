// Automatic record locks (crates/api/src/record_locks.rs): with the org's
// switch on, two agents changing the same record (mockpay "customer") at the
// same moment: one runs, the other gets 409 naming the holder. Different
// records, the switch off, or a payload without the record all run as before.
// Shadow calls report a held lock and never take one. Needs migration 062
// and REDIS_URL (mockpay hit counter).

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
const pay = (agent, payload) => client.post(`/v1/webhook/${orgId}/${agent}`,
  { service: 'mockpay', action: 'payment.create', payload: { fail: false, ...payload } },
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

test('off by default, and the setting lists the locked actions', async () => {
  const s = await client.get(`/api/v1/record-locks?org_id=${orgId}`, asUser());
  assert.equal(s.status, 200, JSON.stringify(s.data));
  assert.equal(s.data.enabled, false);
  assert.ok(s.data.actions.includes('stripe.charge.create'));
  const customer = `cus_off_${RUN_ID}`;
  const [a, b] = await Promise.all([
    pay('billing', { amount: 10, customer, delay_ms: 600 }),
    pay('support', { amount: 11, customer }),
  ]);
  assert.deepEqual([a.status, b.status], [200, 200]);
});

test('two agents, same customer, at once: one runs, the other gets 409', async () => {
  assert.equal((await setLocks(true)).status, 200);
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

test('only members change the setting', async () => {
  const other = await registerAndVerify(`locks-other-${RUN_ID}@internal.test`, `org_locks_other_${RUN_ID}`);
  const r = await client.put('/api/v1/record-locks', { org_id: orgId, enabled: false }, { headers: { Cookie: other } });
  assert.equal(r.status, 403);
});
