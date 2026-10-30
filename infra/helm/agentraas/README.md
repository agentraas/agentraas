# AgentRaaS Helm chart

Runs the AgentRaaS API (the free self-host image; Team features on, Enterprise with a key) on Kubernetes, with a
bundled single-instance Postgres and Redis, or your own.

```bash
helm install agentraas ./infra/helm/agentraas \
  --set publicUrl=https://agentraas.example.com \
  --set ingress.enabled=true --set ingress.host=agentraas.example.com
```

With a managed database and Redis:

```bash
helm install agentraas ./infra/helm/agentraas \
  --set postgresql.enabled=false --set externalDatabase.url=postgres://user:pass@host:5432/agentraas \
  --set redis.enabled=false --set externalRedis.url=redis://host:6379
```

What it does:

- Generates `JWT_SECRET`, `CREDENTIALS_ENCRYPTION_KEY` and the database
  password on first install and keeps them on upgrade. Or bring your own
  Secret with `existingSecret`. **Back up `CREDENTIALS_ENCRYPTION_KEY`**:
  stored third-party credentials can't be decrypted without it. To keep
  it in Vault, AWS KMS, Google Cloud KMS or Azure Key Vault instead, see
  `values.yaml` and https://agentraas.io/docs#kms.
- Runs every migration before the API starts: an init container runs
  `agentraas-migrate` from the AgentRaaS image itself (chart 0.7.1 and
  later), under a Postgres advisory lock so replicas don't race. The
  migrations always match the image version, and an external database
  needs no Postgres image. All migrations are safe to re-run.
- Mounts `config/services.json` (the built-in service catalog) as a
  ConfigMap. It is a symlink into the repo, so the chart never drifts
  from it.
- Runs the API with a read-only root filesystem and probes on `/health`.
- `/metrics` (Prometheus) is on when `metricsToken` is set; scrape it with
  that token as a bearer token.

See `values.yaml` for every option. Set `clientIpHeader` to the header your
ingress writes with the client IP (e.g. `x-real-ip` for ingress-nginx),
otherwise every client shares one rate-limit bucket.

For Redis over TLS, give `externalRedis.url` a `rediss://` URL (image 0.30.2
and later; the certificate must come from a public CA). For agents that call
services on your own network, list those ranges with `extraEnv`, e.g.
`--set-string extraEnv.ALLOWED_PRIVATE_NETWORKS=10.20.0.0/16`; see
https://agentraas.io/docs#private-platforms.

Verified: `helm lint`, `kubeconform -strict` on both value sets, the
migration step against an empty Postgres (twice concurrently, then again),
and the API image running read-only with only the ConfigMap mounted. Not
yet run on a real cluster.
