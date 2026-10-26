# Benchmarks

Two tests, both against the local stack (`./install.sh`, API on `:13000`),
both reproducible with the commands below. Never point them at production.

- `infra/scripts/bench-overhead.py`: **latency added** per call, sequential,
  so nothing queues. Times the same call straight to the built-in mock
  upstream and through AgentRaaS.
- `infra/bench/load.js` (k6): **behavior under concurrent load** at a fixed
  arrival rate. Every call has a unique payload, so each one takes the full
  path: API key auth, rate limit, dedup claim in Redis, spend caps, policies,
  forward, audit write to Postgres. `DUP=1` repeats one payload to time the
  answer from the dedup cache.

## Results (2026-10-26, v0.24.1)

Same machine and setup as the 2026-10-01 run below (Intel i3-12100, 4 cores,
8 GB; k6, API, Postgres and Redis on one box, so a floor, not a ceiling).
Each row is one 30 s run.

| Run | Rate (calls/s) | p50 | p95 | p99 | Errors |
|---|---|---|---|---|---|
| Mock upstream called directly | 1000 | 0.1 ms | 0.1 ms | 0.2 ms | 0 |
| Unique calls, full path | 500 | 2.5 ms | 3.6 ms | 24 ms | 0 |
| Unique calls, full path | 800 | 2.7 ms | 36 ms | 101 ms | 0 (30 calls not started: k6 out of free VUs) |
| Unique calls, full path | 1000 | 3.0 ms | 108 ms | 142 ms | 0 |
| One payload repeated (dedup cache) | 1000 | 1.8 ms | 29 ms | 64 ms | 3 x 409 "already being processed", the copies that arrived while the first was still running |
| Refund with a `max_from` lookup (one extra upstream read each) | 300 | 2.3 ms | 3.0 ms | 3.4 ms | 0 |
| Refund with a `max_from` lookup | 500 | 2.5 ms | 31 ms | 426 ms | 0, saturated (302 calls not started) |

**Two instances, one Redis and Postgres (`PAIRS=1`).** A second API container
on port 13002, same image and settings. Every payload was sent twice, the two
copies back to back to different instances, 600 calls/s for 30 s:
17,568 calls, 8,784 payloads, **8,784 ran**: each exactly once. The other
copies were answered from the record (7,440) or told the first was still
running (1,344, a 409). No payload ran twice. Latency in this run (p99 1.1 s)
says nothing about scaling: both instances shared the same 4 cores with k6.

**Summary:** about the same as 0.9.0: one instance on this 4-core machine
handles 800 to 1000 unique calls/s, p99 around 100 to 140 ms at the top. A
lookup limit roughly halves that for the calls it applies to (300 calls/s
clean, 500 saturated), since each one is two upstream calls. Exactly-once
holds across instances.

## Results (2026-10-01, v0.9.0)

Hardware: one desktop, Intel i3-12100 (4 cores, 8 threads), 8 GB RAM. k6,
the API, Postgres and Redis all ran on that same machine, so the load
generator competed with the server for CPU. These numbers are a floor for
this hardware, not a ceiling for the software. Each row is a 30 s run.

Unique calls (full path):

| Rate (calls/s) | p50 | p95 | p99 | Errors |
|---|---|---|---|---|
| 500 | 2.4 ms | 3.5 ms | 17 ms | 0 |
| 800 | 2.6 ms | 26 ms | 52 ms | 0 |
| 1000 | 3.0 ms | 101 ms | 254 ms | 0 (best of two runs; the other saturated, p50 385 ms) |
| 1500 | 712 ms | 1.8 s | 2.0 s | 0, saturated: queueing |

Repeated call (answered from the dedup cache):

| Rate (calls/s) | p50 | p95 | p99 | Errors |
|---|---|---|---|---|
| 1000 | 1.8 ms | 13 ms | 71 ms | 0 |

For reference, the mock upstream called directly at 1000 calls/s answers in
0.08 ms p50, 0.26 ms p99, so the times above are almost entirely AgentRaaS.

Sequential overhead from `bench-overhead.py` (2026-09-27, same machine, 3
runs of 500): 2.1 to 2.4 ms added at p50, 2.9 to 3.7 ms at p95.

**Summary:** on this 4-core machine one instance handles about 800 unique
calls/s with p99 under 60 ms. Around 1000 calls/s it is at the edge:
one run held, another saturated.
Raising the Postgres pool from 10 to 40 connections did not move the
ceiling. No call failed at any rate; past saturation, calls wait.

## What has not been measured

- A dedicated server, or k6 on a separate machine.
- Throughput of more than one API instance on separate machines. Correctness
  across two instances is measured (above); the speedup is not.
- Where the time goes at saturation. Profile before tuning.

## Running it

```bash
# 1. Raise the bench org's limits in .env for the run (remove afterwards),
#    then restart the API container:
#      ENTERPRISE_RATE_LIMIT_PER_MIN=10000000
#      ENTERPRISE_MONTHLY_LIMIT=100000000
# 2. Create a throwaway org and agent:
eval "$(python3 infra/scripts/bench-overhead.py --setup-only | tail -1)"
# 3. Run k6 (TARGET=direct for the baseline, DUP=1 for cached replies,
#    LOOKUP=1 for refunds under a max_from policy on mockpay payment.refund,
#    PAIRS=1 with BASE=http://localhost:13000,http://localhost:13002 for two
#    instances; the second one: podman run on the agentraas-net network with
#    the first one's environment and mounts, published on 13002):
podman run --rm --network host -v "$PWD/infra/bench:/b:ro,z" \
  -e RATE=500 -e DURATION=30s -e ORG -e AGENT -e KEY \
  docker.io/grafana/k6:latest run /b/load.js
```

The first non-200 reply from each k6 VU is printed as a warning, so a
failing run shows its cause.
