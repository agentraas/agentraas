# Cross-agent locks (design)

Status: built in 0.29.0 (2026-10-29) as "record locks" (`record_locks.rs`,
migration 062, `/docs#record-locks`). 0.30.0 added `lock_wait_ms`, the
last-write note for approvers, `lock` on custom actions, and turned locks
on by default (migration 063). 0.30.3 added the FIFO wait list (a Redis
sorted set per record, `resource_lock_q:<org>:<id>`).

## Problem

Two different agents act on the same business record at the same time: a
billing agent changes Stripe customer `cus_123`'s subscription while a
support agent refunds that customer; two sales agents update the same
HubSpot contact. Dedup doesn't catch it, because the calls are different.

## What exists today

`resource_id` (webhook body or `X-AgentRaaS-Resource-Id`) plus
`agentraas_core::resource_lock`: after the dedup claim, the gateway runs
`SET resource_lock:<org>:<resource_id> <token> NX EX 15`. A second call
naming the same id gets `409` and an audit row `blocked / resource_locked`.
The lock is released (compare-and-delete on the token) when the call
finishes, and kept until its TTL when the outcome is unknown.

Two gaps:

1. The agent has to name the record. Agents rarely do, so in practice the
   lock is never taken.
2. Fixed in 0.28.1: the lease was a fixed 15 s, shorter than the default
   `PROXY_TIMEOUT_SECONDS` (30), so a slow call could lose its lock while
   still running. It is now the longest forward (every attempt timing out,
   plus backoff) plus 5 s.

## Proposal

### Lock key derived per service

Each curated action that writes to a record names where the record id is,
in `config/services.json`:

```json
"subscription.create": { "method": "POST", "path": "/v1/subscriptions",
  "lock": { "record": "customer", "from": ["customer"] } }
"contact.update":      { "lock": { "record": "contact", "from": ["contactId", "properties.email"] } }
"payment.refund":      { "lock": { "record": "payment", "from": ["payment"] } }
```

`from` lists payload paths, first non-empty wins. The key is
`resource_lock:<org>:<service>:<record>:<sha256(normalized value)>`:

- the service is part of the key, so `stripe` customer `123` and `hubspot`
  contact `123` never collide;
- the value is normalized (trimmed; emails lowercased) and hashed, so no
  customer id or email sits in Redis in clear text;
- custom actions get the same `lock` field when registered;
- an explicit `resource_id` from the agent is taken as well as the derived
  key (since 0.30.2; it used to replace it, which let a call skip the record
  lock). Both are taken in id order, so two calls can't deadlock.

The same Redis `SET NX EX` + token mechanism as today, so no new moving
part. At most two locks per call, taken in id order with a bounded wait,
so there is no deadlock.

### Lease

- Lease = the longest forward + 5 s (`forward::resource_lock_ttl_seconds`, since 0.28.1).
- Released right after the call (success or a failure that certainly did
  not run), kept until the lease ends when the outcome is unknown, as today.
- A crash leaves the lock to expire on its lease; Redis keeps the time, so
  server clocks don't matter.

### What the second agent gets

`409` with a body an agent can act on:

```json
{ "error": "resource_locked", "record": "stripe customer",
  "held_by_agent": "billing-agent", "retry_after_ms": 1800, "reqId": "..." }
```

plus a `Retry-After` header. The holder's agent id is shown because both
agents belong to the same org. Optional, off by default: `wait_ms` (up to a
few seconds) to wait for the lock instead of failing, useful for MCP
callers that can't easily retry. The audit row stays `blocked /
resource_locked`, so reports and alerts already count it.

### Order in the pipeline

Unchanged: dedup claim first (a retry of the same call gets its cached
answer, not a lock error), then the lock, then validation, policies, spend
caps and approvals. A call refused by a later gate releases the lock.

### Approvals

A call held for approval must not keep the lock while it waits (that can be
hours and would freeze the record for every other agent). So:

- the lock is released when the call is held;
- it is taken again when the approved call actually runs. If another agent
  holds it at that moment, the approved call waits up to `wait_ms` and then
  fails as "record busy" into the dead-letter queue, still approved, for a
  person to replay;
- the approval page shows "another agent changed this record after the
  request was made" when the record's last-write time (one more Redis key,
  set on each successful locked call) is newer than the hold.

### Shadow mode

Shadow calls never take a lock: they would block live agents for actions
that are never sent. A shadow call reads the key instead (`GET`, no `SET`)
and, if it is held, reports `would_wait_for_lock` as its decision, the same
way it reports `would_deduplicate`.

## Failure modes

| Failure | Behaviour |
|---|---|
| Redis down | The dedup claim before it fails first, so the call is refused (500); no lock-less forward happens. |
| Record id missing from the payload | No lock is taken and the call runs, logged once per action so the `from` paths can be fixed. Failing closed here would break every call with an optional field. |
| Upstream slower than the lease | Prevented by the lease formula above; if it still happens the second agent may run, which is today's behaviour without any lock. |
| Server crash mid-call | Lock expires on its lease; the dedup slot already turns the call into outcome unknown. |
| Hot record (many agents) | Callers that wait (`lock_wait_ms`) queue in arrival order; while anyone waits, only the head may take the lock. A waiter that crashes drops out after the longest wait. |
| Two orgs, same id | Different keys (org is in the key). |

## Rollout

1. (Done in 0.28.1: the lease now outlasts the longest forward.)
2. `lock` entries for the curated write actions; per-org switch "Lock
   records automatically", off by default. Cost: one `SET NX` and one
   compare-and-delete per locked call, measured with
   `infra/scripts/bench-overhead.py`.
3. Integration tests: two agents, same customer, concurrent: exactly one
   reaches mockpay (hit counter), the other gets `409` with `retry_after_ms`;
   approval resume while locked; shadow call reports the lock without taking it.
4. Default on for curated actions once a release has run with it.
