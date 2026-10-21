//! Secret references: a stored credential value can name a secret in the
//! customer's own secrets manager instead of holding the secret, and it is
//! fetched when an agent call needs it. The real key never reaches our
//! database, and rotating it there takes effect here without re-saving.
//!
//!   vault://secret/data/stripe#api_key              Vault / OpenBao KV (v1 or v2)
//!   aws-sm://arn:aws:secretsmanager:...:secret:x[#field]   AWS Secrets Manager
//!   gcp-sm://projects/p/secrets/s[/versions/3][#field]     Google Secret Manager
//!   azure-kv://my-vault.vault.azure.net/secrets/x[/version][#field]  Azure Key Vault
//!
//! Self-hosted servers only: the secret is read with the server's own
//! identity (VAULT_TOKEN, AWS role, ...), which on a shared Cloud server
//! would let one customer name another's secrets. Saving one on Cloud is
//! refused (`credentials.rs`) and resolving one there fails (`agent/db.rs`).
//!
//! `#field` reads one field of a JSON secret. Login is the same as for the
//! KMS (`kms.rs`): VAULT_ADDR/VAULT_TOKEN, the AWS role, the GCP service
//! account, the Azure managed identity. Values are cached in memory for
//! SECRET_REF_CACHE_SECONDS (default 300); if a refresh fails, the last good
//! value is used and the failure logged, so a secrets-manager blip doesn't
//! stop agents.

use crate::kms::{aws_call, aws_region, azure_token, field, gcp_token, json_ok, vault_request};
use crate::util::configured_env;
use anyhow::{anyhow, bail};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const SCHEMES: [&str; 4] = ["vault://", "aws-sm://", "gcp-sm://", "azure-kv://"];

// Azure is the one scheme where the reference picks the host, and the
// managed-identity token goes with the request: only real Key Vault hosts.
const AZURE_VAULT_SUFFIXES: [&str; 3] = [".vault.azure.net", ".vault.azure.cn", ".vault.usgovcloudapi.net"];

/// Rejects a malformed reference before it is saved or fetched.
pub fn check(reference: &str) -> anyhow::Result<()> {
    let (scheme, rest) = reference.split_once("://").ok_or_else(|| anyhow!("not a secret reference"))?;
    match scheme {
        "vault" if !rest.contains('#') => bail!("a Vault reference needs #field, e.g. vault://secret/data/stripe#api_key"),
        "azure-kv" => {
            let host = rest.split('/').next().unwrap_or_default();
            let plain_host = host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
            if !plain_host || !AZURE_VAULT_SUFFIXES.iter().any(|s| host.ends_with(s)) || !rest.contains("/secrets/") || rest.contains('?') {
                bail!("an Azure reference looks like azure-kv://<vault>.vault.azure.net/secrets/<name>");
            }
        }
        _ => {}
    }
    Ok(())
}

pub fn is_ref(value: &str) -> bool {
    SCHEMES.iter().any(|s| value.starts_with(s))
}

/// Any string value of a credentials object is a reference.
pub fn has_ref(credentials: &Value) -> bool {
    credentials.as_object().is_some_and(|o| o.values().filter_map(Value::as_str).any(is_ref))
}

// ponytail: one global map, never pruned; fine for the handful of
// references an install has.
fn cache() -> &'static Mutex<HashMap<String, (String, Instant)>> {
    static CACHE: OnceLock<Mutex<HashMap<String, (String, Instant)>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

// Not the forwarding client: that one refuses private addresses, and a
// company's Vault usually is one. Safe here because no reference picks its
// host except Azure's, which `check` limits to Key Vault.
fn http() -> &'static reqwest::Client {
    static HTTP: OnceLock<reqwest::Client> = OnceLock::new();
    HTTP.get_or_init(|| reqwest::Client::builder().timeout(Duration::from_secs(10)).build().expect("TLS backend available"))
}

/// Returns `value` unchanged unless it is a reference.
pub async fn resolve(value: &str) -> anyhow::Result<String> {
    if !is_ref(value) {
        return Ok(value.to_string());
    }
    check(value)?;
    let ttl = Duration::from_secs(
        configured_env("SECRET_REF_CACHE_SECONDS").and_then(|s| s.parse().ok()).unwrap_or(300),
    );
    let cached = cache().lock().unwrap().get(value).cloned();
    if let Some((v, at)) = &cached {
        if at.elapsed() < ttl {
            return Ok(v.clone());
        }
    }
    let fetched = tokio::time::timeout(Duration::from_secs(10), fetch(http(), value))
        .await
        .unwrap_or_else(|_| Err(anyhow!("secrets manager did not answer within 10s")));
    match fetched {
        Ok(v) => {
            cache().lock().unwrap().insert(value.to_string(), (v.clone(), Instant::now()));
            Ok(v)
        }
        Err(err) => match cached {
            Some((v, _)) => {
                tracing::warn!(reference = value, error = %err, "secret refresh failed, using the last fetched value");
                Ok(v)
            }
            None => Err(err),
        },
    }
}

async fn fetch(http: &reqwest::Client, reference: &str) -> anyhow::Result<String> {
    let (scheme, rest) = reference.split_once("://").expect("is_ref checked the scheme");
    let (path, field_name) = match rest.split_once('#') {
        Some((p, f)) => (p, Some(f)),
        None => (rest, None),
    };
    let secret = match scheme {
        "vault" => {
            let v = json_ok(vault_request(http, reqwest::Method::GET, path)?.send().await?).await?;
            // KV v2 nests the secret under data.data, v1 under data.
            let data = v["data"].get("data").filter(|d| d.is_object()).unwrap_or(&v["data"]);
            let name = field_name.expect("check() requires #field for Vault");
            return data
                .get(name)
                .and_then(Value::as_str)
                .map(String::from)
                .ok_or_else(|| anyhow!("Vault secret {path} has no field {name}"));
        }
        "aws-sm" => {
            let v = aws_call(http, &aws_region(path)?, "secretsmanager", "secretsmanager.GetSecretValue", &json!({ "SecretId": path })).await?;
            field(&v, &["SecretString"])?.to_string()
        }
        "gcp-sm" => {
            let name = if path.contains("/versions/") { path.to_string() } else { format!("{path}/versions/latest") };
            let v = json_ok(
                http.get(format!("https://secretmanager.googleapis.com/v1/{name}:access"))
                    .bearer_auth(gcp_token(http).await?)
                    .send()
                    .await?,
            )
            .await?;
            String::from_utf8(STANDARD.decode(field(&v, &["payload", "data"])?)?)?
        }
        "azure-kv" => {
            let v = json_ok(
                http.get(format!("https://{path}?api-version=7.4"))
                    .bearer_auth(azure_token(http).await?)
                    .send()
                    .await?,
            )
            .await?;
            field(&v, &["value"])?.to_string()
        }
        _ => unreachable!("is_ref only accepts the four schemes"),
    };
    match field_name {
        None => Ok(secret),
        Some(name) => {
            let json: Value = serde_json::from_str(&secret).map_err(|_| anyhow!("secret is not JSON, drop #{name}"))?;
            match json.get(name).and_then(Value::as_str) {
                Some(v) => Ok(v.to_string()),
                None => bail!("secret has no field {name}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_references_only() {
        assert!(is_ref("vault://secret/data/stripe#api_key"));
        assert!(is_ref("aws-sm://arn:aws:secretsmanager:us-east-1:1:secret:x"));
        assert!(!is_ref("sk_live_123"));
        assert!(!is_ref("https://example.com"));
        assert!(has_ref(&json!({ "api_key": "gcp-sm://projects/p/secrets/s" })));
        assert!(!has_ref(&json!({ "username": "u", "password": "p" })));
    }

    #[test]
    fn azure_host_must_be_key_vault() {
        assert!(check("azure-kv://my-vault.vault.azure.net/secrets/stripe").is_ok());
        assert!(check("azure-kv://169.254.169.254/secrets/x").is_err());
        assert!(check("azure-kv://evil.example/secrets/x?.vault.azure.net").is_err());
        assert!(check("azure-kv://evil.com?x=.vault.azure.net/secrets/y").is_err());
        assert!(check("azure-kv://a.vault.azure.net@evil.com/secrets/y").is_err());
        assert!(check("vault://secret/data/stripe").is_err());
    }

    #[tokio::test]
    async fn plain_values_pass_through() {
        assert_eq!(resolve("sk_test_1").await.unwrap(), "sk_test_1");
    }

    // Against a real Vault; skipped unless VAULT_TEST_SECRET_REF is set
    // (a KV path holding api_key = "sk_test_from_vault").
    #[tokio::test]
    async fn vault_kv_roundtrip() {
        let Some(r) = configured_env("VAULT_TEST_SECRET_REF") else { return };
        assert_eq!(resolve(&r).await.unwrap(), "sk_test_from_vault");
    }
}
