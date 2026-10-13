// Jira: an agent-callable service (jira.issue.create and friends) and an
// alert channel that opens tickets. No real Atlassian site is called here:
// this checks config, validation and storage.

const test = require('node:test');
const assert = require('node:assert/strict');
const axios = require('axios');

const client = axios.create({ baseURL: process.env.TEST_BASE_URL || 'http://localhost:3000', validateStatus: () => true });
const RUN_ID = Date.now();

async function registerAndVerify(email, password, orgId) {
  const reg = await client.post('/api/v1/auth/register', { email, password, org_id: orgId });
  assert.equal(reg.status, 200, JSON.stringify(reg.data));
  const token = new URL(reg.data.dev_verify_url).searchParams.get('verify_token');
  const verify = await client.get(`/api/v1/auth/verify-email?token=${token}`);
  return verify.headers['set-cookie'][0].split(';')[0];
}

test('Jira is a service agents can call, and a validated alert channel', async () => {
  const orgId = `org_jira_${RUN_ID}`;
  const Cookie = await registerAndVerify(`jira-${RUN_ID}@internal.test`, 'validpassword123', orgId);
  const headers = { Cookie };

  const services = await client.get('/api/v1/services', { headers });
  assert.equal(services.status, 200);
  assert.ok(JSON.stringify(services.data).includes('jira'), 'jira is listed as a service');

  const base = { org_id: orgId, type: 'jira', target: 'test-token', email: 'ops@acme.test', project_key: 'OPS' };
  for (const site of ['evil.com/x', 'acme.evil.com']) {
    const bad = await client.post('/api/v1/notification-webhooks', { ...base, site }, { headers });
    assert.equal(bad.status, 422, `site ${site} must be refused: ${JSON.stringify(bad.data)}`);
  }
  assert.equal((await client.post('/api/v1/notification-webhooks', { ...base, site: 'acme', project_key: 'ops op' }, { headers })).status, 422);

  const ok = await client.post('/api/v1/notification-webhooks', { ...base, site: 'https://Acme.atlassian.net' }, { headers });
  assert.equal(ok.status, 200, JSON.stringify(ok.data));
  const list = await client.get('/api/v1/notification-webhooks', { headers });
  const row = list.data.find((r) => r.org_id === orgId);
  assert.equal(row.type, 'jira');
  assert.equal(JSON.stringify(list.data).includes('test-token'), false, 'the API token is never listed');
});
