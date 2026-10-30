# AgentRaaS on Google Cloud: Cloud Run, Cloud SQL for PostgreSQL, Memorystore
# for Redis (reached over Direct VPC egress), and a Cloud KMS key that holds
# the master key for stored service credentials (CREDENTIALS_KEY_PROVIDER=gcp,
# read with the service's own service account). Secrets come from Secret
# Manager, so nothing secret sits in the service definition. See README.md.

terraform {
  required_version = ">= 1.5"
  required_providers {
    google = { source = "hashicorp/google", version = "~> 6.20" }
    random = { source = "hashicorp/random", version = "~> 3.6" }
  }
}

provider "google" {
  project        = var.project_id
  region         = var.region
  default_labels = { app = var.name }
}

data "google_project" "this" {}

locals {
  # Cloud Run's address is fixed by name, project number and region, so the
  # service can be told its own URL.
  run_url    = "https://${var.name}-${data.google_project.this.number}.${var.region}.run.app"
  public_url = var.public_url != "" ? var.public_url : local.run_url
  # Cloud Run can't pull from ghcr.io directly; the remote repository below
  # proxies it.
  image = startswith(var.image, "ghcr.io/") ? "${var.region}-docker.pkg.dev/${var.project_id}/${google_artifact_registry_repository.ghcr.repository_id}/${trimprefix(var.image, "ghcr.io/")}" : var.image
}

resource "google_project_service" "this" {
  for_each = toset([
    "artifactregistry.googleapis.com",
    "cloudkms.googleapis.com",
    "compute.googleapis.com",
    "redis.googleapis.com",
    "run.googleapis.com",
    "secretmanager.googleapis.com",
    "servicenetworking.googleapis.com",
    "sqladmin.googleapis.com",
  ])
  service            = each.value
  disable_on_destroy = false
}

resource "google_artifact_registry_repository" "ghcr" {
  location      = var.region
  repository_id = "${var.name}-ghcr"
  description   = "Pull-through cache of ghcr.io for the AgentRaaS image"
  format        = "DOCKER"
  mode          = "REMOTE_REPOSITORY"
  remote_repository_config {
    docker_repository {
      custom_repository { uri = "https://ghcr.io" }
    }
  }
  depends_on = [google_project_service.this]
}

# --- Network: a subnet Cloud Run sends private traffic through (Direct VPC
# egress), and a private services range for Cloud SQL and Memorystore.
# Traffic to Stripe, Slack and the rest leaves Cloud Run directly, so no
# Cloud NAT is needed.

resource "google_compute_network" "this" {
  name                    = var.name
  auto_create_subnetworks = false
  depends_on              = [google_project_service.this]
}

resource "google_compute_subnetwork" "run" {
  name                     = "${var.name}-run"
  network                  = google_compute_network.this.id
  region                   = var.region
  ip_cidr_range            = "10.40.0.0/24"
  private_ip_google_access = true
}

resource "google_compute_global_address" "services" {
  name          = "${var.name}-services"
  purpose       = "VPC_PEERING"
  address_type  = "INTERNAL"
  prefix_length = 16
  network       = google_compute_network.this.id
}

resource "google_service_networking_connection" "services" {
  network                 = google_compute_network.this.id
  service                 = "servicenetworking.googleapis.com"
  reserved_peering_ranges = [google_compute_global_address.services.name]
}

# --- KMS: the credentials master key. Google encrypts the database, Redis
# and the secrets at rest with its own keys.
# ponytail: no customer-managed keys on Cloud SQL, Memorystore and Secret
# Manager (each needs its service agent granted on the key first); add if
# policy requires it.

resource "google_kms_key_ring" "this" {
  name       = var.name
  location   = var.region
  depends_on = [google_project_service.this]
}

resource "google_kms_crypto_key" "credentials" {
  name            = "credentials"
  key_ring        = google_kms_key_ring.this.id
  rotation_period = "7776000s"
  lifecycle {
    prevent_destroy = true
  }
}

resource "random_bytes" "credentials_key" { length = 32 }

# KMS stores the key's base64 text; the server accepts that (kms.rs).
resource "google_kms_secret_ciphertext" "credentials_key" {
  count      = var.credentials_key_ciphertext == "" ? 1 : 0
  crypto_key = google_kms_crypto_key.credentials.id
  plaintext  = random_bytes.credentials_key.base64
}

resource "random_password" "db" {
  length  = 32
  special = false
}

resource "random_password" "jwt" {
  length  = 64
  special = false
}

# --- Postgres and Redis, private addresses only.

resource "google_sql_database_instance" "this" {
  name                = var.name
  database_version    = "POSTGRES_16"
  region              = var.region
  deletion_protection = var.deletion_protection
  settings {
    edition                     = "ENTERPRISE"
    tier                        = var.db_tier
    disk_size                   = var.db_disk_size
    disk_type                   = "PD_SSD"
    availability_type           = var.db_high_availability ? "REGIONAL" : "ZONAL"
    deletion_protection_enabled = var.deletion_protection
    ip_configuration {
      ipv4_enabled    = false
      private_network = google_compute_network.this.id
      ssl_mode        = "ENCRYPTED_ONLY"
    }
    backup_configuration {
      enabled                        = true
      point_in_time_recovery_enabled = true
      backup_retention_settings { retained_backups = 7 }
    }
  }
  depends_on = [google_service_networking_connection.services]
}

resource "google_sql_database" "this" {
  name     = "agentraas"
  instance = google_sql_database_instance.this.name
}

resource "google_sql_user" "this" {
  name     = "agentraas"
  instance = google_sql_database_instance.this.name
  password = random_password.db.result
}

# ponytail: no TLS or AUTH on Redis (the server's Redis client has no TLS);
# it has only a private address inside the network.
resource "google_redis_instance" "this" {
  name                    = var.name
  region                  = var.region
  tier                    = var.redis_tier
  memory_size_gb          = var.redis_memory_size_gb
  redis_version           = "REDIS_7_2"
  authorized_network      = google_compute_network.this.id
  connect_mode            = "PRIVATE_SERVICE_ACCESS"
  transit_encryption_mode = "DISABLED"
  depends_on              = [google_service_networking_connection.services]
}

# --- Secrets, read by Cloud Run when it starts the containers.

locals {
  secret_values = merge(
    {
      DATABASE_URL               = "postgres://agentraas:${random_password.db.result}@${google_sql_database_instance.this.private_ip_address}:5432/agentraas?sslmode=require"
      JWT_SECRET                 = random_password.jwt.result
      CREDENTIALS_ENCRYPTION_KEY = var.credentials_key_ciphertext != "" ? var.credentials_key_ciphertext : google_kms_secret_ciphertext.credentials_key[0].ciphertext
    },
    var.license_token != "" ? { LICENSE_TOKEN = var.license_token } : {}
  )
  secret_keys = concat(["DATABASE_URL", "JWT_SECRET", "CREDENTIALS_ENCRYPTION_KEY"], nonsensitive(var.license_token != "") ? ["LICENSE_TOKEN"] : [])
}

resource "google_secret_manager_secret" "app" {
  for_each  = toset(local.secret_keys)
  secret_id = "${var.name}-${lower(replace(each.key, "_", "-"))}"
  replication {
    auto {}
  }
  depends_on = [google_project_service.this]
}

resource "google_secret_manager_secret_version" "app" {
  for_each    = google_secret_manager_secret.app
  secret      = each.value.id
  secret_data = local.secret_values[each.key]
}

# --- The service account the API runs as: reads its secrets, unwraps
# CREDENTIALS_ENCRYPTION_KEY at startup and, for Enterprise secret references
# (gcp-sm://), reads only what you grant it.

resource "google_service_account" "api" {
  account_id   = "${var.name}-api"
  display_name = "AgentRaaS API"
}

resource "google_secret_manager_secret_iam_member" "api" {
  for_each  = google_secret_manager_secret.app
  secret_id = each.value.id
  role      = "roles/secretmanager.secretAccessor"
  member    = google_service_account.api.member
}

resource "google_kms_crypto_key_iam_member" "api" {
  crypto_key_id = google_kms_crypto_key.credentials.id
  role          = "roles/cloudkms.cryptoKeyDecrypter"
  member        = google_service_account.api.member
}

# --- Cloud Run. Each instance runs agentraas-migrate first (under an
# advisory lock), then the API.

locals {
  environment = merge({
    RUST_LOG        = "info"
    DEPLOYMENT_MODE = "self-hosted"
    PUBLIC_URL      = local.public_url
    REDIS_URL       = "redis://${google_redis_instance.this.host}:${google_redis_instance.this.port}"
    # Google's front end appends the caller's address.
    CLIENT_IP_HEADER         = "x-forwarded-for:last"
    CREDENTIALS_KEY_PROVIDER = "gcp"
    CREDENTIALS_KMS_KEY      = google_kms_crypto_key.credentials.id
  }, var.extra_env)
}

resource "google_cloud_run_v2_service" "api" {
  name                = var.name
  location            = var.region
  ingress             = "INGRESS_TRAFFIC_ALL"
  deletion_protection = false

  template {
    service_account = google_service_account.api.email
    scaling {
      min_instance_count = var.min_instances
      max_instance_count = var.max_instances
    }
    vpc_access {
      egress = "PRIVATE_RANGES_ONLY"
      network_interfaces {
        network    = google_compute_network.this.id
        subnetwork = google_compute_subnetwork.run.id
      }
    }
    containers {
      image   = local.image
      command = ["sh"]
      args    = ["-c", "agentraas-migrate && exec ar-api-rs"]
      ports { container_port = 3000 }
      resources {
        limits   = { cpu = var.cpu, memory = var.memory }
        cpu_idle = false # background jobs keep running between requests
      }
      dynamic "env" {
        for_each = local.environment
        content {
          name  = env.key
          value = env.value
        }
      }
      dynamic "env" {
        for_each = google_secret_manager_secret.app
        content {
          name = env.key
          value_source {
            secret_key_ref {
              secret  = env.value.secret_id
              version = "latest"
            }
          }
        }
      }
      startup_probe {
        http_get { path = "/health" }
        period_seconds    = 10
        failure_threshold = 24
      }
      liveness_probe {
        http_get { path = "/health" }
      }
    }
  }

  depends_on = [
    google_secret_manager_secret_version.app,
    google_secret_manager_secret_iam_member.api,
    google_kms_crypto_key_iam_member.api,
    google_sql_database.this,
    google_sql_user.this,
  ]
}

# Public: AgentRaaS does its own login and API keys.
resource "google_cloud_run_v2_service_iam_member" "public" {
  name     = google_cloud_run_v2_service.api.name
  location = var.region
  role     = "roles/run.invoker"
  member   = "allUsers"
}
