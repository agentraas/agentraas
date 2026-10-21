//! Secret references, the part every build has: recognising one. Fetching
//! it from Vault / AWS / GCP / Azure is an Enterprise feature in
//! `ee/secret_refs.rs`; without the `enterprise` build a reference is refused.

const SCHEMES: [&str; 4] = ["vault://", "aws-sm://", "gcp-sm://", "azure-kv://"];

pub fn is_ref(value: &str) -> bool {
    SCHEMES.iter().any(|s| value.starts_with(s))
}

/// Any string value of a credentials object is a reference.
pub fn has_ref(credentials: &serde_json::Value) -> bool {
    credentials.as_object().is_some_and(|o| o.values().filter_map(serde_json::Value::as_str).any(is_ref))
}

/// Returns `value` unchanged unless it is a reference, which is fetched.
pub async fn resolve(value: &str) -> anyhow::Result<String> {
    if !is_ref(value) {
        return Ok(value.to_string());
    }
    #[cfg(feature = "enterprise")]
    return crate::ee::secret_refs::resolve(value).await;
    #[cfg(not(feature = "enterprise"))]
    anyhow::bail!("secret references need the Enterprise edition")
}

/// Rejects a malformed reference before it is saved.
pub fn check(reference: &str) -> anyhow::Result<()> {
    #[cfg(feature = "enterprise")]
    return crate::ee::secret_refs::check(reference);
    #[cfg(not(feature = "enterprise"))]
    anyhow::bail!("secret references need the Enterprise edition ({reference})")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn recognises_references_only() {
        assert!(is_ref("vault://secret/data/stripe#api_key"));
        assert!(is_ref("aws-sm://arn:aws:secretsmanager:us-east-1:1:secret:x"));
        assert!(!is_ref("sk_live_123"));
        assert!(!is_ref("https://example.com"));
        assert!(has_ref(&json!({ "api_key": "gcp-sm://projects/p/secrets/s" })));
        assert!(!has_ref(&json!({ "username": "u", "password": "p" })));
    }

    #[tokio::test]
    async fn plain_values_pass_through() {
        assert_eq!(resolve("sk_test_1").await.unwrap(), "sk_test_1");
    }
}
