# Licensing

AgentRaaS's core is open source and free to self-host for any purpose,
with no action limit. The Team and Enterprise features are
source-available: Team features are free to self-host in production,
Enterprise features need a license key. Two licenses cover this repository:

| Part | License | File |
|---|---|---|
| Team/Enterprise features: `src/api-gateway-rs/crates/api/src/ee/`, `src/api-gateway-rs/crates/core/src/semantic_dedup.rs`, `src/api-gateway-rs/crates/core/src/hmac_verify.rs`, `compose.ee.yaml` | AgentRaaS Enterprise License | [LICENSE-EE.md](./LICENSE-EE.md) |
| Everything else: the server core (the rest of `src/api-gateway-rs/`), the SDKs (`src/sdk`, `src/sdk-js`), `src/chaos-action`, `src/scan-action`, `integrations/`, `infra/`, `compose.yaml`, docs | MIT | [LICENSE-MIT](./LICENSE-MIT) |

In plain words (the license files are what counts):

- **MIT (server core, SDKs and the rest).** Use, modify, run, host and
  redistribute it for anything. Keep the copyright notice.
- **Enterprise License (Team/Enterprise features).** Read, modify and try
  them for free. Team features are free in production on your own servers,
  no key needed. Enterprise features need an Enterprise license key (ask
  hello@agentraas.io). Without one, Enterprise features answer 403 and
  everything else works with no limit. No hosted competing service.

## Earlier releases

This split applies from server 0.24.0. Each earlier release keeps the
license it was released under, and its own license files are in that
release's git tag:

- 0.16.0 to 0.23.x: server AGPL-3.0, Team/Enterprise features under the
  AgentRaaS Enterprise License (`dlp.rs` too in 0.16.0 to 0.16.4), SDKs
  and the rest MIT or Apache-2.0.
- 0.10.0 to 0.15.1: server AGPL-3.0, `ee/`, `dlp.rs`, `hmac_verify.rs`
  and `compose.ee.yaml` under FSL-1.1-ALv2 (Apache-2.0 two years after each
  release), SDKs and the rest MIT or Apache-2.0.
- Up to and including 0.9.2: MIT or Apache-2.0 for the core.

Questions: support@agentraas.io.
