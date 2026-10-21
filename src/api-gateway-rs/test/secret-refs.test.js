// Secret references (secret_refs.rs): a credential can name a secret in
// Vault / AWS / GCP / Azure instead of holding it. Self-hosted Enterprise
// only (Cloud refuses them: the server's identity is shared). On an
// Enterprise org the reference is saved and shown as-is, and a call whose
// reference can't be fetched fails before anything is sent, with the
// reason in the audit log (this server has no VAULT_ADDR). Below Enterprise, saving one is 403
// and a plain key still saves. The real fetch is unit-tested against Vault
// (cargo test secret_refs with VAULT_TEST_SECRET_REF).
//
// Run with the server already up: node --test test/secret-refs.test.js

const test = require('node:test');
const assert = require('node:assert/strict');
const axios = require('axios');
const { Client } = require('pg');

const client = axios.create({ baseURL: process.env.TEST_BASE_URL || 'http://localhost:3000', validateStatus: () => true });
const RUN_ID = Date.now();
const REF = 'vault://secret/data/stripe#api_key';

test('secret references: Enterprise saves and resolves them, lower plans get 403', async (t) => {
  const orgId = `org_secretref_${RUN_ID}`;
  const agentId = `agent_secretref_${RUN_ID}`;
  const reg = await client.post('/api/v1/auth/register', { email: `secretref-${RUN_ID}@internal.test`, password: 'validpassword123', org_id: orgId });
  assert.equal(reg.status, 200, JSON.stringify(reg.data));
  const verify = await client.get(`/api/v1/auth/verify-email?token=${new URL(reg.data.dev_verify_url).searchParams.get('verify_token')}`);
  const headers = { Cookie: verify.headers['set-cookie'][0].split(';')[0] };
  const save = (api_key) => client.post('/api/v1/credentials', { org_id: orgId, service: 'stripe', credentials: { api_key } }, { headers });

  const me = (await client.get('/api/v1/auth/me', { headers })).data.user;
  if (me.deployment_mode === 'cloud' || me.plan !== 'enterprise') {
    // Cloud: refused for everyone (the server's identity is shared). Self-host below Enterprise: 403 too.
    const refused = await save(REF);
    assert.equal(refused.status, 403, JSON.stringify(refused.data));
    const plain = await save('sk_test_plain');
    assert.equal(plain.status, 200, JSON.stringify(plain.data));
    t.diagnostic(`${me.deployment_mode}/${me.plan}: only the 403 path checked`);
    return;
  }

  const badAzure = await save('azure-kv://evil.example/secrets/x');
  assert.equal(badAzure.status, 422, JSON.stringify(badAzure.data));

  const saved = await save(REF);
  assert.equal(saved.status, 200, JSON.stringify(saved.data));
  assert.equal(saved.data.masked_preview, REF);

  const conn = await client.post('/api/v1/agents/connect', { org_id: orgId, agent_id: agentId, label: 'secret ref test' }, { headers });
  assert.equal(conn.status, 200, JSON.stringify(conn.data));
  const res = await client.post(`/v1/webhook/${orgId}/${agentId}`,
    { service: 'stripe', action: 'customer.create', payload: { email: `ref-${RUN_ID}@internal.test` } },
    { headers: { Authorization: `Bearer ${conn.data.api_key}` } });
  assert.ok(res.status >= 400, JSON.stringify(res.data));
  // Agents get the generic error; the reason is in the audit log (dashboard row detail).
  const pg = new Client({ connectionString: process.env.DATABASE_URL || 'postgres://agentraas:agentraas@localhost:5432/agentraas' });
  await pg.connect();
  const row = await pg.query('SELECT error_type FROM audit_log WHERE req_id = $1 ORDER BY created_at DESC LIMIT 1', [res.data.reqId]);
  await pg.end();
  assert.match(row.rows[0]?.error_type || '', /secret reference .* could not be fetched/);
});
