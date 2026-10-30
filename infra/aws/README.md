# AgentRaaS on AWS (Terraform)

Runs AgentRaaS in your own AWS account with managed services:

| Part | AWS service |
|---|---|
| API (the `ghcr.io/agentraas/agentraas` image) | ECS Fargate, behind an Application Load Balancer |
| Postgres | RDS for PostgreSQL 16, private subnets, encrypted, 7 days of backups |
| Redis | ElastiCache for Redis 7, private subnets, encrypted at rest |
| Master key for stored service keys | AWS KMS (`CREDENTIALS_KEY_PROVIDER=aws`, read with the task's role) |
| Database URL, JWT secret, license key | Secrets Manager, encrypted with the same KMS key |
| Logs | CloudWatch Logs, 30 days |

Every task start runs `agentraas-migrate` first (a second container in the
task, under a Postgres advisory lock), then the API.

## Use it

Needs Terraform 1.5+ and AWS credentials for an account you own.

```bash
cd infra/aws
terraform init
terraform apply \
  -var region=eu-west-1 \
  -var public_url=https://agentraas.example.com \
  -var certificate_arn=arn:aws:acm:eu-west-1:111122223333:certificate/...
```

Then point `agentraas.example.com` (a CNAME) at the `load_balancer_dns_name`
output and open the `url` output. Without `certificate_arn` it serves plain
HTTP on the load balancer's own address: fine to try it, not for production.

Useful variables (all in `variables.tf`): `license_token` for Enterprise,
`desired_count`, `db_instance_class`, `db_multi_az`, `extra_env` for SMTP,
OpenTelemetry and the rest, `deletion_protection` (on by default).

## Keys and state

- **Master key.** Without `credentials_key_ciphertext`, Terraform generates
  the 32-byte key and stores it encrypted by KMS. Its plain value is also in
  the Terraform state, as are the database and JWT passwords, so keep the
  state in an encrypted backend (an S3 bucket with KMS) with limited access.
  To keep the key out of the state, make it yourself and pass the ciphertext:

  ```bash
  terraform apply -target=aws_kms_key.this     # the key first
  aws kms encrypt --key-id alias/agentraas --plaintext fileb://<(openssl rand 32) \
    --query CiphertextBlob --output text      # -> credentials_key_ciphertext
  ```
- **Never delete the KMS key** (`kms_key_arn` output): every stored service
  key, the database and Redis are encrypted with it.
- **Secret references** (Enterprise, `aws-sm://`): grant the `task_role_name`
  output `secretsmanager:GetSecretValue` on the secrets you name.

## What it leaves out on purpose

- No NAT gateway: API tasks run in public subnets with public IPs so they
  can reach Stripe, Slack and the rest; only the load balancer can reach
  them. Add a NAT gateway and private subnets if your policy needs it.
- Redis has no password: it is reachable only from the API's security
  group, and traffic to it is encrypted (TLS, image 0.30.2 or later).
- One Redis node and, by default, one database instance (`db_multi_az` adds
  a standby).

Client addresses: the load balancer appends the caller's address to
`X-Forwarded-For`, so the task sets `CLIENT_IP_HEADER=x-forwarded-for:last`.
