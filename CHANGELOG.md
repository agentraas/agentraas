# Changelog

All notable changes to the AgentRaaS server (`src/api-gateway-rs`) are
recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
and versions follow [Semantic Versioning](https://semver.org/). Until 1.0.0,
a minor version may contain breaking changes; each one is listed under
**Breaking** with the steps to migrate.

The SDKs are versioned separately: `agentraas` on PyPI (`src/sdk`) and
`agentraas` on npm (`src/sdk-js`).

Every release that adds a file under `infra/migrations/` says so. Apply
migrations in order before starting the new version; they are written to be
safe to re-run.

## [Unreleased]

### Fixed

- With `ALLOWED_PRIVATE_NETWORKS` set, `.local` hostnames (Kubernetes
  `*.svc.cluster.local`) are accepted when they resolve inside a listed
  range; they were refused by name before the address check.

## [0.30.1] - 2026-10-30

### Added

- `ALLOWED_PRIVATE_NETWORKS` (self-hosted only): CIDR ranges agents may
  reach, for services that only have an internal address (air-gapped or
  internal networks). Applies to built-in services, custom actions, MCP
  servers and alert webhooks. Loopback and link-local (cloud metadata) stay
  blocked whatever is listed; ignored on Cloud.

### Fixed

- Helm chart 0.7.1: the migration step runs `agentraas-migrate` from the
  AgentRaaS image instead of `psql` from `postgresql.image`, so an external
  database needs no Postgres image, and migrations always match the image
  version. The migrations ConfigMap is gone.

## [0.30.0] - 2026-10-30

New migration: `063_record_locks_defaults.sql`. Apply it before starting
this version.

### Breaking

- Record locks are on by default. An org that never touched the switch now
  gets `409 resource_locked` when two calls change the same record at the
  same moment. To keep the old behaviour, turn it off in Rules, Record
  locks (or `PUT /api/v1/record-locks` with `"enabled": false`), or send
  `lock_wait_ms` so the second call waits instead.
- With the Enterprise build, only org admins can create, delete or promote
  shadow rules (members with write access could before).

### Added

- `lock_wait_ms` (webhook body, `X-AgentRaaS-Lock-Wait-Ms` header, MCP
  argument; up to 5000): wait for a busy record instead of getting the 409
  at once. An approved held call waits up to 5 s before going to Failed
  requests.
- Custom actions can name the record they change when registered:
  `"lock": {"record": "order", "from": ["order_id"]}`.
- The dashboard's Waiting for approval list and the approval link page say
  when another call changed the record after the call was held
  (`record_changed` in `/api/v1/hitl/pending` and `/v1/hitl/link`).
- Terraform for Azure (`infra/azure`: Container Apps, PostgreSQL Flexible
  Server, Cache for Redis, Key Vault) and Google Cloud (`infra/gcp`: Cloud
  Run, Cloud SQL, Memorystore, Cloud KMS, Secret Manager). Validated, not
  yet applied to a real account.
- Docs: Private platforms (VMware, Nutanix, Proxmox, Hyper-V, OpenShift,
  air-gapped), none tested yet.

### Fixed

- Azure Key Vault (KMS and `azure-kv://` secret references) now gets its
  managed identity token on Container Apps and App Service
  (`IDENTITY_ENDPOINT`), not only from IMDS.

## [0.29.0] - 2026-10-29

New migration: `062_record_locks.sql`. Apply it before starting this
version.

### Added

- Record locks: with an org's switch on (Rules, Record locks; off by
  default), two calls that change the same record at the same moment (a
  Stripe customer, a HubSpot contact, a Razorpay payment) no longer race:
  the second gets `409` with `code: "resource_locked"`, the agent holding
  it and `retry_after_ms`. Which record each catalog action changes is in
  `config/services.json` (`"lock"`). An approved held call takes the lock
  again before it runs; shadow calls report `would_wait_for_lock` and never
  take one. API: `/api/v1/record-locks`.

### Changed

- Shadow mode now predicts an open circuit breaker, a used-up plan, an
  in-flight or timed-out duplicate and a reused Idempotency-Key the way the
  live call answers them. An approved held call or a dead-letter replay of
  an agent in shadow mode is not sent.
- Savings reports count each payment once in "money protected", however
  often it was retried (`payments` replaces `duplicates` in that section),
  count an approved call once, and leave approval denials out of the block
  rate. The monthly email claims one org at a time.
- Existing dashboard totals and the public ledger leave shadow calls out.
- The resource-lock 409 body has `code`, `held_by_agent` and
  `retry_after_ms` next to `error`.

### Security

- mockpay's test hit counter only accepts short identifiers.
- White-label display names may not contain control characters.
- `rustls-webpki` 0.103.16 and `webpki-roots` 1.0.11 (RUSTSEC-2026-0382,
  RUSTSEC-2026-0381).

## [0.28.1] - 2026-10-29

No new migration.

### Fixed

- A resource lock (`resource_id`) could expire while its call was still
  running: its lifetime was a fixed 15 seconds, shorter than the default
  30-second proxy timeout, so a second agent could act on the same record
  during a slow call. It now lasts as long as the longest possible forward
  (every attempt timing out, plus backoff) plus 5 seconds, and is still
  released as soon as the call ends.

## [0.28.0] - 2026-10-29

New migration: `061_reports_and_shadow.sql`. Apply it before starting this
version.

### Added

- Shadow mode: put an agent (or one service or action of it) in shadow
  mode and its writes are checked against your rules and recorded, but
  never sent; its stored keys are not used. The agent gets a placeholder
  answer marked `"shadow": true` with what would have happened (run,
  duplicate, blocked, needs approval). Reads are still sent unless blocked
  too. Works on the webhook, SDK and MCP. Dashboard: Rules, Shadow mode and
  Monitor, Shadow calls. API: `/api/v1/shadow-rules`, `/api/v1/shadow-calls`.
- Savings reports: duplicates caught, calls blocked by policy or spend cap,
  approvals, per-agent and per-service rates, top duplicated actions, and
  an estimate of the money in duplicate payments stopped, per currency.
  JSON, CSV or a self-contained HTML page (`/api/v1/reports`), and an
  opt-in monthly email. Enterprise: white-label name, logo and the new
  `accent_color` on the page and email.

### Changed

- Custom-action secret headers are decrypted only when the call is sent,
  not when the route is looked up.

## [0.27.1] - 2026-10-28

No new migration.

### Changed

- A service with `authType: "login"` that answers 401 (a token revoked
  before it expired) gets the same call once more with a fresh login, so the
  agent no longer sees that one 401. A 401 means the call was refused, not
  run, so it can't run twice.

## [0.27.0] - 2026-10-28

No new migration.

### Added

- **Keys that expire, renewed by the server.** A catalog service with
  `authType: "login"` and a `tokenLogin` block trades the saved credentials
  for a token, keeps it in Redis (encrypted, like stored credentials) until a
  minute before it expires, and logs in again after a 401. Wrong credentials
  answer 424 with the reason and don't count against the circuit breaker.
- **PayPal** now logs in with the app's client ID and secret (and is in
  Connect a service); `paypal_sandbox` for sandbox apps. An access token
  saved earlier as a plain key is still sent as is.
- **Shiprocket** (create orders, assign a courier, track an AWB, cancel) and
  **PhonePe test mode** (`phonepe_sandbox`: order status, refund, refund
  status).

## [0.26.0] - 2026-10-28

No new migration.

### Added

- **Shipping and SMS for India:** Delhivery (create shipments, track a
  waybill, check a pincode; a separate `delhivery_staging` for test tokens)
  and MSG91 (send SMS from a DLT-approved template).
- A catalog `authType` of `token`, which sends `Authorization: Token <key>`
  (Delhivery's form).

## [0.25.0] - 2026-10-26

No new migration.

### Added

- **Limits the server looks up (`max_from`).** An action policy field can be
  held to a number AgentRaaS reads from the provider right before the call,
  with the org's own key, instead of one the agent sends: "a refund at most
  half of what the order cost". Optional `factor`, `minus` (such as Razorpay's
  `amount_refunded`) and `service` (read from another service, such as a
  custom action on the platform's own order API). A missing amount, a missing
  id, a failed read or an answer without the number is a violation, so it
  blocks or goes to approval like any other: nothing unchecked is let through.
  Blocked calls are logged as `policy_lookup_limit`.
- **Services for India:** Cashfree Payments (orders, refunds; a separate
  `cashfree_sandbox` for test keys), RazorpayX Payouts, Gupshup (WhatsApp),
  and `payment.get` / `payment.refund` for Razorpay.
- The sandbox `mockpay` has `order.get`, stateless: an order id ends in its
  amount (`order_1200` is an order of 1200).

### Changed

- A curated `GET` action sends what is left of the payload as a query string
  instead of a JSON body.
- A service's `extraHeaders` value `{idempotency_key}` is replaced by the same
  stable key sent as `Idempotency-Key`, for providers with their own header
  (RazorpayX requires `X-Payout-Idempotency` on every payout).

## [0.24.1] - 2026-10-26

### Fixed

- A call to a service with no key saved for the org now answers 424 with the
  real message ("No credentials configured for slack...") at once. Before, it
  was retried as if the provider had failed, answered 500 "An internal error
  occurred", and counted against that service's circuit breaker, which every
  org shares: one org without a Slack key could open Slack's breaker for all.

## [0.24.0] - 2026-10-26

No new migration. No change in behavior.

### Changed
- **License.** The server core is now MIT (was AGPL-3.0), and the SDKs and
  everything else are MIT only (was MIT or Apache-2.0). The Team/Enterprise
  features stay under the AgentRaaS Enterprise License. Two licenses in
  total: `LICENSE-MIT` and `LICENSE-EE.md`; `LICENSE-AGPL`, `LICENSE-APACHE`
  and `LICENSE-FSL.md` are removed. Earlier releases keep the license they
  shipped under (see `LICENSE.md`).

## [0.23.1] - 2026-10-22

No new migration.

### Changed
- **Breaking for anyone pointing at port 13001:** the API's host port is now
  13000 (`compose.yaml`), the port the SDKs, `.env.example` and the
  self-host download already used. `./install.sh` recreates the containers
  and moves a `PUBLIC_URL` of `http://localhost:13001` to 13000; update
  reverse proxies, tunnels and scripts that point at `:13001` yourself.

### Fixed
- `GET /api/v1/recent` (the audit log behind the dashboard's activity list)
  returns at most 1,000 rows; any larger `limit` used to read every row the
  caller's orgs have. The CSV export (newest 10,000) is unchanged.

## [0.23.0] - 2026-10-21

No new migration; `001`, `005` and `057` changed only so they re-run safely.

### Added
- **AWS template** (`infra/aws`, Terraform): ECS Fargate behind a load
  balancer, RDS Postgres, ElastiCache Redis, a KMS key for the credentials
  master key, Secrets Manager. Checked with `terraform validate` and
  tflint; not yet applied to a real account.
- The image runs on its own: it now carries the service catalog and the
  migrations, plus `agentraas-migrate` (psql under an advisory lock) to run
  before the API on platforms without compose or Helm. It builds from the
  repo root (`podman build -f src/api-gateway-rs/Containerfile .`);
  `Containerfile.render` is gone, Render uses the same file.
- `CLIENT_IP_HEADER=x-forwarded-for:last` reads the last entry, the one a
  load balancer that appends (AWS) added. Plain `x-forwarded-for` still
  takes the first entry, which the client controls.

### Changed
- A KMS may hold the credentials key as its base64 text as well as the raw
  32 bytes (Terraform and most CLIs pass text).
- The self-host download now includes `LICENSE-EE.md`.

### Fixed
- Re-running the whole migration set failed: `001` created three indexes
  without `IF NOT EXISTS`, `005` updated audit rows after `024` made them
  append-only, and `057` narrowed the alert-type check that `058` widens.
  All three are now safe to re-run on a new or an existing database, as the
  Helm chart and `agentraas-migrate` do on every start.

## [0.22.1] - 2026-10-21

No migration.

### Changed
- Fetching secret references moved to `crates/api/src/ee/`, under the
  AgentRaaS Enterprise License like the other Enterprise features. 0.22.0
  shipped it under AGPL-3.0 and that release stays so. A build without the
  `enterprise` feature recognises a reference and refuses it.
- The Helm chart's default image is the current release (it pointed at
  0.9.0, which no registry has).

## [0.22.0] - 2026-10-21

No migration.

### Added
- **Secret references (self-hosted Enterprise).** A service credential can be a
  reference such as `vault://secret/data/stripe#api_key`,
  `aws-sm://<secret ARN>`, `gcp-sm://projects/p/secrets/s` or
  `azure-kv://<vault>.vault.azure.net/secrets/s` instead of the key. The
  key is fetched from your secrets manager when a call needs it, cached in
  memory for 5 minutes (`SECRET_REF_CACHE_SECONDS`) and never stored. Not
  available on Cloud, where the server's access to a secrets manager would
  be shared between customers. See `/docs#secret-refs`.

## [0.21.0] - 2026-10-21

No migration.

### Added
- **KMS key management.** `CREDENTIALS_ENCRYPTION_KEY` can now hold the key
  encrypted by HashiCorp Vault / OpenBao (transit), AWS KMS, Google Cloud
  KMS or Azure Key Vault instead of the plain key: set
  `CREDENTIALS_KEY_PROVIDER` (`vault`, `aws`, `gcp`, `azure`) and
  `CREDENTIALS_KMS_KEY`. The server decrypts it once at startup and refuses
  to start if it can't. Stored data is unchanged, so an existing install
  switches over without re-encrypting anything. Unset, nothing changes.
  Steps per provider: `/docs#kms`.

### Security
- Dependency updates from `cargo audit`: `zip` 0.6 to 9 (three advisories,
  only reachable when extracting, which AgentRaaS never does), `rustls`
  0.23.46 and `quinn-proto` 0.11.19. The remaining `rsa` advisory comes in
  only through sqlx's unused MySQL driver and has no fix yet.

### Changed
- **Self-hosted Team features are free, with no license key.** A server
  without `LICENSE_TOKEN` now runs as Team: human approvals, approval holds
  on spend caps and policies, semantic dedup, OIDC SSO, up to 3 seats. Only
  Enterprise features (agent identity, inbound webhooks, white-label,
  unlimited seats) need a key; ask hello@agentraas.io. `LICENSE-EE.md` and
  `LICENSE.md` are reworded to match. Cloud plans are unchanged.
- **`install.sh` works with Docker as well as Podman**, so it runs on
  macOS (Docker Desktop) and Windows (Docker Desktop with WSL2, run from a
  WSL terminal). Podman is used when both are installed. `.gitattributes`
  keeps shell scripts in LF so a Windows checkout still runs.

### Fixed
- `install.sh` waited for Postgres's init server instead of the real one,
  so a first install on a fresh volume could run migrations before the
  `agentraas` database existed ("database does not exist").

## [0.20.0] - 2026-10-17

No migration.

### Added
- Indian brokers Upstox and Dhan as built-in services: place, modify and
  cancel NSE/BSE orders, with each order run once, side/product/order
  type/validity/segment checked before the broker sees it, and undo
  cancelling a placed order. Live orders need self-hosting on a static IP
  (SEBI's retail algo rules); see `/trading#india`.

### Fixed
- An alert channel that rejects a message (a deleted Teams workflow, a
  revoked bot) is now logged instead of failing silently.

## [0.19.0] - 2026-10-17

Migration: `060`.

### Added
- Daily money caps: a spend cap can add up a payload field per hour or day
  (for example 10,000 in `notional` a day), with or instead of a call count.
  A call without a number in that field is refused, a refused call doesn't
  use up the budget, and a cap can name a time zone.
- Allowed hours on action policies (for example weekdays 09:30 to 16:00
  America/New_York), daylight saving handled; outside them a call is
  blocked or held for approval. In the dashboard, the API and
  `agentraas apply`.

## [0.18.0] - 2026-10-17

Migrations: `058`, `059`.

### Added
- Approval links: a held call sends a one-time link to every alert channel
  (Microsoft Teams, Telegram, email, Discord and Slack webhooks, alongside
  the Slack app). Opening it only shows the request; Approve or Deny
  decides. Held calls can also be approved in the dashboard (Monitor →
  Waiting for approval). The first decision wins.
- Alpaca paper and live trading as built-in services.
- One AgentRaaS key per agent works for every connected service, with
  per-key switches to limit it to chosen services.
- Teams and email alert channels; dashboard charts follow the org filter.

### Security
- A policy limit or approval threshold now also applies when the number
  arrives as a string (`"5000"`). Before, a quoted number skipped it.
  Upgrading is recommended.

## [0.17.0] - 2026-10-13

Migration: `057`.

### Added
- Validation errors carry a concrete `hint` for the agent (for example
  "send amount as a JSON number: 49.99").
- Jira as a service (`jira.issue.create/comment/transition/delete`; create
  is undoable) and as an alert channel (one ticket per org per alert per
  day).
- An alert on every channel when a call's outcome is unknown (once an hour
  per action).

## [0.16.5] - 2026-10-13

Migration: `056`.

### Security
- An org auditor could delete or change rules, policies, caps, webhooks and
  gateway targets, revoke identity tokens and replay DLQ entries; by-id
  writes now check write access, not just membership.
- The public demo session could fetch a license key and the self-host
  package; it no longer can.
- The Paddle webhook trusted a browser-written user id; the subscription's
  first owner now decides.
- `{ACCOUNT}` in a URL is now character-checked.
- A per-account cap of 50 failed logins an hour from all IPs (a password
  reset clears it).

### Changed
- Emails are case-insensitive (migration 056).
- **License.** `dlp.rs` (PII redaction) is AGPL-3.0 again, part of the free
  Community edition. It was listed under the Enterprise License in 0.16.0
  to 0.16.4.

## [0.16.4] - 2026-10-11

No migration.

### Security
- A Custom Action URL containing `{NAME}` was filled from the server's
  environment before the call, so a user could send server secrets (such as
  `JWT_SECRET`) to a host they control. Custom Action and webhook URLs are
  now used exactly as stored. Upgrade, and rotate `JWT_SECRET` and
  `CREDENTIALS_ENCRYPTION_KEY` if untrusted users could create Custom
  Actions on your server.

### Added
- Dashboard: Connect a service. Pick a service, paste its API key, and get
  the AgentRaaS URL, key and a ready example call, plus the actions it
  supports and their required fields.
- Services: Resend, SendGrid, GitHub (issues), Notion (pages), Zendesk
  (tickets); Stripe PaymentIntents, refunds of a PaymentIntent,
  subscriptions (create, cancel, with undo) and invoices; HubSpot deals and
  contact updates. 27 services in total.
- A curated service's URL can take `{field}` from the call (removed from the
  body; letters, digits, `.`, `_`, `-` only) and `{ACCOUNT}` from the stored
  credential's username.

### Fixed
- Twilio's URL used the server's `TWILIO_SID` env var instead of the
  customer's Account SID, so Twilio could not work for Cloud customers.
- Shopify's URL contained a literal `YOUR_STORE`; calls now send `shop`.
- Stripe form bodies sent nested values as JSON text; they now use Stripe's
  bracket form (`items[0][price]`).
- The Plugins window no longer promises a hosted gateway that does not exist.

## [0.16.3] - 2026-10-11

No migration.

### Added
- Public read-only demo: `GET /api/v1/demo/session` (Cloud only) signs a
  2-hour session for the demo account (`users.is_demo`) and opens the
  dashboard. The session refuses every request except GET/HEAD and the live
  dedup test, has its own per-visitor rate limit, and is limited per IP.
  The homepage has a "See the live demo" button.
- Connect agent: Python and MCP client config snippets with the new key
  filled in, and a notice when the agent's first call arrives.

### Fixed
- The curl and n8n examples shown after connecting an agent called
  `mockpay` `charge.create`, which does not exist, so every new user's first
  test call failed; they use `payment.create` now.
- The `/docs` MCP client example had no `x-agentraas-key` header.

## [0.16.2] - 2026-10-11

No migration.

### Added
- Dashboard: click an activity row for its details: why it was
  deduplicated, blocked or failed in plain words, request ID, dedup hash,
  end user, run and step, the redacted payload preview when there is one,
  and a link to the original call a duplicate repeated. `/api/v1/recent`
  now returns `payload_hash`, `payload_preview` and `end_user_id`.
- Dashboard: a three-step first-run card, Setup/Rules/Monitor/Account tabs
  instead of every button at once, an Account and billing button, an org
  filter in the activity list when you belong to more than one org, and
  Team/Enterprise tags on buttons your plan or license does not include.

### Changed
- Dashboard: only services with calls (or a failing circuit) are listed,
  with proper names; older activity rows show their date; "HITL rules" is
  "Approval rules"; Refresh sits next to the time range; page title and
  tagline match the site.

### Fixed
- Dashboard: four `innerHTML` sites inserted values without `escapeHtml`.

## [0.16.1] - 2026-10-11

No migration.

### Fixed
- A license key for a subscription whose billing period had ended was
  signed with an expiry equal to its issue time, so it was dead on arrival
  and the self-hosted server silently fell back to Community. Such a request
  now answers 409, and every key stays valid 7 days past the period's end so
  a renewal can be pasted in without losing features.
- Account panel: a paid account no longer sees the free-trial pitch; the
  usage line says overage is not charged while overage billing is off;
  white-label branding shows only on Enterprise (and the Cloud trial).

## [0.16.0] - 2026-10-11

No migration.

### Breaking
- **Self-hosted Team and Enterprise features need a license key again.**
  Without `LICENSE_TOKEN` the server runs as the free Community edition:
  no action limit, one dashboard seat, blocking spend caps and action
  policies included; human approvals (HITL rules, and the "hold for
  approval" action of spend caps and policies), semantic dedup, SSO
  configuration and invites (Team) and agent identity, inbound webhooks,
  white-label branding (Enterprise) answer 403. A Team ($19/month) or
  Enterprise ($29/month) plan shows the key under Account in the dashboard.
  A missing, invalid or expired key falls back to Community; the server
  always starts.
- **License.** From this release, `ee/`, `semantic_dedup.rs`, `dlp.rs`,
  `hmac_verify.rs` and
  `compose.ee.yaml` are under the AgentRaaS Enterprise License
  (`LICENSE-EE.md`). 0.10.0 to 0.15.1 keep the licenses they shipped under.
- **Cloud billing.** A free Cloud org gets 1,000 actions once (was 500 a
  month), with every feature. Team includes 10,000 actions a month,
  Enterprise 20,000; above that, $1 per 500 actions up to the org's monthly
  cap. `CLOUD_MONTHLY_LIMIT` is replaced by `CLOUD_TRIAL_ACTIONS`;
  `PADDLE_PAYG_PRICE_ID` is gone, `PADDLE_ENTERPRISE_PRICE_ID` is new.

### Added
- Dashboard: **Monitor → Recordings** lists recordings, shows each call's
  payload and response, and deletes a recording.
- Dashboard Account: Team/Enterprise checkout, trial usage, and the
  self-hosted license key.

### Fixed
- The homepage's PyPI version badge was blocked by the CSP since 0.12.0;
  `connect-src` now allows `https://pypi.org`.
- The Paddle webhook set the plan from `custom_data`, which the buyer's
  browser writes; it now uses the price actually paid.

## [0.15.1] - 2026-10-09

No migration.

### Added
- **Record and replay over MCP.** `tools/call` honours the same
  `X-AgentRaaS-Record` / `X-AgentRaaS-Replay` headers as the webhook and SDK
  endpoints; the run id comes from the `run_id` tool argument or
  `X-AgentRaaS-Run-Id`.
- SDKs: Python 0.10.0 `Client(record=, replay=, run_id=)`, JS 0.4.0
  `record` / `replay` / `runId` client options.

## [0.15.0] - 2026-10-08

Migration: `055_recordings.sql` (new `recordings` table). Apply it before
starting 0.15.0.

### Added
- **Record and replay.** `X-AgentRaaS-Record: <name>` on a webhook or SDK
  call keeps every successful call under that name for 30 days (payload and
  response encrypted). `X-AgentRaaS-Replay: <name>` answers the same calls
  from the recording, in order per `service.action`, without forwarding,
  deduplicating or counting usage; each answer carries
  `replay: { recording, position, payload_matches }`, and a call the
  recording doesn't have gets `404`. The position restarts with a new
  `X-AgentRaaS-Run-Id`. `GET /api/v1/recordings?org_id=`,
  `GET|DELETE /api/v1/recordings/:name?org_id=`.

## [0.14.0] - 2026-10-07

Migration: `054_undo_log.sql` (new `undo_log` table; `undo_action` and
`undo_with` columns on `custom_actions`). Apply it before starting 0.14.0.

### Added
- **Undo log.** Every action that ran and has a known reverse is recorded
  for 30 days and can be undone from the dashboard (Monitor → Undo actions)
  or `POST /api/v1/undo-log/:id/undo`; `GET /api/v1/undo-log?org_id=` lists
  them. Built in: Stripe `charge.create` → `charge.refund`, Slack
  `message.post` → `message.delete`, mockpay `payment.create` →
  `payment.refund`. Custom actions take an `undo` field naming another
  custom action and how to fill it (`response.<path>` / `payload.<path>`).
  Each entry is undone at most once (atomic claim, `Idempotency-Key:
  agentraas-undo-<id>`); no answer from the provider marks it unknown and
  it is not retried. Recorded from the webhook, SDK and MCP paths, approved
  HITL calls and DLQ replays. Undos are audited with status `undo` and never
  billed.
- `stripe charge.refund` and `slack message.delete` curated actions.

## [0.13.0] - 2026-10-07

No migrations.

### Changed
- **No plans or feature tiers: every account gets every feature.** Free
  Cloud accounts now get approvals (HITL), SSO and member management,
  identity tokens, inbound webhooks, branding and semantic dedup, with no
  seat limit, like self-hosted servers. Cloud is priced only by usage: 500
  actions a month free, then $1 per 1,000 that ran (billing switches on
  later; until then the free 500 applies). Plans differ only in limits.
- Dashboard: one console for everyone; the Team upsell and the header
  upgrade badge are gone; Account describes what the account gets.
- Website, docs, Terms and Privacy: pricing is "Open source, free" and
  "Cloud, pay as you go"; the Team and Enterprise cards are gone (contracts
  and SLA support are a contact line).

## [0.12.1] - 2026-10-07

No migrations.

### Security
- **On Cloud, every call needs a real agent key.** An org that had never
  created a key accepted calls with any key or none (a first-run
  convenience), so anyone could run calls under an org_id nobody had
  registered. That shortcut now applies to self-hosted servers only
  (`DEPLOYMENT_MODE` other than `cloud`).

## [0.12.0] - 2026-10-07

No migrations.

### Changed
- **One request pipeline for webhook, SDK and MCP calls.** MCP `tools/call`
  used to run its own copy of the pipeline, which had fallen behind. MCP
  calls now also get: scoped agent identity tokens (`art_live_`), the
  `resource_id` lock (new optional tool argument), schema-drift detection,
  custom-action fan-out and circuit-open notifications. MCP error texts now
  match the webhook's. An MCP call that would need human approval is still
  refused (it can't wait), with the same message as before.

### Security
- The CSP no longer allows inline scripts. Every page's JavaScript moved
  to `public/js/` (the dashboard app is `js/dashboard.js`), served at
  `/js/<name>.js`; the edge Worker sends the same policy.

## [0.11.0] - 2026-10-07

Migrations: `052_payg_billing.sql`, `053_session_revocation.sql`. Apply both
before starting this version.

### Security
- **Sessions can be revoked server-side.** Logout now denies that session
  token until it expires, instead of only clearing the cookie (a copied
  cookie kept working for 7 days). A password change or reset ends every
  other session of that user; the caller gets a fresh cookie. New
  `POST /api/v1/auth/logout-all` ("Log out everywhere" in Account). A
  deleted user's sessions stop working.
- **Content-Security-Policy** on every response (see SECURITY.md).

### Changed
- sqlx 0.7 to 0.8 (0.7's Postgres driver will be rejected by a future Rust).

### Added
- Pay-as-you-go Cloud billing, off by default (`BILLING_PAYG_ENABLED`): a
  `payg` plan with every feature, $1 per 1,000 actions that ran after the
  free monthly allowance, a per-org monthly cap
  (`GET/PUT /api/v1/billing/usage`), and a monthly Paddle charge job.
  Duplicates, blocked calls and approval waits are never billed. Migration
  `052_payg_billing.sql`.

### Removed
- License tokens: `LICENSE_TOKEN`, `GET /api/v1/licensing/token` and the
  dashboard's "Your license" panel. Self-hosting needs none (0.10.0).
- `GET /api/v1/download/self-host/enterprise-image` and the
  `self-host-artifacts` mount. Everyone downloads the same source package,
  which now includes the Team/Enterprise module and the license files.

### Changed
- A self-hosted dashboard shows every history range and the CSV export.

## [0.10.0] - 2026-10-06

### Changed
- **License.** The server is now AGPL-3.0 (`LICENSE-AGPL`). The
  Team/Enterprise module (`crates/api/src/ee/`, `crates/core/src/dlp.rs`,
  `crates/core/src/hmac_verify.rs`, `compose.ee.yaml`) is under the
  Functional Source License, FSL-1.1-ALv2 (`LICENSE-FSL.md`): free to use and
  self-host for any purpose except a competing hosted service, and Apache-2.0
  two years after each release. The SDKs stay MIT/Apache-2.0. 0.9.2 and
  earlier keep their old license. Summary in `LICENSE.md`.
- **Self-hosting includes every feature.** A self-hosted server is always
  the Enterprise tier: no `LICENSE_TOKEN` needed, and the dashboard shows
  every control. The Render blueprint builds the full edition with
  `ENTERPRISE_MODE=true`. No migrations.

## [0.9.2] - 2026-10-06

### Security
- **DNS rebinding past the SSRF guard.** A destination URL was checked when
  it was resolved, but the HTTP client resolved it again on its own, so a
  record with a zero TTL could pass the check and then connect to a private
  address. The client now drops private and reserved addresses itself, at
  connect time.
- `/api/v1/public/execution-ledger` (unauthenticated) ran a 24-hour count
  on every request. It is now computed at most once a minute.

### Fixed
- A `resource_id` lock is released when the call finishes. It used to be
  held for its full 15 seconds, so a follow-up action on the same resource
  got a 409. It is still held until it expires when the upstream's outcome
  is unknown (a timeout after sending).

No migrations.

## [0.9.1] - 2026-10-01

### Security
- **MCP tool calls skipped action policies and spend caps.** A call made
  through `/mcp` was forwarded even when a deny policy, the `no_secrets` /
  `new_destination` guards or a spend cap should have stopped it. MCP now
  runs the same checks as the webhook path; a rule that would ask for human
  approval blocks the call over MCP. Upgrade if you use action policies or
  spend caps with MCP clients.
- Calls held for approval stored the agent's raw API key in
  `hitl_requests`. They now store a prefix and SHA-256 only.
  **Migration:** run `051_hash_held_call_keys.sql`.
- SSO discovery and the Slack approval callback now go through the SSRF
  guard: an org admin could otherwise point the server at an internal
  address.
- Security headers on every response (`X-Frame-Options: DENY`, `nosniff`,
  `Referrer-Policy`, HSTS in production).
- rustls 0.23.45 (RUSTSEC-2026-0285).

### Fixed
- MCP audit rows recorded every agent as `mcp-agent` instead of the
  verified agent id.
- An internal error in the circuit-breaker, usage or spend-cap check left
  the call's dedup slot claimed, so an identical retry got 409 until the
  lease expired.

## [0.9.0] - 2026-10-01

First versioned release. It covers everything shipped up to this date; the
server's `/health` response now includes its version.

### Added
- `/metrics` in Prometheus text format: `agentraas_calls_total` and the
  `agentraas_call_duration_ms` histogram, labeled by service and outcome.
  Off unless `METRICS_TOKEN` is set; scrape with it as a bearer token.
- OpenTelemetry export of every handled call as an OTLP span
  (`OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_HEADERS`).
- Action policies (allow, deny, require approval per service and action)
  and the prompt-injection guard (`no_secrets`, `new_destination`).
- Timeout-safe forwarding: every forward sends
  `Idempotency-Key: agentraas-<dedup hash>`; a call that times out after
  sending is recorded as "outcome unknown", kept in its dedup slot and never
  retried automatically. `PROXY_TIMEOUT_SECONDS` (default 30).
- Crash recovery for in-flight calls: dedup claims carry a lease.
- Spend and call caps per agent, schema drift detection, a local dev tunnel,
  dead-letter queue with replay, circuit breakers, human approval (HITL).

### Security
- Client IP for rate limiting is read only from `CLIENT_IP_HEADER`;
  `X-Forwarded-For` is no longer trusted.

### Changed
- `compose.yaml` pulls MinIO from `cgr.dev/chainguard/minio` (runs as root
  so existing data stays writable). MinIO's own registries now refuse
  anonymous pulls.

### Migrations
- Up to `050_known_destinations.sql`. On an existing install, apply any of
  `046` to `050` you have not run yet.

[Unreleased]: https://github.com/agentraas/agentraas/compare/v0.27.1...HEAD
[0.27.1]: https://github.com/agentraas/agentraas/compare/v0.27.0...v0.27.1
[0.27.0]: https://github.com/agentraas/agentraas/compare/v0.26.0...v0.27.0
[0.26.0]: https://github.com/agentraas/agentraas/compare/v0.25.0...v0.26.0
[0.25.0]: https://github.com/agentraas/agentraas/compare/v0.24.1...v0.25.0
[0.24.1]: https://github.com/agentraas/agentraas/compare/v0.24.0...v0.24.1
[0.24.0]: https://github.com/agentraas/agentraas/compare/v0.23.1...v0.24.0
[0.23.1]: https://github.com/agentraas/agentraas/compare/v0.23.0...v0.23.1
[0.23.0]: https://github.com/agentraas/agentraas/compare/v0.22.1...v0.23.0
[0.22.1]: https://github.com/agentraas/agentraas/compare/v0.22.0...v0.22.1
[0.22.0]: https://github.com/agentraas/agentraas/compare/v0.21.0...v0.22.0
[0.21.0]: https://github.com/agentraas/agentraas/compare/v0.20.0...v0.21.0
[0.9.2]: https://github.com/agentraas/agentraas/compare/v0.9.1...v0.9.2
[0.9.1]: https://github.com/agentraas/agentraas/compare/v0.9.0...v0.9.1
[0.9.0]: https://github.com/agentraas/agentraas/releases/tag/v0.9.0
