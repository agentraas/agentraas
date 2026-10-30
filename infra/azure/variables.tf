variable "location" {
  description = "Azure region to run in."
  type        = string
  default     = "eastus"
}

variable "name" {
  description = "Prefix for every resource name (lowercase letters, digits, dashes)."
  type        = string
  default     = "agentraas"
}

variable "image" {
  description = "API image. Needs 0.23.0 or later (it carries the service catalog, the migrations and agentraas-migrate)."
  type        = string
  default     = "ghcr.io/agentraas/agentraas:0.30.0"
}

variable "public_url" {
  description = "The address people use, e.g. https://agentraas.example.com. Empty = the app's own https://<name>.<environment domain> address."
  type        = string
  default     = ""
}

variable "license_token" {
  description = "Enterprise license key (LICENSE_TOKEN). Empty = the free edition, Team features included."
  type        = string
  default     = ""
  sensitive   = true
}

variable "credentials_key_ciphertext" {
  description = "Your CREDENTIALS_ENCRYPTION_KEY, encrypted with this module's Key Vault key (RSA-OAEP-256, base64, see README.md). Set = the server unwraps it at startup (CREDENTIALS_KEY_PROVIDER=azure). Empty = one is generated and stored as a plain Key Vault secret; its value then also sits in the Terraform state, so keep the state encrypted."
  type        = string
  default     = ""
}

variable "min_replicas" {
  description = "API replicas always running. More than one is safe: dedup, locks and limits live in Redis. Keep at least 1 (0 = cold starts and no background jobs)."
  type        = number
  default     = 1
}

variable "max_replicas" {
  type    = number
  default = 3
}

variable "cpu" {
  description = "vCPU per replica (Container Apps pairs it with memory: 0.5 with 1Gi, 1 with 2Gi, ...)."
  type        = number
  default     = 0.5
}

variable "memory" {
  type    = string
  default = "1Gi"
}

variable "db_sku_name" {
  type    = string
  default = "B_Standard_B1ms"
}

variable "db_storage_mb" {
  description = "Postgres storage in MB."
  type        = number
  default     = 32768
}

variable "db_high_availability" {
  description = "A standby database in a second availability zone (needs a General Purpose or Memory Optimized db_sku_name)."
  type        = bool
  default     = false
}

variable "redis_sku_name" {
  type    = string
  default = "Basic"
}

variable "redis_family" {
  type    = string
  default = "C"
}

variable "redis_capacity" {
  type    = number
  default = 0
}

variable "deletion_protection" {
  description = "Locks the database against deletion and turns on Key Vault purge protection. Turn off only for a throwaway test."
  type        = bool
  default     = true
}

variable "extra_env" {
  description = "More environment variables for the API (SMTP_HOST, PADDLE_*, OTEL_EXPORTER_OTLP_ENDPOINT, ...). Not for secrets."
  type        = map(string)
  default     = {}
}
