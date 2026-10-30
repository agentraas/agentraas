# AgentRaaS on Azure (Terraform)

Runs AgentRaaS in your own Azure subscription with managed services:

| Part | Azure service |
|---|---|
| API (the `ghcr.io/agentraas/agentraas` image) | Container Apps, HTTPS ingress, in your own virtual network |
| Postgres | Azure Database for PostgreSQL Flexible Server 16, delegated subnet, no public access, 7 days of backups |
| Redis | Azure Cache for Redis, private endpoint only |
| Master key for stored service keys | Key Vault key (`CREDENTIALS_KEY_PROVIDER=azure`, read with the app's managed identity), see below |
| Database URL, Redis URL, JWT secret, license key | Key Vault secrets, read by the app's user-assigned managed identity |
| Logs | Log Analytics, 30 days |

Every replica start runs `agentraas-migrate` first (an init container,
under a Postgres advisory lock), then the API.

## Use it

Needs Terraform 1.5+ and the Azure CLI logged in (`az login`) to a
subscription you own, with rights to create role assignments (Owner, or
Contributor plus User Access Administrator).

```bash
cd infra/azure
export ARM_SUBSCRIPTION_ID=$(az account show --query id -o tsv)
terraform init
terraform apply -var location=westeurope
```

Then open the `url` output. It is the app's own
`https://agentraas.<environment domain>` address, with a certificate from
Azure. For your own domain: CNAME it to the `app_fqdn` output, add it to the
app with a managed certificate
(`az containerapp hostname add` then `az containerapp hostname bind
--validation-method CNAME`), and apply again with
`-var public_url=https://agentraas.example.com`.

Useful variables (all in `variables.tf`): `license_token` for Enterprise,
`min_replicas`, `max_replicas`, `db_sku_name`, `db_high_availability`,
`extra_env` for SMTP, OpenTelemetry and the rest, `deletion_protection` (on
by default).

If the first apply fails with a Key Vault "Forbidden": Azure role
assignments can take a few minutes to apply. Run `terraform apply` again.

## Keys and state

- **Master key.** Without `credentials_key_ciphertext`, Terraform generates
  the 32-byte key and stores it as a plain Key Vault secret, read by the
  app's identity. Its value is also in the Terraform state, as are the
  database password, the Redis access key and the JWT secret, so keep the
  state in an encrypted backend (an Azure Storage container) with limited
  access. To keep the key out of the state and have only Key Vault able to
  unwrap it, encrypt it with the module's Key Vault key (the provider can't
  do this step, unlike on AWS and Google Cloud) and pass the ciphertext:

  ```bash
  terraform apply                              # the vault and its key first
  az keyvault key encrypt --vault-name "$(terraform output -raw key_vault_name)" \
    --name credentials --algorithm RSA-OAEP-256 --data-type plaintext \
    --value "$(openssl rand -base64 32)" --query result -o tsv
  terraform apply -var credentials_key_ciphertext=<that output>
  ```

  Do this before you store any service key: switching keys later makes the
  ones already stored unreadable. To move an existing install, encrypt its
  current `CREDENTIALS_ENCRYPTION_KEY` instead of a new one.
- **Image 0.30.2 or later**: the template connects to Redis over TLS
  (`rediss://`, port 6380), which earlier servers can't speak.
- **Image 0.30.0 or later** for the Key Vault mode: earlier servers only
  asked IMDS for the identity token, not Container Apps' own endpoint.
- **Never purge the Key Vault** (`key_vault_name` output) or its
  `credentials` key: every stored service key depends on it. With
  `deletion_protection` on, the vault has purge protection and a delete lock.
- **Secret references** (Enterprise, `azure-kv://`): give the app's identity
  (`app_identity_principal_id` output) "Key Vault Secrets User" on the vaults
  you name.

## What it leaves out on purpose

- One Redis node (Basic) and, by default, one database server
  (`db_high_availability` adds a zone-redundant standby).
- Azure has announced the retirement of Azure Cache for Redis in favor of
  Azure Managed Redis; this template still uses the former.

Client addresses: Container Apps' ingress appends the caller's address to
`X-Forwarded-For`, so the app sets `CLIENT_IP_HEADER=x-forwarded-for:last`.
