// Indian brokers (Upstox, Dhan): credentials save, orders are checked before
// anything is sent, and market hours (Asia/Kolkata) and a daily cap hold.
// Nothing here reaches a broker: every order is refused before the forward.
//
// Run with the server already up: node --test test/india-brokers.test.js

const test = require('node:test');
const assert = require('node:assert/strict');
const axios = require('axios');

const BASE_URL = process.env.TEST_BASE_URL || 'http://localhost:3000';
const client = axios.create({ baseURL: BASE_URL, validateStatus: () => true });
const RUN_ID = Date.now();

let orgId, agentId, cookie, apiKey;

test.before(async () => {
  orgId = `org_india_${RUN_ID}`;
  agentId = `agent_india_${RUN_ID}`;
  const reg = await client.post('/api/v1/auth/register', { email: `india-${RUN_ID}@internal.test`, password: 'validpassword123', org_id: orgId });
  assert.equal(reg.status, 200, JSON.stringify(reg.data));
  const verify = await client.get(`/api/v1/auth/verify-email?token=${new URL(reg.data.dev_verify_url).searchParams.get('verify_token')}`);
  cookie = { headers: { Cookie: verify.headers['set-cookie'][0].split(';')[0] } };
  const conn = await client.post('/api/v1/agents/connect', { org_id: orgId, agent_id: agentId, label: 'india brokers test' }, cookie);
  assert.equal(conn.status, 200, JSON.stringify(conn.data));
  apiKey = conn.data.api_key;
});

const call = (service, action, payload) =>
  client.post(`/v1/webhook/${orgId}/${agentId}`, { service, action, payload }, { headers: { Authorization: `Bearer ${apiKey}` } });

const upstoxOrder = { instrument_token: 'NSE_EQ|INE002A01018', transaction_type: 'BUY', product: 'D', order_type: 'LIMIT', validity: 'DAY', price: 1400, trigger_price: 0, disclosed_quantity: 0 };
const dhanOrder = { dhanClientId: '1000000003', transactionType: 'BUY', exchangeSegment: 'NSE_EQ', productType: 'CNC', orderType: 'LIMIT', validity: 'DAY', securityId: '11536', price: 3500 };

// Inside NSE cash hours right now (weekday 09:15-15:30 Asia/Kolkata)?
function nseOpenNow() {
  const parts = Object.fromEntries(new Intl.DateTimeFormat('en-GB', { timeZone: 'Asia/Kolkata', weekday: 'short', hour: '2-digit', minute: '2-digit', hourCycle: 'h23' })
    .formatToParts(new Date()).map((p) => [p.type, p.value]));
  const minutes = Number(parts.hour) * 60 + Number(parts.minute);
  return !['Sat', 'Sun'].includes(parts.weekday) && minutes >= 9 * 60 + 15 && minutes < 15 * 60 + 30;
}

test('both brokers take a daily access token, and a bad value is refused before sending', async () => {
  for (const service of ['upstox', 'dhan']) {
    const saved = await client.post('/api/v1/credentials', { org_id: orgId, service, credentials: { api_key: `test-token-${service}` } }, cookie);
    assert.equal(saved.status, 200, JSON.stringify(saved.data));
  }

  const up = await call('upstox', 'order.place', { ...upstoxOrder, quantity: 1, transaction_type: 'HOLD' });
  assert.equal(up.status, 422, JSON.stringify(up.data));
  assert.match(JSON.stringify(up.data), /transaction_type must be one of: BUY, SELL/);

  const dh = await call('dhan', 'order.place', { ...dhanOrder, quantity: 1, exchangeSegment: 'NSE' });
  assert.equal(dh.status, 422, JSON.stringify(dh.data));
  assert.match(JSON.stringify(dh.data), /exchangeSegment must be one of: NSE_EQ/);

  const noId = await call('upstox', 'order.cancel', {});
  assert.equal(noId.status, 422, JSON.stringify(noId.data));
});

test('a policy keeps orders to NSE hours and to delivery', async () => {
  const hours = { tz: 'Asia/Kolkata', days: ['mon', 'tue', 'wed', 'thu', 'fri'], from: '09:15', to: '15:30' };
  const res = await client.post('/api/v1/action-policies',
    { org_id: orgId, service: 'dhan', action: 'order.place', effect: 'require', hours, fields: { productType: { enum: ['CNC'] }, exchangeSegment: { enum: ['NSE_EQ', 'BSE_EQ'] } } }, cookie);
  assert.equal(res.status, 200, JSON.stringify(res.data));

  // Intraday is off the list, so this is refused in market hours too:
  // outside them for the hours, inside them for the product.
  const intraday = await call('dhan', 'order.place', { ...dhanOrder, quantity: 2, productType: 'INTRADAY' });
  assert.equal(intraday.status, 403, JSON.stringify(intraday.data));
  if (nseOpenNow()) {
    assert.match(intraday.data.error, /productType must be one of: CNC/);
  } else {
    assert.match(intraday.data.error, /only allowed on mon, tue, wed, thu, fri from 09:15 to 15:30 \(Asia\/Kolkata\)/);
  }

  const fno = await call('dhan', 'order.place', { ...dhanOrder, quantity: 3, exchangeSegment: 'NSE_FNO' });
  assert.equal(fno.status, 403, JSON.stringify(fno.data));
});

test('a daily cap in Asia/Kolkata adds up the order size', async () => {
  const cap = (body) => client.post('/api/v1/spend-cap-rules',
    { org_id: orgId, service: 'upstox', action: 'order.place', window: 'day', on_exceed: 'block', amount_field: 'quantity', max_amount: 100, ...body }, cookie);
  assert.equal((await cap({ timezone: 'India/Mumbai' })).status, 422);
  const ok = await cap({ timezone: 'Asia/Kolkata' });
  assert.equal(ok.status, 200, JSON.stringify(ok.data));

  // One order above the day's limit is refused before it is sent.
  const big = await call('upstox', 'order.place', { ...upstoxOrder, quantity: 500 });
  assert.equal(big.status, 429, JSON.stringify(big.data));
  assert.match(big.data.error, /would bring this day's total to 500, above 100/);

  const listed = (await client.get('/api/v1/spend-cap-rules', cookie)).data.rules.find((r) => r.id === ok.data.id);
  assert.deepEqual([listed.amount_field, listed.max_amount, listed.timezone], ['quantity', 100, 'Asia/Kolkata']);
});
