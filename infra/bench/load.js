// k6 load test: fixed arrival rate against the local stack, either straight
// to the built-in mock upstream (TARGET=direct) or through AgentRaaS
// (TARGET=via, the default). Run both at the same RATE and compare: the
// difference is what AgentRaaS adds under load. Every call has a unique
// payload, so each one does the full path (auth, dedup claim, caps, forward,
// audit write); DUP=1 sends one payload over and over to time cached replies.
// BASE may list several instances (comma-separated); calls alternate between
// them. PAIRS=1 sends every payload twice, the two copies back to back on
// different instances, to check a call still runs once across instances.
// LOOKUP=1 sends refunds that a max_from policy checks against mockpay's
// order.get first (one extra upstream read per call; see README).
// Set up with: python3 infra/scripts/bench-overhead.py --setup-only
import http from 'k6/http';
import exec from 'k6/execution';
import { check } from 'k6';

const BASES = (__ENV.BASE || 'http://localhost:13000').split(',');
const via = (__ENV.TARGET || 'via') === 'via';
const dup = __ENV.DUP === '1';
const pairs = __ENV.PAIRS === '1';
const lookup = __ENV.LOOKUP === '1';

export const options = {
  summaryTrendStats: ['p(50)', 'p(95)', 'p(99)', 'max'],
  scenarios: {
    load: {
      executor: 'constant-arrival-rate',
      rate: Number(__ENV.RATE || 500),
      timeUnit: '1s',
      duration: __ENV.DURATION || '30s',
      preAllocatedVUs: 200,
      maxVUs: 1000,
    },
  },
};

// A fresh nonce per run, so a second run's payloads aren't already deduplicated by the first.
export function setup() {
  return { run: Date.now() };
}

export default function (data) {
  const i = exec.scenario.iterationInTest;
  const n = dup ? 424242 : pairs ? Math.floor(i / 2) + 1 : __VU * 10000000 + __ITER;
  const BASE = BASES[i % BASES.length];
  const body = lookup
    ? { service: 'mockpay', action: 'payment.refund', payload: { payment: `mockpay_${n}`, order_id: 'order_100000', amount: 10, run: data.run } }
    : { service: 'mockpay', action: 'payment.create', payload: { amount: n, run: dup ? 0 : data.run, fail: false } };
  const res = via
    ? http.post(`${BASE}/v1/webhook/${__ENV.ORG}/${__ENV.AGENT}`, JSON.stringify(body),
        { headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${__ENV.KEY}` } })
    : http.post(`${BASE}/internal/mockpay`, JSON.stringify({ amount: n, fail: false }),
        { headers: { 'Content-Type': 'application/json' } });
  // Print the first non-200 reply per VU, so a failing run shows why (429, 503 breaker, 409 pending...).
  if (res.status !== 200 && !globalThis.reported) {
    globalThis.reported = true;
    console.warn(`${res.status} ${String(res.body).slice(0, 200)}`);
  }
  // In PAIRS mode the second copy may arrive while the first still runs: 409 (in progress) is the right answer then.
  check(res, { '200': (r) => r.status === 200 || (pairs && r.status === 409) });
}
