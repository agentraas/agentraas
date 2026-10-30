plugin "azurerm" {
  enabled = true
  version = "0.33.0"
  source  = "github.com/terraform-linters/tflint-ruleset-azurerm"
}

# prevent_destroy can't follow a variable; deletion_protection (on by
# default) puts delete locks on the database and Key Vault instead, and
# Key Vault soft delete keeps a deleted secret for 30 days.
rule "azurerm_resources_missing_prevent_destroy" {
  enabled = false
}
