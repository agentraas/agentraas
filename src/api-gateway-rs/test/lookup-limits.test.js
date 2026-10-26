// Action policy max_from: a limit read from the provider before the call runs
// ("refund at most half the order"), so the agent can't supply the number it
// is checked against. Uses mockpay's stateless order.get (an order id ends in
// its amount: order_1200 is an order of 1200).

const test = require('node:test');
const assert = require('node:assert/strict');
const axios = require('axios');

const BASE_URL = process.env.TEST_BASE_URL || 'http://localhost:3000';
const client = axios.create({ baseURL: BASE_URL, validateStatus: () => true });
const RUN_ID = Date.now();

async function setup() {
  const email = `lookup-${RUN_ID}@internal.test`;
  const orgId = `org_lookup_${RUN_ID}`;
  const reg = await client.post('/api/v1/auth/register', { email, password: 'validpassword123', org_id: orgId });
  assert.equal(reg.status, 200, JSON.stringify(reg.data));
  const token = new URL(reg.data.dev_verify_url).searchParams.get('verify_token');
  const verified = await client.get(`/api/v1/auth/verify-email?token=${token}`);
  const cookie = verified.headers['set-cookie'][0].split(';')[0];
  const key = (await client.post('/api/v1/agents/connect', { org_id: orgId, agent_id: 'support' }, { headers: { Cookie: cookie } })).data.api_key;
  return { orgId, cookie, key };
}

test('a refund is checked against the order amount the server looks up itself', async () => {
  const { orgId, cookie, key } = await setup();
  const policy = (fields) => client.post('/api/v1/action-policies', { org_id: orgId, service: 'mockpay', action: 'payment.refund', effect: 'require', fields }, { headers: { Cookie: cookie } });

  const bad = await policy({ amount: { max_from: { with: { order_id: 'order_id' }, field: 'order_amount' } } });
  assert.equal(bad.status, 422, 'a max_from without an action is refused when saved');

  const saved = await policy({ amount: { max_from: { action: 'order.get', with: { order_id: 'order_id' }, field: 'order_amount', factor: 0.5 } } });
  assert.equal(saved.status, 200, JSON.stringify(saved.data));

  const refund = (payload) => client.post(`/v1/webhook/${orgId}/support`, { service: 'mockpay', action: 'payment.refund', payload: { payment: `mockpay_${Math.random()}`, ...payload } }, { headers: { Authorization: `Bearer ${key}` } });

  const within = await refund({ order_id: 'order_1200', amount: 600 });
  assert.equal(within.status, 200, `half of a 1200 order is allowed: ${JSON.stringify(within.data)}`);

  const over = await refund({ order_id: 'order_1200', amount: 700 });
  assert.equal(over.status, 403, JSON.stringify(over.data));
  assert.match(over.data.error, /amount 700 is more than the 600 allowed \(50% of order_amount, read from mockpay\.order\.get\)/);

  const noAmount = await refund({ order_id: 'order_1200' });
  assert.equal(noAmount.status, 403, 'a refund with no amount (a full refund) is not waved through');
  assert.match(noAmount.data.error, /amount is missing/);

  const noOrder = await refund({ order_id: 'order_none', amount: 10 });
  assert.equal(noOrder.status, 403, 'a lookup that fails blocks the call');
  assert.match(noOrder.data.error, /Couldn't check amount against mockpay\.order\.get/);

  const noId = await refund({ amount: 10 });
  assert.equal(noId.status, 403);
  assert.match(noId.data.error, /order_id is missing/);
});
