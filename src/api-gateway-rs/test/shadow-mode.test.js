// Shadow mode (crates/api/src/shadow.rs): an agent in shadow mode gets a
// marked placeholder answer and its writes never reach the provider. Proved
// with mockpay's hit counter (mockpay:hits:<tag> in Redis), across all three
// entry points, concurrent duplicates, retries and a call that would time
// out. Needs migration 061 and REDIS_URL pointing at the stack's Redis.

const test = require('node:test');
const assert = require('node:assert/strict');
const axios = require('axios');
const Redis = require('ioredis');
const { Client } = require('pg');

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

let pg, orgId, agentId, cookie, key, capId;
test.before(async () => {
  pg = new Client({ connectionString: process.env.DATABASE_URL || 'postgres://agentraas:agentraas@localhost:5432/agentraas' });
  await pg.connect();
  orgId = `org_shadow_${RUN_ID}`;
  agentId = `agent_shadow_${RUN_ID}`;
  cookie = await registerAndVerify(`shadow-${RUN_ID}@internal.test`, orgId);
  key = (await client.post('/api/v1/agents/connect', { org_id: orgId, agent_id: agentId, label: 'shadow test' }, { headers: { Cookie: cookie } })).data.api_key;
});
test.after(async () => { await pg.end(); redis.disconnect(); });

const asUser = () => ({ headers: { Cookie: cookie } });
const hits = async (tag) => Number((await redis.get(`mockpay:hits:${tag}`)) || 0);
const shadowOn = (body = {}) => client.post('/api/v1/shadow-rules', { org_id: orgId, agent_id: agentId, ...body }, asUser());
const promote = () => client.post('/api/v1/shadow-rules/promote', { org_id: orgId, agent_id: agentId }, asUser());
const webhook = (service, action, payload, headers = {}) =>
  client.post(`/v1/webhook/${orgId}/${agentId}`, { service, action, payload }, { headers: { Authorization: `Bearer ${key}`, ...headers } });
const sdk = (payload) => client.post('/v1/sdk/mockpay/payment.create', payload,
  { headers: { 'x-agentraas-key': key, 'x-agentraas-org': orgId, 'x-agentraas-agent': agentId } });
const mcp = async (payload) => {
  const res = await client.post('/mcp',
    { jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'mockpay_payment_create', arguments: { org_id: orgId, agent_id: agentId, payload } } },
    { headers: { 'x-agentraas-key': key } });
  assert.equal(res.status, 200, JSON.stringify(res.data));
  return JSON.parse(res.data.result.content[0].text);
};
const auditStatuses = async (reqIds) => (await pg.query(
  'SELECT status, error_type FROM audit_log WHERE req_id = ANY($1)', [reqIds])).rows;

test('writes in shadow mode never reach the provider, on every entry point', async () => {
  const set = await shadowOn({ stub: { id: 'stub_payment', status: 'pretend' } });
  assert.equal(set.status, 200, JSON.stringify(set.data));
  const tag = `w_${RUN_ID}`;
  const payload = { amount: 40, fail: false, hit_tag: tag };

  // Five identical calls at once, then retries, then the other two entry points.
  const burst = await Promise.all(Array.from({ length: 5 }, () => webhook('mockpay', 'payment.create', payload)));
  const retries = [await webhook('mockpay', 'payment.create', payload), await webhook('mockpay', 'payment.create', payload)];
  const viaSdk = await sdk(payload);
  const viaMcp = await mcp(payload);

  for (const r of [...burst, ...retries, viaSdk]) {
    assert.equal(r.status, 200, JSON.stringify(r.data));
    assert.equal(r.data.shadow, true);
    assert.equal(r.data.decision, 'would_execute');
    assert.deepEqual(r.data.response, { id: 'stub_payment', status: 'pretend' });
  }
  assert.equal(viaMcp.shadow, true);
  assert.equal(await hits(tag), 0, 'the provider was never called');

  const rows = await auditStatuses([...burst, ...retries, viaSdk].map((r) => r.data.reqId).concat(viaMcp.reqId));
  assert.equal(rows.length, 9);
  assert.ok(rows.every((r) => r.status === 'shadow'), JSON.stringify(rows));
});

test('a call that would time out is not sent either', async () => {
  const tag = `slow_${RUN_ID}`;
  const started = Date.now();
  const r = await webhook('mockpay', 'payment.create', { amount: 41, fail: false, delay_ms: 5000, hit_tag: tag });
  assert.equal(r.status, 200, JSON.stringify(r.data));
  assert.equal(r.data.shadow, true);
  assert.ok(Date.now() - started < 2000, 'answered without waiting on the provider');
  assert.equal(await hits(tag), 0);
});

test('a provider without credentials still gets a shadow answer: nothing is decrypted', async () => {
  const r = await webhook('stripe', 'charge.create', { amount: 500, currency: 'usd', source: 'tok_visa' });
  assert.equal(r.status, 200, JSON.stringify(r.data));
  assert.equal(r.data.shadow, true);
});

test('gates are evaluated without using up a live window', async () => {
  const cap = await client.post('/api/v1/spend-cap-rules',
    { org_id: orgId, agent_id: agentId, service: 'mockpay', action: 'payment.create', window: 'hour', max_calls: 1, on_exceed: 'block' }, asUser());
  assert.equal(cap.status, 200, JSON.stringify(cap.data));
  capId = cap.data.id;
  for (const n of [1, 2, 3]) {
    const r = await webhook('mockpay', 'payment.create', { amount: 50 + n, fail: false });
    assert.equal(r.data.decision, 'would_execute', 'a peek never counts the call');
  }
  const invalid = await webhook('mockpay', 'payment.create', { amount: 0, fail: false });
  assert.equal(invalid.data.decision, 'would_block:validation_failed');

  const deny = await client.post('/api/v1/action-policies',
    { org_id: orgId, agent_id: agentId, service: 'mockpay', action: 'payment.refund', effect: 'deny' }, asUser());
  assert.equal(deny.status, 200, JSON.stringify(deny.data));
  const refused = await webhook('mockpay', 'payment.refund', { payment: 'mockpay_x' });
  assert.equal(refused.status, 200);
  assert.match(refused.data.decision, /^would_block:policy/);

  const listed = await client.get(`/api/v1/shadow-calls?org_id=${orgId}&decision=would_block`, asUser());
  assert.equal(listed.status, 200);
  assert.ok(listed.data.calls.length >= 2);
  assert.ok(listed.data.calls.every((c) => c.decision.startsWith('would_block')));
});

test('reads are forwarded unless the rule blocks them', async () => {
  const order = `order_${RUN_ID}7`;
  const read = await webhook('mockpay', 'order.get', { order_id: order });
  assert.equal(read.status, 200, JSON.stringify(read.data));
  assert.notEqual(read.data.shadow, true);
  assert.equal(await hits(order), 1);

  const narrow = await shadowOn({ service: 'mockpay', action: 'order.get', block_reads: true });
  assert.equal(narrow.status, 200, JSON.stringify(narrow.data));
  const blocked = await webhook('mockpay', 'order.get', { order_id: order });
  assert.equal(blocked.data.shadow, true);
  assert.equal(await hits(order), 1, 'still only the first read');
});

test('promote to live replays nothing and the next call runs for real', async () => {
  const tag = `promote_${RUN_ID}`;
  const payload = { amount: 77, fail: false, hit_tag: tag };
  const before = await webhook('mockpay', 'payment.create', payload);
  assert.equal(before.data.shadow, true);

  const p = await promote();
  assert.equal(p.status, 200, JSON.stringify(p.data));
  assert.equal(p.data.replayed, 0);
  assert.ok(p.data.rules_removed >= 1);
  assert.equal(await hits(tag), 0, 'promoting sent nothing');

  // The shadow call claimed no dedup slot, so the same payload runs live
  // once, and the cap of 1 call still has room: the shadow calls never counted.
  const live = await webhook('mockpay', 'payment.create', payload);
  assert.equal(live.status, 200, JSON.stringify(live.data));
  assert.equal(live.data.shadow, undefined);
  assert.notEqual(live.data.cached, true);
  assert.equal(await hits(tag), 1);
  await client.delete(`/api/v1/spend-cap-rules/${capId}`, asUser());
});

test('a live duplicate is reported as one in shadow mode', async () => {
  const tag = `dup_${RUN_ID}`;
  const payload = { amount: 88, fail: false, hit_tag: tag };
  const live = await webhook('mockpay', 'payment.create', payload);
  assert.equal(live.status, 200, JSON.stringify(live.data));
  await shadowOn();
  const shadow = await webhook('mockpay', 'payment.create', payload);
  assert.equal(shadow.data.decision, 'would_deduplicate');
  assert.equal(await hits(tag), 1);
  await promote();
});

test('only members manage shadow rules', async () => {
  const other = await registerAndVerify(`shadow-other-${RUN_ID}@internal.test`, `org_shadow_other_${RUN_ID}`);
  const r = await client.post('/api/v1/shadow-rules', { org_id: orgId, agent_id: agentId }, { headers: { Cookie: other } });
  assert.equal(r.status, 403);
  const calls = await client.get(`/api/v1/shadow-calls?org_id=${orgId}`, { headers: { Cookie: other } });
  assert.equal(calls.status, 403);
});
