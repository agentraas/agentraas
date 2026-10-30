# Seller-portal agents (design)

Status: design only, not built.

## Problem

Quick-commerce sellers on Blinkit, Zepto and Swiggy Instamart manage
prices, stock and listings in each platform's seller portal. None of the
three publishes an open seller API, so an agent that wants to "set SKU X to
Rs 99 on Blinkit" has to drive a browser: open the portal, find the SKU,
type the price, press save. Sellers are starting to do this with
browser-driving agents. Those agents make exactly the mistakes AgentRaaS
exists for (a retry that applies a change twice, a loop, a fat-fingered
price, two agents editing the same SKU), and there is no API call for
AgentRaaS to sit in front of.

Idea under test: put a browser MCP server (for example Playwright MCP)
behind AgentRaaS, so browser actions go through the same pipeline as API
calls.

## What exists today

Two ways to put AgentRaaS in front of an MCP server:

1. **`agentraas wrap`** (`src/sdk/agentraas/mcp_wrap.py`). A local stdio
   proxy around the server's own command. It deduplicates `tools/call` for
   write tools (identical tool + arguments inside `--ttl`, default 600 s,
   SQLite in `~/.agentraas`), skips read-only tools (the server's
   `readOnlyHint`, or names starting with `get`, `list`, `search`, `read`
   and similar), never caches a failed call, and appends every call to a
   JSON-lines log. No server, no policies, no approvals, no dashboard.
2. **Registered MCP servers** (`custom_mcp_servers`, migration 040,
   `mcp_servers.rs`). The server's tools appear in AgentRaaS's own `/mcp`
   `tools/list` as `<server>.<tool>`, and a `tools/call` runs the full
   pipeline (`agent::handle_request`, `Source::Mcp`): dedup, record locks,
   validation, action policies, spend caps, audit, then
   `forward::forward_mcp_tool_call`.

Facts from the code that shape this design:

- **Approvals don't work over MCP.** In `agent/mod.rs` a policy or spend
  cap that would hold a call for approval does so only when the source is
  not MCP (`v.hitl && !mcp`). Over MCP the call is refused (403, "Approval
  can't be requested over MCP"), failing closed. Webhook and SDK calls can
  be held (`ee/hitl.rs` answers `pending_approval` with a `status_url`).
- **Registered MCP servers must have a public address.** Registration and
  every forward run `validate_target_url`, and the shared HTTP client drops
  private addresses at connect time (`PublicOnlyResolver`). A Playwright
  MCP server on `localhost` or the LAN can't be registered. Exposing a
  browser that is logged in to a seller account on a public URL is not
  acceptable, so path 2 needs a tunnel or a different hosting answer.
- **The server-side MCP forward is one bare JSON POST.** It sends
  `tools/call` with no `initialize` handshake and no `Mcp-Session-Id`, and
  reads a plain JSON body. A streamable-HTTP server that requires a session
  or answers with an event stream will not work as is. Untested against
  Playwright MCP.
- **Dedup can be steered.** An `idempotency_key` (MCP argument, or
  `X-AgentRaaS-Idempotency-Key`) replaces the payload hash; custom dedup
  rules (`custom_dedup_rules`) pick fields and a TTL per service/action;
  `resource_id` takes a record lock.

## Why raw browser tools are the wrong unit

Browser MCP tools are low level: navigate, snapshot, click, type, press a
key. (Playwright MCP's tools are named like `browser_click` and target an
element by a `ref` taken from the latest page snapshot. This is from its
public docs, not checked here.) Putting AgentRaaS in front of those tools
directly breaks every gate:

- **Dedup is wrong both ways.** Two identical clicks can both be
  legitimate (next page, then next page again), so dedup blocks real work.
  And `ref` values change with every snapshot, so a retried click after a
  re-snapshot has different arguments and is not caught at all.
- **Nothing can be checked.** A policy can't read a price limit out of
  `browser_type {ref: "e41", text: "9"}`. Spend caps, `max_from` limits and
  record locks have no field to look at.
- **Reads and writes look alike.** `browser_click` on a tab and on "Save"
  are the same tool. Only the page knows which one changed state.
- **Screenshots and snapshots carry customer data** (names, phone numbers,
  addresses on order pages), and they would flow through the audit trail,
  recordings and the dead-letter queue.

So one click is not one action. The action is the business intent.

## Proposal

### A portal adapter with intent-level tools

A small MCP server (or HTTP custom action) that runs next to the browser,
on the seller's own machine, and exposes one tool per business intent:

```
blinkit.get_price      {sku}                -> {sku, price, seen_at}
blinkit.set_price      {sku, price}         -> {sku, before, after, verified}
blinkit.set_stock      {sku, quantity}      -> {sku, before, after, verified}
blinkit.set_available  {sku, available}     -> {sku, before, after, verified}
```

Each tool is a fixed Playwright script (deterministic: the same steps every
time), not an LLM choosing clicks. The LLM decides *what* to change; the
script decides *how*. When the portal's page changes, the script fails
loudly instead of clicking something else. Option B, if scripts prove too
brittle: the adapter lets an LLM drive the browser *inside* one intent, but
only after the intent passed AgentRaaS, and with the same read-back check.

AgentRaaS then sees `set_price {sku: "X", price: 99}`, so every existing
gate applies unchanged:

| Gate | How it applies |
|---|---|
| Dedup | Payload hash of the intent; agents pass an `idempotency_key` per decision (below). |
| Record locks | `resource_id = "blinkit:sku:X"` (or a `lock` entry on the custom action), so two agents can't edit one SKU at once. |
| Action policies | `price` between limits, or within a percentage of the current price (`max_from`). |
| Approvals | Webhook/SDK path only (below). |
| Shadow mode | Run the agent for a week with calls decided but not sent. |
| Undo | `set_price` returns `before`, so the undo is `set_price {sku, price: before}`. |
| Audit | One row per intent, not per click. |

Reads (`get_*`) are marked read-only, so they are never deduplicated and
never need approval.

### Idempotency without provider keys

The portals give us no idempotency key, so the adapter makes each write
safe to repeat by itself:

1. Only *set* operations: price = 99, stock = 40, available = false.
   "Add 5 units" or "lower by 10%" is refused; the agent reads first and
   sends the absolute value.
2. Before writing, read the current value. If it already equals the
   target, return `{verified: true, already: true}` without clicking.
3. After saving, reload and read the value back. Equal: success. Not
   equal, or the read fails: an error, and nothing is assumed.

A retry after a timeout is then harmless: step 2 sees the change already
made. AgentRaaS still marks a timed-out call outcome unknown and keeps its
dedup slot (the 2026-09-27 rule); the adapter's `get_price` is how a person
settles it.

### Dedup and the legitimate repeat

A set operation repeated with the same value inside the window is usually
a real duplicate, and blocking it is right. The case to design for: price
99, then 120, then back to 99 within the window. The second "99" has the
same payload as the first, and dedup would answer it from cache without
running, reporting a change that never happened.

Fix: the agent sends an `idempotency_key` per decision (its own decision or
step id), which replaces the payload hash. Retries of one decision share
the key; a new decision gets a new one. As a backstop, a custom dedup rule
gives `set_*` actions a short TTL. Read-back means even a wrong dedup
answer can be detected, but only if the agent reads.

### Approvals

Holds don't work over MCP, and the adapter's tools are reached over MCP in
path 2. Options, in order of preference:

1. **The agent calls the intent through the SDK or webhook**, with the
   adapter registered as a custom action (HTTP). Holds work there today:
   `pending_approval` plus a `status_url` to poll. Needs the adapter on an
   address AgentRaaS may reach (see the next section).
2. **Policies that block instead of hold** over MCP (today's behaviour): a
   price change over the limit is refused, and a person makes it by hand.
   Fails closed, no new code.
3. Make holds work over MCP (answer "held, poll this", or MCP's own
   long-running task support once clients handle it). New pipeline work;
   only if 1 and 2 prove not enough.

### Where the adapter runs, and how AgentRaaS reaches it

The browser holds a logged-in seller session, so it must stay on the
seller's machine and never be exposed publicly. Paths:

- **`agentraas wrap` around the adapter** (local, works today): dedup and a
  local log only. Enough for the first experiment.
- **Self-hosted AgentRaaS next to the adapter**: blocked today by the
  private-address guard. Allowing one named internal target per org (an
  explicit allow-list, off by default) is a separate design and a security
  review, not something to slip in here.
- **A tunnel from the seller's machine** (`agentraas tunnel` exists for
  local development; whether it can carry MCP calls to a registered server
  is unchecked).

### Screenshots, snapshots and redaction

- The adapter returns structured results only (`before`, `after`,
  `verified`), never a screenshot or page snapshot, so customer data on the
  page never enters AgentRaaS's audit log, recordings or dead-letter queue.
- The adapter keeps its own screenshot per write (before and after), on the
  seller's machine, for a fixed retention, and returns only its id and
  SHA-256 so the audit row points at the evidence without holding it.
- Read tools that must return page text run it through the adapter's own
  field list (SKU, price, stock), not the whole page.

### Logins

Seller portals typically log in with a phone OTP; we have not checked each
of the three. The adapter keeps one persistent browser profile per portal
and stops with a clear error ("log in again") when the session ends. It
never tries to solve an OTP or a CAPTCHA. Portal passwords, if any, stay in
the adapter's own store, not in AgentRaaS.

### Terms of service

Automating these portals with a browser very likely conflicts with their
seller terms. Marketplace seller agreements commonly forbid automated
access, scraping or bots, and the realistic consequence is a suspended
seller account, which for a quick-commerce seller is the business. We have
not read the three agreements for this doc, and this is not legal advice.
Consequences for the design and the copy:

- AgentRaaS never ships the adapter as a product or promotes portal
  automation. Like the trading pages, any copy addresses sellers whose
  agents already drive these portals and says what AgentRaaS adds (fewer
  duplicate or wrong changes, a record of each one), with this risk stated.
- Sellers should prefer an official route where one exists (a partner
  integration or bulk-upload file the platform supports), and ask their
  account manager before automating.

## Failure modes

| Failure | Behaviour |
|---|---|
| Portal page changed | Script can't find its element: error, no click. Agent and person see "adapter out of date". |
| Save clicked, response lost | Outcome unknown; slot kept. `get_price` settles it; a retry is a no-op by read-back. |
| Read-back shows a different value | Error with `before`/`after`; nothing retried automatically. |
| Session expired, OTP needed | Error "log in again"; no retry loop. |
| Two agents, same SKU | Record lock: one runs, the other gets `409` with `retry_after_ms`. |
| Same value set twice on purpose | Needs a fresh `idempotency_key`, else answered from cache. |
| Approval needed over MCP | Refused (fails closed) until the agent uses the SDK/webhook path. |
| Portal rate-limits or flags the account | Out of our control; the circuit breaker stops a failing adapter, but the account risk stays. |

## Smallest experiment first

Goal: learn whether intent-level tools plus read-back give reliable,
repeatable writes on one real portal, before any AgentRaaS server work.

1. One portal where Sumedh (or a willing seller) has a test or low-stakes
   account, and one SKU nobody buys.
2. A throwaway adapter with two tools: `get_price {sku}` and
   `set_price {sku, price}` (read-before, save, read-back), Playwright,
   persistent profile, run under `agentraas wrap` in Claude Desktop or any
   MCP client. No server registration, no approvals.
3. Run it: 20 price changes between two values over several days,
   including a forced timeout (kill the adapter after "Save") and a retry,
   and one identical repeat to see dedup answer it.
4. Record: how many writes verified on the first try, how often the script
   broke on a page change, whether the session survived a day, and whether
   the portal showed any warning about automated use.

What kills the idea:

- The account gets a warning, a CAPTCHA wall or a suspension during the
  experiment. Stop: the ToS risk is real, not theoretical.
- Read-back can't be made reliable (the portal shows the new price before
  it is saved, or caches the old one), so a write can't be verified.
  Without verification there is no safe retry and nothing for AgentRaaS to
  add.
- The script breaks more often than a seller would tolerate fixing it
  (say, the selectors need changing more than once in the test period),
  and option B (LLM inside the intent) doesn't do better.

If it survives, next: register the adapter as a custom action reached
through the SDK so approvals work, add `resource_id` and a `max_from`
price policy, and decide the internal-address question with a security
review.
