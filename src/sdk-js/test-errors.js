// A 422 from AgentRaaS throws AgentRaaSError carrying every failing field,
// and its message includes each hint. Runs against a stub server.
const assert = require('node:assert/strict');
const http = require('node:http');
const { Client, AgentRaaSError } = require('./dist/index.js');

const BODY = {
  error: 'amount must be number, got string', reqId: 'req_1',
  validation_errors: [
    { field: 'amount', rule: 'type', message: 'amount must be number, got string', hint: 'Send "amount" as a JSON number: 49.99, without quotes.' },
    { field: 'currency', rule: 'enum', message: 'currency must be one of: usd, eur', hint: 'Send "usd" (exact spelling) in "currency".' },
  ],
  next_step: 'Nothing was sent. Fix every field listed in validation_errors, following each hint, and send the call again.',
};

const server = http.createServer((req, res) => {
  req.resume();
  req.on('end', () => {
    const v = req.url.startsWith('/v1/sdk/stripe');
    res.writeHead(v ? 422 : 401, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify(v ? BODY : { error: 'nope', reqId: 'req_2' }));
  });
});

server.listen(0, '127.0.0.1', async () => {
  const client = new Client({ agentraasKey: 'k', orgId: 'o', agentId: 'a', baseUrl: `http://127.0.0.1:${server.address().port}` });
  try {
    await assert.rejects(client.call('stripe', 'charge.create', { amount: '49.99' }), (err) => {
      assert.ok(err instanceof AgentRaaSError);
      assert.equal(err.statusCode, 422);
      assert.equal(err.validationErrors.length, 2);
      assert.match(err.message, /49\.99, without quotes/);
      assert.match(err.message, /"usd" \(exact spelling\)/);
      assert.match(err.message, /Nothing was sent/);
      return true;
    });
    await assert.rejects(client.call('slack', 'message.post', {}), (err) => {
      assert.deepEqual(err.validationErrors, []);
      assert.equal(err.message, 'nope (reqId=req_2)');
      return true;
    });
    console.log('client errors: ok');
  } finally { server.close(); }
});
