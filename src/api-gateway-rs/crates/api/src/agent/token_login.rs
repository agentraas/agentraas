//! Services that trade the stored credential for a token that expires
//! (PayPal, PhonePe, Shiprocket): log in with the org's credential, keep the
//! token in Redis (encrypted, like stored credentials) until a minute before
//! it expires, and drop it when the service answers 401.

use std::time::Duration;

use agentraas_core::config::TokenLogin;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::db::Credential;
use crate::state::SharedState;

fn cache_key(service: &str, org_id: &str, end_user_id: Option<&str>) -> String {
    format!("svc_token:{service}:{org_id}:{}", end_user_id.unwrap_or("-"))
}

/// Ties a cached token to the credential it came from, so saving a new key
/// logs in again instead of reusing the old key's token.
fn credential_hash(cred: &Credential) -> String {
    let mut h = Sha256::new();
    for part in [&cred.api_key, &cred.username, &cred.password] {
        h.update(part.as_deref().unwrap_or(""));
        h.update([0]);
    }
    hex::encode(&h.finalize()[..8])
}

fn login_body(spec: &TokenLogin, cred: &Credential) -> serde_json::Map<String, Value> {
    spec.body
        .iter()
        .map(|(k, v)| {
            let v = match v.as_str() {
                Some("{username}") => Value::from(cred.username.clone().unwrap_or_default()),
                Some("{password}") => Value::from(cred.password.clone().unwrap_or_default()),
                _ => v.clone(),
            };
            (k.clone(), v)
        })
        .collect()
}

/// Seconds to keep the token: its own lifetime from the login answer, less a
/// minute so a call never goes out with a token about to expire.
fn cache_seconds(spec: &TokenLogin, answer: &Value, now: i64) -> i64 {
    let number = |field: &Option<String>| {
        let v = answer.get(field.as_deref()?)?;
        v.as_i64().or_else(|| v.as_str()?.parse().ok())
    };
    let lifetime = number(&spec.expires_in_field)
        .or_else(|| number(&spec.expires_at_field).map(|at| at - now))
        .unwrap_or(spec.ttl_seconds);
    (lifetime - 60).max(30)
}

/// The token to send. An error is a setup problem (wrong credentials, the
/// login refused), answered as such without touching the circuit breaker.
pub async fn token(
    state: &SharedState,
    spec: &TokenLogin,
    service: &str,
    org_id: &str,
    end_user_id: Option<&str>,
    cred: &Credential,
) -> Result<String, String> {
    // A token saved as a plain key (PayPal before it logged in itself) is
    // used as it is, until it expires.
    if cred.username.is_none() && cred.password.is_none() {
        if let Some(key) = &cred.api_key {
            return Ok(key.clone());
        }
    }
    let key = cache_key(service, org_id, end_user_id);
    let hash = credential_hash(cred);
    let mut conn = state.redis_conn_result().ok();
    if let Some(conn) = conn.as_mut() {
        let stored: Option<String> = redis::cmd("GET").arg(&key).query_async(conn).await.unwrap_or(None);
        if let Some((h, encrypted)) = stored.as_deref().and_then(|s| s.split_once(':')) {
            if h == hash {
                if let Ok(token) = state.cipher.decrypt(encrypted) {
                    return Ok(token);
                }
            }
        }
    }

    // ponytail: two calls at once with no cached token both log in; fine
    // while each login stays valid on its own (true for the three services).
    let mut req = state.http_client.post(&spec.url).timeout(Duration::from_secs(15));
    if spec.basic_auth {
        req = req.basic_auth(cred.username.clone().unwrap_or_default(), cred.password.clone());
    }
    let body = login_body(spec, cred);
    req = if spec.content_type.as_deref() == Some("application/x-www-form-urlencoded") {
        let form: Vec<(String, String)> = body.iter().map(|(k, v)| (k.clone(), v.as_str().map_or_else(|| v.to_string(), str::to_string))).collect();
        req.form(&form)
    } else {
        req.json(&body)
    };
    let response = req.send().await.map_err(|err| format!("Could not reach {service} to log in: {err}"))?;
    let status = response.status();
    let answer: Value = response.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        return Err(format!(
            "{service} refused the login with the saved credentials (HTTP {}). Check them in the dashboard's Credentials panel.",
            status.as_u16()
        ));
    }
    let Some(token) = answer.get(&spec.token_field).and_then(Value::as_str) else {
        return Err(format!("{service} answered the login without a token in \"{}\".", spec.token_field));
    };
    if let Some(conn) = conn.as_mut() {
        let seconds = cache_seconds(spec, &answer, chrono::Utc::now().timestamp());
        let _: Result<(), _> = redis::cmd("SET")
            .arg(&key)
            .arg(format!("{hash}:{}", state.cipher.encrypt(token)))
            .arg("EX")
            .arg(seconds)
            .query_async(conn)
            .await;
    }
    Ok(token.to_string())
}

/// After a 401: the token was revoked or expired early, so the next call
/// logs in again.
pub async fn forget(state: &SharedState, service: &str, org_id: &str, end_user_id: Option<&str>) {
    if let Ok(mut conn) = state.redis_conn_result() {
        let _: Result<(), _> = redis::cmd("DEL").arg(cache_key(service, org_id, end_user_id)).query_async(&mut conn).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(extra: Value) -> TokenLogin {
        let mut v = serde_json::json!({"url": "https://example.test/token", "tokenField": "access_token"});
        v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn token_kept_for_its_own_lifetime_less_a_minute() {
        let now = 1_000_000;
        let s = spec(serde_json::json!({"expiresInField": "expires_in", "expiresAtField": "expires_at", "ttlSeconds": 864000}));
        assert_eq!(cache_seconds(&s, &serde_json::json!({"expires_in": 32400}), now), 32340); // PayPal
        assert_eq!(cache_seconds(&s, &serde_json::json!({"expires_at": now + 3600}), now), 3540); // PhonePe
        assert_eq!(cache_seconds(&s, &serde_json::json!({"expires_in": "600"}), now), 540);
        assert_eq!(cache_seconds(&s, &serde_json::json!({}), now), 863940); // Shiprocket: fallback
        assert_eq!(cache_seconds(&s, &serde_json::json!({"expires_at": now - 5}), now), 30);
    }

    #[test]
    fn login_body_fills_the_stored_credential() {
        let s = spec(serde_json::json!({"body": {"client_id": "{username}", "client_secret": "{password}", "grant_type": "client_credentials"}}));
        let cred = Credential { api_key: None, username: Some("ID".into()), password: Some("SECRET".into()) };
        let body = login_body(&s, &cred);
        assert_eq!(body["client_id"], "ID");
        assert_eq!(body["client_secret"], "SECRET");
        assert_eq!(body["grant_type"], "client_credentials");
        assert_eq!(s.prefix, "Bearer");
    }

    #[test]
    fn a_new_credential_misses_the_cached_token() {
        let a = Credential { api_key: None, username: Some("ID".into()), password: Some("old".into()) };
        let b = Credential { password: Some("new".into()), ..a.clone() };
        assert_ne!(credential_hash(&a), credential_hash(&b));
    }
}
