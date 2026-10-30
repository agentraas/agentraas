# AgentRaaS on Google Cloud (Terraform)

Runs AgentRaaS in your own Google Cloud project with managed services:

| Part | Google Cloud service |
|---|---|
| API (the `ghcr.io/agentraas/agentraas` image) | Cloud Run, pulled through an Artifact Registry remote repository |
| Postgres | Cloud SQL for PostgreSQL 16, private IP only, TLS required, 7 days of backups |
| Redis | Memorystore for Redis 7.2, private IP only, reached over Direct VPC egress |
| Master key for stored service keys | Cloud KMS (`CREDENTIALS_KEY_PROVIDER=gcp`, read with the service's service account) |
| Database URL, JWT secret, license key | Secret Manager |
| Logs | Cloud Logging |

Every instance runs `agentraas-migrate` first (under a Postgres advisory
lock), then the API, in the same container.

## Use it

Needs Terraform 1.5+ and `gcloud auth application-default login` for a
project you own (billing on). The template turns on the APIs it needs.

```bash
cd infra/gcp
terraform init
terraform apply -var project_id=my-project -var region=europe-west1
```

Then open the `url` output: the service's own `https://...run.app`
address, with a certificate from Google. For your own domain, put a global
external Application Load Balancer with a serverless network endpoint group
in front of the service (or a Cloud Run domain mapping where your region
has it), and apply again with `-var public_url=https://agentraas.example.com`.

Useful variables (all in `variables.tf`): `license_token` for Enterprise,
`min_instances`, `max_instances`, `db_tier`, `db_high_availability`,
`redis_tier`, `extra_env` for SMTP, OpenTelemetry and the rest,
`deletion_protection` (on by default).

## Keys and state

- **Master key.** Without `credentials_key_ciphertext`, Terraform generates
  the 32-byte key and stores it encrypted by Cloud KMS. Its plain value is
  also in the Terraform state, as are the database and JWT passwords, so
  keep the state in an encrypted backend (a Cloud Storage bucket) with
  limited access. To keep the key out of the state, make it yourself and
  pass the ciphertext:

  ```bash
  terraform apply -target=google_kms_crypto_key.credentials   # the key first
  openssl rand -base64 32 | tr -d '\n' | gcloud kms encrypt \
    --location <region> --keyring agentraas --key credentials \
    --plaintext-file - --ciphertext-file - | base64 -w0   # -> credentials_key_ciphertext
  ```
- **Never destroy the KMS key's versions** (`kms_key_id` output): every
  stored service key depends on them. Terraform refuses to destroy the key
  (`prevent_destroy`), and Cloud KMS key rings and keys can't be deleted, so
  a second install in the same project and region needs a different `name`.
- **Secret references** (Enterprise, `gcp-sm://`): grant the
  `service_account_email` output `roles/secretmanager.secretAccessor` on the
  secrets you name.

## What it leaves out on purpose

- No Cloud NAT: only private ranges go through the VPC; calls to Stripe,
  Slack and the rest leave Cloud Run directly. Add Cloud NAT and
  `egress = "ALL_TRAFFIC"` if you need a fixed outbound address.
- No AUTH on Redis: it has only a private address in the network. Traffic
  to it is TLS, checked against Memorystore's own CA (`REDIS_CA_CERT`,
  image 0.30.3 or later).
- Google-managed encryption keys for Cloud SQL, Memorystore and Secret
  Manager (customer-managed keys need each service's agent granted first).
- One Redis node (`redis_tier = "STANDARD_HA"` adds a replica) and, by
  default, one database zone (`db_high_availability` adds a standby).
- CPU stays allocated between requests (`cpu_idle = false`) so background
  jobs keep running; keep `min_instances` at 1 or more.

Client addresses: Google's front end appends the caller's address to
`X-Forwarded-For`, so the service sets `CLIENT_IP_HEADER=x-forwarded-for:last`.
