// Per-key service switches (agent/mod.rs set_key_services): a key with a
// list may only call those services; no list means every service.
// Regenerating keeps the list, and nobody else can change it.
//
// Run with the server already up: node --test test/key-services.test.js

const test = require('node:test');
const assert = require('node:assert/strict');
const axios = require('axios');

const BASE_URL = process.env.TEST_BASE_URL || 'http://localhost:3000';
const client = axios.create({ baseURL: BASE_URL, validateStatus: () => true });
const RUN_ID = Date.now();

async function registerAndVerify(email, orgId) {
  const reg = await client.post('/api/v1/auth/register', { email, password: 'validpassword123', org_id: orgId });
  assert.equal(reg.status, 200, JSON.stringify(reg.data));
  const verify = await client.get(`/api/v1/auth/verify-email?token=${new URL(reg.data.dev_verify_url).searchParams.get('verify_token')}`);
  return { headers: { Cookie: verify.headers['set-cookie'][0].split(';')[0] } };
}

test('a key only reaches the services switched on for it', async () => {
  const orgId = `org_keysvc_${RUN_ID}`;
  const agentId = 'my-agent';
  const auth = await registerAndVerify(`keysvc-${RUN_ID}@internal.test`, orgId);
  const conn = await client.post('/api/v1/agents/connect', { org_id: orgId, agent_id: agentId, label: 'key services test' }, auth);
  assert.equal(conn.status, 200, JSON.stringify(conn.data));
  let apiKey = conn.data.api_key;
  const keyId = async () => (await client.get('/api/v1/agents/keys', auth)).data.find((k) => !k.revoked_at && k.org_id === orgId).id;
  const pay = (amount) => client.post(`/v1/webhook/${orgId}/${agentId}`,
    { service: 'mockpay', action: 'payment.create', payload: { amount, fail: false } },
    { headers: { Authorization: `Bearer ${apiKey}` } });
  const setServices = async (services) => client.put(`/api/v1/agents/keys/${await keyId()}/services`, { services }, auth);

  assert.equal((await pay(1)).status, 200, 'a new key reaches every service');

  assert.equal((await setServices(['stripe'])).status, 200);
  const off = await pay(2);
  assert.equal(off.status, 403, JSON.stringify(off.data));
  assert.match(off.data.error, /isn't enabled for "mockpay"/);

  assert.equal((await setServices(['stripe', 'mockpay'])).status, 200);
  assert.equal((await pay(3)).status, 200);

  // Regenerating keeps the switches.
  assert.equal((await setServices(['stripe'])).status, 200);
  const regen = await client.post(`/api/v1/agents/keys/${await keyId()}/regenerate`, {}, auth);
  assert.equal(regen.status, 200, JSON.stringify(regen.data));
  apiKey = regen.data.api_key;
  const listed = (await client.get('/api/v1/agents/keys', auth)).data.find((k) => !k.revoked_at && k.org_id === orgId);
  assert.deepEqual(listed.allowed_services, ['stripe']);
  assert.equal((await pay(4)).status, 403, 'the regenerated key is limited too');

  assert.equal((await setServices(null)).status, 200);
  assert.equal((await pay(5)).status, 200, 'null turns the limit off');

  assert.equal((await setServices(['bad name!'])).status, 422);

  const other = await registerAndVerify(`keysvc-other-${RUN_ID}@internal.test`, `org_keysvc_other_${RUN_ID}`);
  const foreign = await client.put(`/api/v1/agents/keys/${await keyId()}/services`, { services: [] }, other);
  assert.equal(foreign.status, 404, "another user can't change this key");
});
