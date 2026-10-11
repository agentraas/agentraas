// Self-hosted without LICENSE_TOKEN = Community: Team/Enterprise features
// (here: a spend cap that holds for approval) answer 403, the core still works. Skips on Cloud and on a licensed server
// (CI's self-hosted run has a test license), so run it against a
// self-hosted server started without one.

const test = require('node:test');
const assert = require('node:assert/strict');
const axios = require('axios');

const client = axios.create({ baseURL: process.env.TEST_BASE_URL || 'http://localhost:3000', validateStatus: () => true });
const RUN_ID = Date.now();

test('no license key: gated features 403, core works', async (t) => {
  const orgId = `org_licgate_${RUN_ID}`;
  const reg = await client.post('/api/v1/auth/register', { email: `licgate-${RUN_ID}@internal.test`, password: 'validpassword123', org_id: orgId });
  assert.equal(reg.status, 200, JSON.stringify(reg.data));
  const verify = await client.get(`/api/v1/auth/verify-email?token=${new URL(reg.data.dev_verify_url).searchParams.get('verify_token')}`);
  const headers = { Cookie: verify.headers['set-cookie'][0].split(';')[0] };

  const me = (await client.get('/api/v1/auth/me', { headers })).data.user;
  if (me.deployment_mode === 'cloud' || me.plan !== 'free') {
    t.skip(`needs a self-hosted server with no license (got ${me.deployment_mode}, ${me.plan})`);
    return;
  }

  const cap = await client.post('/api/v1/spend-cap-rules', { org_id: orgId, service: 'mockpay', action: 'payment.create', window: 'hour', max_calls: 5, on_exceed: 'hitl' }, { headers });
  assert.equal(cap.status, 403, JSON.stringify(cap.data));
  assert.match(cap.data.error, /LICENSE_TOKEN/);

  const block = await client.post('/api/v1/spend-cap-rules', { org_id: orgId, service: 'mockpay', action: 'payment.create', window: 'hour', max_calls: 5, on_exceed: 'block' }, { headers });
  assert.equal(block.status, 200, 'a blocking spend cap is Community');

  const connect = await client.post('/api/v1/agents/connect', { org_id: orgId, agent_id: 'a1', label: 'license gate' }, { headers });
  assert.equal(connect.status, 200, 'the core (agents, proxy) needs no key');
});
