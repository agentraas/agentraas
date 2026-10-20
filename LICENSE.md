# Licensing

AgentRaaS's core is open source and free to self-host for any purpose,
with no action limit. The Team and Enterprise features are
source-available: Team features are free to self-host in production,
Enterprise features need a license key. These licenses cover different parts of this repository:

| Part | License | File |
|---|---|---|
| The server: everything under `src/api-gateway-rs/` except the files in the next row | GNU AGPL-3.0 | [LICENSE-AGPL](./LICENSE-AGPL) |
| Team/Enterprise features: `src/api-gateway-rs/crates/api/src/ee/`, `src/api-gateway-rs/crates/core/src/semantic_dedup.rs`, `src/api-gateway-rs/crates/core/src/hmac_verify.rs`, `compose.ee.yaml` | AgentRaaS Enterprise License, from server 0.16.0 (`dlp.rs`, PII redaction, was in this row for 0.16.0 to 0.16.4 and is AGPL-3.0 again after them; earlier releases: FSL-1.1-ALv2 for `ee/`, `dlp.rs`, `hmac_verify.rs`, `compose.ee.yaml`; AGPL-3.0 for `semantic_dedup.rs`) | [LICENSE-EE.md](./LICENSE-EE.md) ([LICENSE-FSL.md](./LICENSE-FSL.md) for the older releases) |
| Everything else: the SDKs (`src/sdk`, `src/sdk-js`), `src/chaos-action`, `integrations/`, `infra/`, `compose.yaml`, docs | MIT or Apache-2.0, your choice | [LICENSE-MIT](./LICENSE-MIT), [LICENSE-APACHE](./LICENSE-APACHE) |

In plain words (the license files are what counts):

- **AGPL-3.0 (server).** Use, modify and run it for anything. If you let
  other people use a modified version over a network, you must offer them
  your modified source.
- **Enterprise License (Team/Enterprise features, 0.16.0 on).** Read,
  modify and try them for free. Team features are free in production on
  your own servers, no key needed. Enterprise features need an Enterprise
  license key (ask hello@agentraas.io). Without one, Enterprise features
  answer 403 and everything else works with no limit. No hosted competing
  service.
- **FSL (the same files in releases 0.10.0 to 0.15.1).** Those releases
  stay under the FSL: self-host for anything except a competing hosted
  service, Apache-2.0 two years after each release.
- **MIT/Apache-2.0 (SDKs and the rest).** No conditions beyond keeping the
  notice.

Releases up to and including server 0.9.2 keep the license they were
released under (MIT/Apache-2.0 for the core).

Questions: support@agentraas.io.
