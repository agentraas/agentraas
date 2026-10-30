variable "project_id" {
  description = "Google Cloud project to run in."
  type        = string
}

variable "region" {
  description = "Google Cloud region to run in."
  type        = string
  default     = "us-central1"
}

variable "name" {
  description = "Prefix for every resource name (lowercase letters, digits, dashes)."
  type        = string
  default     = "agentraas"
}

variable "image" {
  description = "API image. Needs 0.23.0 or later (it carries the service catalog, the migrations and agentraas-migrate). A ghcr.io image is pulled through an Artifact Registry remote repository."
  type        = string
  default     = "ghcr.io/agentraas/agentraas:0.30.2"
}

variable "public_url" {
  description = "The address people use, e.g. https://agentraas.example.com. Empty = the service's own https://...run.app address."
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
  description = "Your CREDENTIALS_ENCRYPTION_KEY, already encrypted with this module's KMS key (base64 ciphertext). Empty = one is generated; its plain value then sits in the Terraform state, so keep the state encrypted."
  type        = string
  default     = ""
}

variable "min_instances" {
  description = "API instances always running. More than one is safe: dedup, locks and limits live in Redis. Keep at least 1."
  type        = number
  default     = 1
}

variable "max_instances" {
  type    = number
  default = 3
}

variable "cpu" {
  type    = string
  default = "1"
}

variable "memory" {
  type    = string
  default = "1Gi"
}

variable "db_tier" {
  type    = string
  default = "db-g1-small"
}

variable "db_disk_size" {
  description = "Postgres storage in GB."
  type        = number
  default     = 20
}

variable "db_high_availability" {
  description = "A standby database in a second zone."
  type        = bool
  default     = false
}

variable "redis_tier" {
  description = "BASIC (one node) or STANDARD_HA (with a replica)."
  type        = string
  default     = "BASIC"
}

variable "redis_memory_size_gb" {
  type    = number
  default = 1
}

variable "deletion_protection" {
  description = "Protects the database from `terraform destroy` and from deletion in the console. Turn off only for a throwaway test."
  type        = bool
  default     = true
}

variable "extra_env" {
  description = "More environment variables for the API (SMTP_HOST, PADDLE_*, OTEL_EXPORTER_OTLP_ENDPOINT, ...). Not for secrets, and not PORT (Cloud Run sets it)."
  type        = map(string)
  default     = {}
}
