// Money caps on spend caps (sum of a payload field per window, in a time
// zone) and allowed hours on action policies.
//
// Run with the server already up: node --test test/limits-and-hours.test.js

const test = require('node:test');
const assert = require('node:assert/strict');
const axios = require('axios');

const BASE_URL = process.env.TEST_BASE_URL || 'http://localhost:3000';
const client = axios.create({ baseURL: BASE_URL, validateStatus: () => true });
const RUN_ID = Date.now();
const DAYS = ['sun', 'mon', 'tue', 'wed', 'thu', 'fri', 'sat'];

let orgId, cookie, key;
const agentId = 'my-agent';

test.before(async () => {
  orgId = `org_limits_${RUN_ID}`;
  const reg = await client.post('/api/v1/auth/register', { email: `limits-${RUN_ID}@internal.test`, password: 'validpassword123', org_id: orgId });
  assert.equal(reg.status, 200, JSON.stringify(reg.data));
  const verify = await client.get(`/api/v1/auth/verify-email?token=${new URL(reg.data.dev_verify_url).searchParams.get('verify_token')}`);
  cookie = { headers: { Cookie: verify.headers['set-cookie'][0].split(';')[0] } };
  key = (await client.post('/api/v1/agents/connect', { org_id: orgId, agent_id: agentId, label: 'limits' }, cookie)).data.api_key;
});

const pay = (payload, action = 'payment.create') => client.post(`/v1/webhook/${orgId}/${agentId}`,
  { service: 'mockpay', action, payload: { fail: false, ...payload } },
  { headers: { Authorization: `Bearer ${key}` } });

test('a money cap adds up a field per day, in its time zone', async () => {
  const cap = (body) => client.post('/api/v1/spend-cap-rules', { org_id: orgId, service: 'mockpay', action: 'payment.create', window: 'day', on_exceed: 'block', ...body }, cookie);
  assert.equal((await cap({ amount_field: 'tip' })).status, 422, 'amount_field needs max_amount');
  assert.equal((await cap({})).status, 422, 'a cap needs a limit');
  assert.equal((await cap({ amount_field: 'tip', max_amount: 100, timezone: 'Mars/Base' })).status, 422);
  const ok = await cap({ amount_field: 'tip', max_amount: 100, timezone: 'Asia/Kolkata' });
  assert.equal(ok.status, 200, JSON.stringify(ok.data));

  assert.equal((await pay({ amount: 1, tip: 60 })).status, 200);
  const over = await pay({ amount: 2, tip: '50' });
  assert.equal(over.status, 429, JSON.stringify(over.data));
  assert.match(over.data.error, /would bring this day's total to 110, above 100/);
  assert.equal((await pay({ amount: 3, tip: 40 })).status, 200, 'the refused 50 did not use up the budget');
  const missing = await pay({ amount: 4 });
  assert.equal(missing.status, 429, JSON.stringify(missing.data));
  assert.match(missing.data.error, /needs a number in "tip"/);

  const listed = (await client.get('/api/v1/spend-cap-rules', cookie)).data.rules.find((r) => r.id === ok.data.id);
  assert.deepEqual([listed.amount_field, listed.max_amount, listed.timezone, listed.max_calls], ['tip', 100, 'Asia/Kolkata', null]);
});

test('a policy with hours allows calls only inside them', async () => {
  const now = new Date();
  const today = DAYS[now.getUTCDay()];
  const policy = (hours, action) => client.post('/api/v1/action-policies',
    { org_id: orgId, service: 'mockpay', action, effect: 'require', hours }, cookie);

  assert.equal((await policy({ tz: 'UTC', from: '17:00', to: '09:00' }, 'payment.refund')).status, 422);
  assert.equal((await policy({ from: '09:00', to: '17:00' }, 'payment.refund')).status, 422);

  // Outside: every day except today. Inside: today, all day (UTC).
  const closed = await policy({ tz: 'UTC', days: DAYS.filter((d) => d !== today), from: '00:00', to: '23:59' }, 'payment.refund');
  assert.equal(closed.status, 200, JSON.stringify(closed.data));
  const refused = await pay({ payment: 'pay_x' }, 'payment.refund');
  assert.equal(refused.status, 403, JSON.stringify(refused.data));
  assert.match(refused.data.error, /only allowed on .* from 00:00 to 23:59 \(UTC\)/);

  const open = await policy({ tz: 'UTC', days: [today], from: '00:00', to: '23:59' }, 'payment.create');
  assert.equal(open.status, 200, JSON.stringify(open.data));
  assert.equal((await pay({ amount: 5, tip: 0 })).status, 200);
});
