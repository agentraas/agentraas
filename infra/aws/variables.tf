variable "region" {
  description = "AWS region to run in."
  type        = string
  default     = "us-east-1"
}

variable "name" {
  description = "Prefix for every resource name."
  type        = string
  default     = "agentraas"
}

variable "image" {
  description = "API image. Needs 0.23.0 or later (it carries the service catalog, the migrations and agentraas-migrate)."
  type        = string
  default     = "ghcr.io/agentraas/agentraas:0.30.3"
}

variable "public_url" {
  description = "The address people use, e.g. https://agentraas.example.com. Empty = the load balancer's http:// address."
  type        = string
  default     = ""
}

variable "certificate_arn" {
  description = "ACM certificate for HTTPS on the load balancer. Empty = HTTP only (fine to try it, not for production)."
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
  description = "Your CREDENTIALS_ENCRYPTION_KEY, already encrypted with this module's KMS key (base64 CiphertextBlob). Empty = one is generated; its plain value then sits in the Terraform state, so keep the state encrypted."
  type        = string
  default     = ""
}

variable "desired_count" {
  description = "API tasks. More than one is safe: dedup, locks and limits live in Redis."
  type        = number
  default     = 1
}

variable "task_cpu" {
  type    = number
  default = 512
}

variable "task_memory" {
  type    = number
  default = 1024
}

variable "db_instance_class" {
  type    = string
  default = "db.t4g.micro"
}

variable "db_allocated_storage" {
  description = "Postgres storage in GB."
  type        = number
  default     = 20
}

variable "db_multi_az" {
  description = "A standby database in a second availability zone."
  type        = bool
  default     = false
}

variable "redis_node_type" {
  type    = string
  default = "cache.t4g.micro"
}

variable "deletion_protection" {
  description = "Protects the database from `terraform destroy` and keeps a final snapshot. Turn off only for a throwaway test."
  type        = bool
  default     = true
}

variable "extra_env" {
  description = "More environment variables for the API (SMTP_HOST, PADDLE_*, OTEL_EXPORTER_OTLP_ENDPOINT, ...). Not for secrets."
  type        = map(string)
  default     = {}
}
