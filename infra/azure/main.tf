# AgentRaaS on Azure: Container Apps, Azure Database for PostgreSQL Flexible
# Server, Azure Cache for Redis, and a Key Vault that holds the database URL,
# the JWT secret and the credentials master key (wrapped by a Key Vault key,
# CREDENTIALS_KEY_PROVIDER=azure, once you pass credentials_key_ciphertext).
# A user-assigned managed identity reads them, so nothing secret sits in the
# app definition. See README.md.

terraform {
  required_version = ">= 1.5"
  required_providers {
    azurerm = { source = "hashicorp/azurerm", version = "~> 4.10" }
    random  = { source = "hashicorp/random", version = "~> 3.6" }
  }
}

# Subscription: ARM_SUBSCRIPTION_ID in the environment (or `az account set`).
provider "azurerm" {
  features {
    key_vault {
      purge_soft_delete_on_destroy = false
    }
  }
}

data "azurerm_client_config" "current" {}

# Key Vault, Postgres and Redis names are global; the suffix keeps them free.
resource "random_string" "suffix" {
  length  = 6
  special = false
  upper   = false
}

locals {
  unique     = "${var.name}-${random_string.suffix.result}"
  tags       = { app = var.name }
  public_url = var.public_url != "" ? var.public_url : "https://${var.name}.${azurerm_container_app_environment.this.default_domain}"
}

resource "azurerm_resource_group" "this" {
  name     = var.name
  location = var.location
  tags     = local.tags
}

# --- Network: one subnet for the Container Apps environment, one delegated
# to Postgres, one for Redis's private endpoint. The apps reach Stripe, Slack
# and the rest through the environment's own outbound address.

resource "azurerm_virtual_network" "this" {
  name                = var.name
  location            = azurerm_resource_group.this.location
  resource_group_name = azurerm_resource_group.this.name
  address_space       = ["10.40.0.0/16"]
  tags                = local.tags
}

resource "azurerm_subnet" "apps" {
  name                 = "apps"
  resource_group_name  = azurerm_resource_group.this.name
  virtual_network_name = azurerm_virtual_network.this.name
  address_prefixes     = ["10.40.0.0/23"]
}

resource "azurerm_subnet" "db" {
  name                 = "db"
  resource_group_name  = azurerm_resource_group.this.name
  virtual_network_name = azurerm_virtual_network.this.name
  address_prefixes     = ["10.40.10.0/24"]
  service_endpoints    = ["Microsoft.Storage"]
  delegation {
    name = "postgres"
    service_delegation {
      name    = "Microsoft.DBforPostgreSQL/flexibleServers"
      actions = ["Microsoft.Network/virtualNetworks/subnets/join/action"]
    }
  }
}

resource "azurerm_subnet" "redis" {
  name                 = "redis"
  resource_group_name  = azurerm_resource_group.this.name
  virtual_network_name = azurerm_virtual_network.this.name
  address_prefixes     = ["10.40.11.0/24"]
}

resource "azurerm_private_dns_zone" "db" {
  name                = "${var.name}.postgres.database.azure.com"
  resource_group_name = azurerm_resource_group.this.name
  tags                = local.tags
}

resource "azurerm_private_dns_zone_virtual_network_link" "db" {
  name                  = "db"
  resource_group_name   = azurerm_resource_group.this.name
  private_dns_zone_name = azurerm_private_dns_zone.db.name
  virtual_network_id    = azurerm_virtual_network.this.id
}

resource "azurerm_private_dns_zone" "redis" {
  name                = "privatelink.redis.cache.windows.net"
  resource_group_name = azurerm_resource_group.this.name
  tags                = local.tags
}

resource "azurerm_private_dns_zone_virtual_network_link" "redis" {
  name                  = "redis"
  resource_group_name   = azurerm_resource_group.this.name
  private_dns_zone_name = azurerm_private_dns_zone.redis.name
  virtual_network_id    = azurerm_virtual_network.this.id
}

# --- Secrets.

resource "random_bytes" "credentials_key" { length = 32 }

resource "random_password" "db" {
  length  = 32
  special = false
}

resource "random_password" "jwt" {
  length  = 64
  special = false
}

# --- Postgres, reachable only inside the virtual network.

resource "azurerm_postgresql_flexible_server" "this" {
  name                          = local.unique
  location                      = azurerm_resource_group.this.location
  resource_group_name           = azurerm_resource_group.this.name
  version                       = "16"
  sku_name                      = var.db_sku_name
  storage_mb                    = var.db_storage_mb
  backup_retention_days         = 7
  delegated_subnet_id           = azurerm_subnet.db.id
  private_dns_zone_id           = azurerm_private_dns_zone.db.id
  public_network_access_enabled = false
  administrator_login           = "agentraas"
  administrator_password        = random_password.db.result
  dynamic "high_availability" {
    for_each = var.db_high_availability ? [1] : []
    content { mode = "ZoneRedundant" }
  }
  tags       = local.tags
  depends_on = [azurerm_private_dns_zone_virtual_network_link.db]
  lifecycle {
    ignore_changes = [zone, high_availability[0].standby_availability_zone]
  }
}

# The migrations create pgcrypto; Azure allows only listed extensions.
resource "azurerm_postgresql_flexible_server_configuration" "extensions" {
  name      = "azure.extensions"
  server_id = azurerm_postgresql_flexible_server.this.id
  value     = "PGCRYPTO"
}

resource "azurerm_postgresql_flexible_server_database" "this" {
  name      = "agentraas"
  server_id = azurerm_postgresql_flexible_server.this.id
  charset   = "UTF8"
  collation = "en_US.utf8"
}


# --- Redis, private endpoint only.
# ponytail: plain-text port with an access key, no TLS (the server's Redis
# client has none); it is reachable only from inside the virtual network.

resource "azurerm_redis_cache" "this" {
  name                          = local.unique
  location                      = azurerm_resource_group.this.location
  resource_group_name           = azurerm_resource_group.this.name
  sku_name                      = var.redis_sku_name
  family                        = var.redis_family
  capacity                      = var.redis_capacity
  non_ssl_port_enabled          = true
  minimum_tls_version           = "1.2"
  public_network_access_enabled = false
  redis_version                 = "6"
  tags                          = local.tags
}

resource "azurerm_private_endpoint" "redis" {
  name                = "${var.name}-redis"
  location            = azurerm_resource_group.this.location
  resource_group_name = azurerm_resource_group.this.name
  subnet_id           = azurerm_subnet.redis.id
  private_service_connection {
    name                           = "redis"
    private_connection_resource_id = azurerm_redis_cache.this.id
    subresource_names              = ["redisCache"]
    is_manual_connection           = false
  }
  private_dns_zone_group {
    name                 = "redis"
    private_dns_zone_ids = [azurerm_private_dns_zone.redis.id]
  }
  tags = local.tags
}

# --- Key Vault (RBAC). Terraform writes the secrets; the app's identity
# only reads them, and can decrypt with the credentials key.

resource "azurerm_key_vault" "this" {
  name                       = substr(replace(local.unique, "-", ""), 0, 24)
  location                   = azurerm_resource_group.this.location
  resource_group_name        = azurerm_resource_group.this.name
  tenant_id                  = data.azurerm_client_config.current.tenant_id
  sku_name                   = "standard"
  rbac_authorization_enabled = true
  purge_protection_enabled   = var.deletion_protection
  soft_delete_retention_days = 30
  tags                       = local.tags
}

resource "azurerm_role_assignment" "deployer" {
  for_each             = toset(["Key Vault Secrets Officer", "Key Vault Crypto Officer"])
  scope                = azurerm_key_vault.this.id
  role_definition_name = each.value
  principal_id         = data.azurerm_client_config.current.object_id
}

# Wraps the credentials master key once you pass credentials_key_ciphertext
# (README.md); the server then decrypts it at startup with the app's
# identity (CREDENTIALS_KEY_PROVIDER=azure). azurerm can't encrypt with it,
# so until then the generated key is stored as a plain Key Vault secret.
resource "azurerm_key_vault_key" "credentials" {
  name         = "credentials"
  key_vault_id = azurerm_key_vault.this.id
  key_type     = "RSA"
  key_size     = 3072
  key_opts     = ["encrypt", "decrypt"]
  depends_on   = [azurerm_role_assignment.deployer]
}

locals {
  secret_values = merge(
    {
      "database-url"    = "postgres://agentraas:${random_password.db.result}@${azurerm_postgresql_flexible_server.this.fqdn}:5432/agentraas?sslmode=require"
      "jwt-secret"      = random_password.jwt.result
      "credentials-key" = local.kms ? var.credentials_key_ciphertext : random_bytes.credentials_key.base64
      "redis-url"       = "redis://:${azurerm_redis_cache.this.primary_access_key}@${azurerm_redis_cache.this.hostname}:6379"
    },
    var.license_token != "" ? { "license-token" = var.license_token } : {}
  )
  kms = var.credentials_key_ciphertext != ""
  # Secret name in Key Vault -> environment variable.
  secret_env = {
    "database-url"    = "DATABASE_URL"
    "jwt-secret"      = "JWT_SECRET"
    "credentials-key" = "CREDENTIALS_ENCRYPTION_KEY"
    "redis-url"       = "REDIS_URL"
    "license-token"   = "LICENSE_TOKEN"
  }
}

resource "azurerm_key_vault_secret" "app" {
  for_each     = toset(concat(["database-url", "jwt-secret", "credentials-key", "redis-url"], nonsensitive(var.license_token != "") ? ["license-token"] : []))
  name         = each.key
  value        = local.secret_values[each.key]
  key_vault_id = azurerm_key_vault.this.id
  depends_on   = [azurerm_role_assignment.deployer]
}

# Delete locks on what can't be rebuilt. Remove them (deletion_protection =
# false, apply) before `terraform destroy`.
resource "azurerm_management_lock" "data" {
  for_each = var.deletion_protection ? {
    db        = azurerm_postgresql_flexible_server.this.id
    key-vault = azurerm_key_vault.this.id
  } : {}
  name       = "${var.name}-${each.key}"
  scope      = each.value
  lock_level = "CanNotDelete"
  notes      = "Holds the audit trail, stored credentials or their master key."
}

# --- Identity the app runs as: reads the Key Vault secrets and, for
# Enterprise secret references, only what you grant it.

resource "azurerm_user_assigned_identity" "app" {
  name                = "${var.name}-app"
  location            = azurerm_resource_group.this.location
  resource_group_name = azurerm_resource_group.this.name
  tags                = local.tags
}

resource "azurerm_role_assignment" "app" {
  for_each             = toset(["Key Vault Secrets User", "Key Vault Crypto User"])
  scope                = azurerm_key_vault.this.id
  role_definition_name = each.value
  principal_id         = azurerm_user_assigned_identity.app.principal_id
}

# --- Container Apps: an init container runs agentraas-migrate (under an
# advisory lock) on every replica start, then the API.

resource "azurerm_log_analytics_workspace" "this" {
  name                = var.name
  location            = azurerm_resource_group.this.location
  resource_group_name = azurerm_resource_group.this.name
  sku                 = "PerGB2018"
  retention_in_days   = 30
  tags                = local.tags
}

resource "azurerm_container_app_environment" "this" {
  name                       = var.name
  location                   = azurerm_resource_group.this.location
  resource_group_name        = azurerm_resource_group.this.name
  log_analytics_workspace_id = azurerm_log_analytics_workspace.this.id
  infrastructure_subnet_id   = azurerm_subnet.apps.id
  tags                       = local.tags
}

locals {
  environment = merge({
    PORT            = "3000"
    RUST_LOG        = "info"
    DEPLOYMENT_MODE = "self-hosted"
    PUBLIC_URL      = local.public_url
    # Container Apps' ingress appends the caller's address.
    CLIENT_IP_HEADER = "x-forwarded-for:last"
    AZURE_CLIENT_ID  = azurerm_user_assigned_identity.app.client_id
    }, local.kms ? {
    CREDENTIALS_KEY_PROVIDER = "azure"
    CREDENTIALS_KMS_KEY      = azurerm_key_vault_key.credentials.id
  } : {}, var.extra_env)
}

resource "azurerm_container_app" "api" {
  name                         = var.name
  container_app_environment_id = azurerm_container_app_environment.this.id
  resource_group_name          = azurerm_resource_group.this.name
  revision_mode                = "Single"
  tags                         = local.tags

  identity {
    type         = "UserAssigned"
    identity_ids = [azurerm_user_assigned_identity.app.id]
  }

  dynamic "secret" {
    for_each = azurerm_key_vault_secret.app
    content {
      name                = secret.key
      key_vault_secret_id = secret.value.versionless_id
      identity            = azurerm_user_assigned_identity.app.id
    }
  }

  ingress {
    external_enabled = true
    target_port      = 3000
    transport        = "auto"
    traffic_weight {
      latest_revision = true
      percentage      = 100
    }
  }

  template {
    min_replicas = var.min_replicas
    max_replicas = var.max_replicas

    init_container {
      name    = "migrate"
      image   = var.image
      cpu     = var.cpu
      memory  = var.memory
      command = ["agentraas-migrate"]
      env {
        name        = "DATABASE_URL"
        secret_name = "database-url"
      }
    }

    container {
      name   = "api"
      image  = var.image
      cpu    = var.cpu
      memory = var.memory

      dynamic "env" {
        for_each = local.environment
        content {
          name  = env.key
          value = env.value
        }
      }
      dynamic "env" {
        for_each = azurerm_key_vault_secret.app
        content {
          name        = local.secret_env[env.key]
          secret_name = env.key
        }
      }

      liveness_probe {
        transport = "HTTP"
        port      = 3000
        path      = "/health"
      }
      readiness_probe {
        transport = "HTTP"
        port      = 3000
        path      = "/health"
      }
    }
  }

  depends_on = [
    azurerm_role_assignment.app,
    azurerm_postgresql_flexible_server_configuration.extensions,
    azurerm_postgresql_flexible_server_database.this,
    azurerm_private_endpoint.redis,
  ]
}
