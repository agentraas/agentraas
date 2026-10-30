output "url" {
  description = "Where AgentRaaS answers: the dashboard is at /dashboard."
  value       = local.public_url
}

output "app_fqdn" {
  description = "Point your domain's CNAME here when you use public_url (then add the domain to the app, see README.md)."
  value       = azurerm_container_app.api.ingress[0].fqdn
}

output "key_vault_name" {
  description = "Holds the credentials master key. Never purge it: without that key every stored service key is lost."
  value       = azurerm_key_vault.this.name
}

output "app_identity_principal_id" {
  description = "The managed identity the API runs as."
  value       = azurerm_user_assigned_identity.app.principal_id
}

output "logs" {
  value = "az containerapp logs show --name ${azurerm_container_app.api.name} --resource-group ${azurerm_resource_group.this.name} --follow"
}
