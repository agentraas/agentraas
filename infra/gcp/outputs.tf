output "url" {
  description = "Where AgentRaaS answers: the dashboard is at /dashboard."
  value       = local.public_url
}

output "service_url" {
  description = "The service's own run.app address. Map your domain to the service when you use public_url (see README.md)."
  value       = google_cloud_run_v2_service.api.uri
}

output "kms_key_id" {
  description = "Holds the credentials master key. Never destroy its key versions: without them every stored service key is lost."
  value       = google_kms_crypto_key.credentials.id
}

output "service_account_email" {
  description = "Grant this account roles/secretmanager.secretAccessor on the secrets your Enterprise secret references (gcp-sm://) name."
  value       = google_service_account.api.email
}

output "logs" {
  value = "gcloud beta run services logs tail ${google_cloud_run_v2_service.api.name} --region ${var.region} --project ${var.project_id}"
}
