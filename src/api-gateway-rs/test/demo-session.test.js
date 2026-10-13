// The public read-only demo (/api/v1/demo/session): reads work, every write
// but the live dedup test is refused, and each demo visitor has their own
// rate limit. Cloud mode only (self-hosted servers have no public demo).

const test = require('node:test');
const assert = require('node:assert/strict');
const axios = require('axios');
const { Client } = require('pg');

const client = axios.create({ baseURL: process.env.TEST_BASE_URL || 'http://localhost:3000', validateStatus: () => true, maxRedirects: 0 });
const RUN_ID = Date.now();

test('demo session is read-only', async (t) => {
  const pg = new Client({ connectionString: process.env.DATABASE_URL || 'postgres://agentraas:agentraas@localhost:5432/agentraas' });
  await pg.connect();
  try {
    const email = `demo-${RUN_ID}@internal.test`;
    const reg = await client.post('/api/v1/auth/register', { email, password: 'validpassword123', org_id: `org_demo_${RUN_ID}` });
    assert.equal(reg.status, 200, JSON.stringify(reg.data));
    await client.get(`/api/v1/auth/verify-email?token=${new URL(reg.data.dev_verify_url).searchParams.get('verify_token')}`);
    // Use the server's demo account if it has one, else make this one it.
    const existing = await pg.query('SELECT 1 FROM users WHERE is_demo AND NOT is_admin LIMIT 1');
    if (!existing.rows.length) await pg.query('UPDATE users SET is_demo = true WHERE email = $1', [email]);

    const res = await client.get('/api/v1/demo/session');
    if (res.status === 404) { t.skip('Cloud mode only'); return; }
    assert.equal(res.status, 303);
    const headers = { Cookie: res.headers['set-cookie'][0].split(';')[0] };

    const me = await client.get('/api/v1/auth/me', { headers });
    assert.equal(me.status, 200);
    assert.equal(me.data.user.read_only, true);
    assert.equal((await client.get('/api/v1/recent?limit=5', { headers })).status, 200);

    for (const [method, path, body] of [
      ['post', '/api/v1/agents/connect', { org_id: `org_demo_${RUN_ID}`, agent_id: 'x' }],
      ['post', '/api/v1/auth/password', { current_password: 'validpassword123', new_password: 'anotherpassword1' }],
      ['post', '/api/v1/auth/logout-all', {}],
      ['post', '/api/v1/demo/reset', {}],
      ['put', '/api/v1/billing/usage', { org_id: `org_demo_${RUN_ID}`, monthly_cap_usd: 5 }],
    ]) {
      const r = await client[method](path, body, { headers });
      assert.equal(r.status, 403, `${method.toUpperCase()} ${path} must be refused: ${JSON.stringify(r.data)}`);
    }
    // GETs that sign a license key or build a package aren't reads.
    for (const path of [`/api/v1/licensing/token?org_id=org_demo_${RUN_ID}`, '/api/v1/download/self-host']) {
      assert.equal((await client.get(path, { headers })).status, 403, `GET ${path} must be refused`);
    }
    assert.equal((await client.post('/api/v1/demo/live-test', {}, { headers })).status, 200, 'the live dedup test stays allowed');
  } finally {
    await pg.query('UPDATE users SET is_demo = false WHERE email = $1', [`demo-${RUN_ID}@internal.test`]);
    await pg.end();
  }
});
