// Trading (Alpaca) and numeric limits: the alpaca_paper service validates
// orders before anything is sent, and a policy's max holds even when the
// number arrives as a string (Alpaca sends qty and notional as strings).
// Nothing here reaches Alpaca: every call is refused before the forward.
//
// Run with the server already up: node --test test/trading.test.js

const test = require('node:test');
const assert = require('node:assert/strict');
const axios = require('axios');

const BASE_URL = process.env.TEST_BASE_URL || 'http://localhost:3000';
const client = axios.create({ baseURL: BASE_URL, validateStatus: () => true });
const RUN_ID = Date.now();

let orgId, agentId, cookie, apiKey;

test.before(async () => {
  orgId = `org_trading_${RUN_ID}`;
  agentId = `agent_trader_${RUN_ID}`;
  const reg = await client.post('/api/v1/auth/register', { email: `trading-${RUN_ID}@internal.test`, password: 'validpassword123', org_id: orgId });
  assert.equal(reg.status, 200, JSON.stringify(reg.data));
  const verify = await client.get(`/api/v1/auth/verify-email?token=${new URL(reg.data.dev_verify_url).searchParams.get('verify_token')}`);
  cookie = verify.headers['set-cookie'][0].split(';')[0];
  const conn = await client.post('/api/v1/agents/connect', { org_id: orgId, agent_id: agentId, label: 'trading test' }, { headers: { Cookie: cookie } });
  assert.equal(conn.status, 200, JSON.stringify(conn.data));
  apiKey = conn.data.api_key;
});

const call = (service, action, payload) =>
  client.post(`/v1/webhook/${orgId}/${agentId}`, { service, action, payload }, { headers: { Authorization: `Bearer ${apiKey}` } });
const policy = async (body) => {
  const res = await client.post('/api/v1/action-policies', { org_id: orgId, ...body }, { headers: { Cookie: cookie } });
  assert.equal(res.status, 200, JSON.stringify(res.data));
};

test('alpaca_paper takes a key id and secret, and checks an order before sending it', async () => {
  const saved = await client.post('/api/v1/credentials',
    { org_id: orgId, service: 'alpaca_paper', credentials: { username: 'PKTEST', password: 'secret-test' } },
    { headers: { Cookie: cookie } });
  assert.equal(saved.status, 200, JSON.stringify(saved.data));

  const bad = await call('alpaca_paper', 'order.create', { symbol: 'AAPL', qty: '1', side: 'hold', type: 'market', time_in_force: 'day' });
  assert.equal(bad.status, 422, JSON.stringify(bad.data));
  assert.match(JSON.stringify(bad.data), /side must be one of: buy, sell/);
});

test('a quoted number cannot walk past a policy limit', async () => {
  await policy({ service: 'alpaca_paper', action: 'order.create', effect: 'require', fields: { notional: { max: 1000 }, symbol: { enum: ['AAPL', 'SPY'] } } });
  const order = { symbol: 'AAPL', side: 'buy', type: 'market', time_in_force: 'day' };
  const big = await call('alpaca_paper', 'order.create', { ...order, notional: '5000' });
  assert.equal(big.status, 403, JSON.stringify(big.data));
  const offList = await call('alpaca_paper', 'order.create', { ...order, symbol: 'GME', notional: '100' });
  assert.equal(offList.status, 403, JSON.stringify(offList.data));

  // The same fix on a service that runs locally: the quoted value under the
  // limit goes through, over it is refused.
  await policy({ service: 'mockpay', action: 'payment.create', effect: 'require', fields: { tip: { max: 100 } } });
  const over = await call('mockpay', 'payment.create', { amount: 10, tip: '500', fail: false });
  assert.equal(over.status, 403, JSON.stringify(over.data));
  const under = await call('mockpay', 'payment.create', { amount: 10, tip: '50', fail: false });
  assert.equal(under.status, 200, JSON.stringify(under.data));
});

test('a policy can keep an agent from closing every position at once', async () => {
  await policy({ agent_id: agentId, service: 'alpaca_paper', action: 'positions.close_all', effect: 'deny' });
  const res = await call('alpaca_paper', 'positions.close_all', {});
  assert.equal(res.status, 403, JSON.stringify(res.data));
});

test('an approval rule holds a call whose number arrives as a string', async () => {
  const rule = await client.post('/api/v1/hitl-rules',
    { org_id: orgId, service: 'mockpay', action: 'payment.create', field: 'fee', operator: 'gt', threshold: 100 },
    { headers: { Cookie: cookie } });
  assert.equal(rule.status, 200, JSON.stringify(rule.data));
  const held = await call('mockpay', 'payment.create', { amount: 11, fee: '500', fail: false });
  assert.equal(held.status, 202, JSON.stringify(held.data));
  assert.equal(held.data.pending_approval, true);
  const small = await call('mockpay', 'payment.create', { amount: 12, fee: '50', fail: false });
  assert.equal(small.status, 200, JSON.stringify(small.data));
});
