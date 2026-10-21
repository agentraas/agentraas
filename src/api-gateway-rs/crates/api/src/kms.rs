//! Where the AES key behind `CREDENTIALS_ENCRYPTION_KEY` comes from.
//!
//! `CREDENTIALS_KEY_PROVIDER` unset or `env`: the variable is the key itself
//! (base64 of 32 bytes), as before. `vault`, `aws`, `gcp` or `azure`: the
//! variable holds those 32 bytes encrypted by a key in that KMS, and the
//! server asks the KMS to decrypt them once at startup. The master key never
//! leaves the KMS, so a copied `.env` is useless without KMS access. The
//! stored-row format in `crypto.rs` does not change either way.
//!
//! `CREDENTIALS_KMS_KEY` names the KMS key: Vault transit key name, AWS key
//! ARN, GCP `projects/../cryptoKeys/..` name, or Azure key URL.

use crate::util::configured_env;
use anyhow::{anyhow, bail, Context};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::time::Duration;

/// Returns the key as base64, ready for `CredentialCipher::from_base64_key`.
pub async fn credentials_key(stored: &str) -> anyhow::Result<String> {
    let provider = configured_env("CREDENTIALS_KEY_PROVIDER").unwrap_or_else(|| "env".into());
    if provider == "env" {
        return Ok(stored.to_string());
    }
    let key_id = configured_env("CREDENTIALS_KMS_KEY")
        .ok_or_else(|| anyhow!("CREDENTIALS_KMS_KEY must be set when CREDENTIALS_KEY_PROVIDER={provider}"))?;
    let http = reqwest::Client::builder().timeout(Duration::from_secs(10)).build()?;
    let raw = match provider.as_str() {
        "vault" => vault_decrypt(&http, &key_id, stored).await,
        "aws" => aws_decrypt(&http, &key_id, stored).await,
        "gcp" => gcp_decrypt(&http, &key_id, stored).await,
        "azure" => azure_decrypt(&http, &key_id, stored).await,
        other => bail!("unknown CREDENTIALS_KEY_PROVIDER {other:?} (env, vault, aws, gcp or azure)"),
    }
    .with_context(|| format!("unwrapping CREDENTIALS_ENCRYPTION_KEY with {provider}"))?;
    Ok(STANDARD.encode(raw))
}

async fn json_ok(resp: reqwest::Response) -> anyhow::Result<Value> {
    let status = resp.status();
    let body = resp.text().await?;
    if !status.is_success() {
        bail!("KMS answered {status}: {}", body.chars().take(300).collect::<String>());
    }
    Ok(serde_json::from_str(&body)?)
}

fn field<'a>(v: &'a Value, path: &[&str]) -> anyhow::Result<&'a str> {
    path.iter()
        .try_fold(v, |v, k| v.get(k))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("KMS response has no {}", path.join(".")))
}

// Vault / OpenBao transit: `vault write transit/encrypt/<key> plaintext=<base64 key>`
// gives the `vault:v1:...` string to store.
async fn vault_decrypt(http: &reqwest::Client, key: &str, stored: &str) -> anyhow::Result<Vec<u8>> {
    let addr = configured_env("VAULT_ADDR").ok_or_else(|| anyhow!("VAULT_ADDR must be set"))?;
    let token = configured_env("VAULT_TOKEN").ok_or_else(|| anyhow!("VAULT_TOKEN must be set"))?;
    let mount = configured_env("VAULT_TRANSIT_MOUNT").unwrap_or_else(|| "transit".into());
    let mut req = http
        .post(format!("{}/v1/{mount}/decrypt/{key}", addr.trim_end_matches('/')))
        .header("X-Vault-Token", token)
        .json(&json!({ "ciphertext": stored }));
    if let Some(ns) = configured_env("VAULT_NAMESPACE") {
        req = req.header("X-Vault-Namespace", ns);
    }
    let v = json_ok(req.send().await?).await?;
    Ok(STANDARD.decode(field(&v, &["data", "plaintext"])?)?)
}

// GCP Cloud KMS, token from the metadata server (GCE, GKE workload identity,
// Cloud Run). ponytail: no service-account JSON key file; add if someone runs
// outside Google Cloud and needs it.
async fn gcp_decrypt(http: &reqwest::Client, key: &str, stored: &str) -> anyhow::Result<Vec<u8>> {
    let tok = json_ok(
        http.get("http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token")
            .header("Metadata-Flavor", "Google")
            .send()
            .await
            .context("GCP metadata server unreachable (not on Google Cloud?)")?,
    )
    .await?;
    let v = json_ok(
        http.post(format!("https://cloudkms.googleapis.com/v1/{key}:decrypt"))
            .bearer_auth(field(&tok, &["access_token"])?)
            .json(&json!({ "ciphertext": stored }))
            .send()
            .await?,
    )
    .await?;
    Ok(STANDARD.decode(field(&v, &["plaintext"])?)?)
}

// Azure Key Vault (RSA-OAEP-256), token from managed identity (IMDS;
// `AZURE_CLIENT_ID` picks a user-assigned identity). ponytail: no App Service
// IDENTITY_ENDPOINT or client-secret login; add if someone needs it.
async fn azure_decrypt(http: &reqwest::Client, key_url: &str, stored: &str) -> anyhow::Result<Vec<u8>> {
    let mut url = "http://169.254.169.254/metadata/identity/oauth2/token?api-version=2018-02-01&resource=https%3A%2F%2Fvault.azure.net".to_string();
    if let Some(id) = configured_env("AZURE_CLIENT_ID") {
        url.push_str(&format!("&client_id={id}"));
    }
    let tok = json_ok(
        http.get(url)
            .header("Metadata", "true")
            .send()
            .await
            .context("Azure IMDS unreachable (no managed identity?)")?,
    )
    .await?;
    let v = json_ok(
        http.post(format!("{}/decrypt?api-version=7.4", key_url.trim_end_matches('/')))
            .bearer_auth(field(&tok, &["access_token"])?)
            .json(&json!({ "alg": "RSA-OAEP-256", "value": to_base64url(stored) }))
            .send()
            .await?,
    )
    .await?;
    Ok(URL_SAFE_NO_PAD.decode(field(&v, &["value"])?.trim_end_matches('='))?)
}

/// `az keyvault key encrypt` prints standard base64; the REST API wants base64url.
fn to_base64url(s: &str) -> String {
    s.trim_end_matches('=').replace('+', "-").replace('/', "_")
}

struct AwsCreds {
    access: String,
    secret: String,
    token: Option<String>,
}

// AWS KMS Decrypt, signed with SigV4 by hand (no AWS SDK). Credentials from
// AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY[/AWS_SESSION_TOKEN], else the ECS
// task role, else the EC2 instance role (IMDSv2). ponytail: no EKS IRSA
// (web identity) or SSO profiles; add if someone needs them.
async fn aws_decrypt(http: &reqwest::Client, key_arn: &str, stored: &str) -> anyhow::Result<Vec<u8>> {
    let region = configured_env("AWS_REGION")
        .or_else(|| configured_env("AWS_DEFAULT_REGION"))
        .or_else(|| key_arn.split(':').nth(3).filter(|r| !r.is_empty()).map(String::from))
        .ok_or_else(|| anyhow!("set AWS_REGION or use a full key ARN"))?;
    let creds = aws_creds(http).await?;
    let host = format!("kms.{region}.amazonaws.com");
    let body = serde_json::to_vec(&json!({ "CiphertextBlob": stored, "KeyId": key_arn }))?;
    let amz_date = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let mut headers = vec![
        ("content-type", "application/x-amz-json-1.1".to_string()),
        ("host", host.clone()),
        ("x-amz-date", amz_date.clone()),
        ("x-amz-target", "TrentService.Decrypt".to_string()),
    ];
    if let Some(t) = &creds.token {
        headers.push(("x-amz-security-token", t.clone()));
        headers.sort();
    }
    let auth = sigv4_authorization(
        "POST", "/", "", &headers, &body, &creds.access, &creds.secret, &region, "kms", &amz_date,
    );
    let mut req = http.post(format!("https://{host}/")).header("authorization", auth).body(body);
    for (k, v) in headers.iter().filter(|(k, _)| *k != "host") {
        req = req.header(*k, v);
    }
    let v = json_ok(req.send().await?).await?;
    Ok(STANDARD.decode(field(&v, &["Plaintext"])?)?)
}

async fn aws_creds(http: &reqwest::Client) -> anyhow::Result<AwsCreds> {
    if let (Some(access), Some(secret)) =
        (configured_env("AWS_ACCESS_KEY_ID"), configured_env("AWS_SECRET_ACCESS_KEY"))
    {
        return Ok(AwsCreds { access, secret, token: configured_env("AWS_SESSION_TOKEN") });
    }
    let v = if let Some(uri) = configured_env("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI") {
        json_ok(http.get(format!("http://169.254.170.2{uri}")).send().await?).await?
    } else {
        let imds = "http://169.254.169.254/latest";
        let tok = http
            .put(format!("{imds}/api/token"))
            .header("X-aws-ec2-metadata-token-ttl-seconds", "300")
            .send()
            .await
            .context("no AWS credentials in env and EC2 metadata unreachable")?
            .error_for_status()?
            .text()
            .await?;
        let role = http
            .get(format!("{imds}/meta-data/iam/security-credentials/"))
            .header("X-aws-ec2-metadata-token", &tok)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        let role = role.lines().next().ok_or_else(|| anyhow!("EC2 instance has no IAM role"))?;
        json_ok(
            http.get(format!("{imds}/meta-data/iam/security-credentials/{role}"))
                .header("X-aws-ec2-metadata-token", &tok)
                .send()
                .await?,
        )
        .await?
    };
    Ok(AwsCreds {
        access: field(&v, &["AccessKeyId"])?.to_string(),
        secret: field(&v, &["SecretAccessKey"])?.to_string(),
        token: Some(field(&v, &["Token"])?.to_string()),
    })
}

fn hmac_sha256(key: &[u8], msg: &str) -> Vec<u8> {
    let mut m = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes any key length");
    m.update(msg.as_bytes());
    m.finalize().into_bytes().to_vec()
}

/// SigV4 `Authorization` header. `headers`: lowercase names, sorted by name.
#[allow(clippy::too_many_arguments)]
fn sigv4_authorization(
    method: &str,
    path: &str,
    query: &str,
    headers: &[(&str, String)],
    body: &[u8],
    access: &str,
    secret: &str,
    region: &str,
    service: &str,
    amz_date: &str,
) -> String {
    let date = &amz_date[..8];
    let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{}\n", v.trim())).collect();
    let signed: Vec<&str> = headers.iter().map(|(k, _)| *k).collect();
    let signed = signed.join(";");
    let canonical = format!(
        "{method}\n{path}\n{query}\n{canonical_headers}\n{signed}\n{}",
        hex::encode(Sha256::digest(body))
    );
    let scope = format!("{date}/{region}/{service}/aws4_request");
    let to_sign = format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}", hex::encode(Sha256::digest(canonical)));
    let k = hmac_sha256(format!("AWS4{secret}").as_bytes(), date);
    let k = hmac_sha256(&k, region);
    let k = hmac_sha256(&k, service);
    let k = hmac_sha256(&k, "aws4_request");
    let sig = hex::encode(hmac_sha256(&k, &to_sign));
    format!("AWS4-HMAC-SHA256 Credential={access}/{scope}, SignedHeaders={signed}, Signature={sig}")
}

#[cfg(test)]
mod tests {
    use super::*;

    // AWS's published SigV4 example (IAM ListUsers, "Create a signed AWS API request").
    #[test]
    fn sigv4_matches_aws_example() {
        let headers = [
            ("content-type", "application/x-www-form-urlencoded; charset=utf-8".to_string()),
            ("host", "iam.amazonaws.com".to_string()),
            ("x-amz-date", "20150830T123600Z".to_string()),
        ];
        let auth = sigv4_authorization(
            "GET",
            "/",
            "Action=ListUsers&Version=2010-05-08",
            &headers,
            b"",
            "AKIDEXAMPLE",
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            "us-east-1",
            "iam",
            "20150830T123600Z",
        );
        assert!(auth.ends_with("Signature=5d672d79c15b13162d9279b0855cfba6789a8edb4c82c400e06b5924a6f2b5d7"), "{auth}");
    }

    // aws-sig-v4-test-suite "get-vanilla".
    #[test]
    fn sigv4_matches_get_vanilla() {
        let headers = [
            ("host", "example.amazonaws.com".to_string()),
            ("x-amz-date", "20150830T123600Z".to_string()),
        ];
        let auth = sigv4_authorization(
            "GET", "/", "", &headers, b"", "AKIDEXAMPLE",
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY", "us-east-1", "service", "20150830T123600Z",
        );
        assert!(auth.ends_with("Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"), "{auth}");
    }

    #[test]
    fn base64url_conversion() {
        assert_eq!(to_base64url("ab+/cd=="), "ab-_cd");
    }

    // Against a real Vault; skipped unless VAULT_TEST_CIPHERTEXT is set
    // (it and VAULT_TEST_KEY come from `vault write transit/encrypt/agentraas`).
    #[tokio::test]
    async fn vault_roundtrip() {
        let Some(ct) = configured_env("VAULT_TEST_CIPHERTEXT") else { return };
        let raw = vault_decrypt(&reqwest::Client::new(), "agentraas", &ct).await.unwrap();
        assert_eq!(STANDARD.encode(raw), configured_env("VAULT_TEST_KEY").unwrap());
    }

    #[tokio::test]
    async fn env_provider_passes_key_through() {
        std::env::remove_var("CREDENTIALS_KEY_PROVIDER");
        assert_eq!(credentials_key("abc=").await.unwrap(), "abc=");
    }
}
