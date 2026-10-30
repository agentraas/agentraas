//! Cross-agent resource lock — distinct from dedup. Dedup catches a
//! retry of the SAME request (identical payload/idempotency key); this
//! catches two DIFFERENT requests (from the same agent, or two different
//! agents) racing to act on the same declared `resource_id` at the same
//! time — e.g. Agent A and Agent B both updating Stripe customer
//! `cus_123`'s subscription within the same second.
//!
//! Opt-in and additive: a caller that never sends `resource_id` sees no
//! behavior change at all.

use redis::aio::MultiplexedConnection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// `"lock": {"record": "customer", "from": ["customer"]}` on a catalog
/// action: the record it changes, and the payload paths holding its id
/// (first non-empty wins; dotted, array items by index).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockSpec {
    pub record: String,
    pub from: Vec<String>,
}

/// The lock id for a call, from its payload: `<service>:<record>:<sha256>`,
/// so ids from two services never collide and no customer id or email sits
/// in Redis in clear text. `None` when the payload doesn't name the record.
pub fn derived_id(service: &str, spec: &LockSpec, payload: &serde_json::Value) -> Option<String> {
    let value = spec.from.iter().find_map(|path| {
        let v = path.split('.').try_fold(payload, |cur, key| cur.get(key).or_else(|| key.parse::<usize>().ok().and_then(|i| cur.get(i))))?;
        let s = match v {
            serde_json::Value::String(s) => s.trim().to_string(),
            serde_json::Value::Number(n) => n.to_string(),
            _ => return None,
        };
        (!s.is_empty()).then(|| if s.contains('@') { s.to_lowercase() } else { s })
    })?;
    Some(format!("{service}:{}:{}", spec.record, hex::encode(Sha256::digest(value.as_bytes()))))
}

pub struct LockResult {
    pub acquired: bool,
    /// Value stored under the key (a random token, then the owning agent),
    /// so `release` only deletes a lock this caller still owns (not one
    /// re-acquired after its TTL ran out).
    pub token: String,
    /// When not acquired: the agent holding it and its remaining ms.
    pub held_by: Option<(String, i64)>,
}

pub async fn acquire(
    conn: &mut MultiplexedConnection,
    org_id: &str,
    resource_id: &str,
    agent_id: &str,
    ttl_seconds: i64,
) -> redis::RedisResult<LockResult> {
    let token = format!("{:032x}|{agent_id}", rand::random::<u128>());
    let result: Option<String> = redis::cmd("SET")
        .arg(key(org_id, resource_id))
        .arg(&token)
        .arg("EX")
        .arg(ttl_seconds)
        .arg("NX")
        .query_async(conn)
        .await?;
    if result.as_deref() == Some("OK") {
        return Ok(LockResult { acquired: true, token, held_by: None });
    }
    let held_by = holder(conn, org_id, resource_id).await?;
    Ok(LockResult { acquired: false, token, held_by })
}

/// Who holds a lock and for how many more ms, without touching it (shadow
/// mode reads this; it never takes a lock).
pub async fn holder(conn: &mut MultiplexedConnection, org_id: &str, resource_id: &str) -> redis::RedisResult<Option<(String, i64)>> {
    let (value, pttl): (Option<String>, i64) = redis::pipe().get(key(org_id, resource_id)).pttl(key(org_id, resource_id)).query_async(conn).await?;
    Ok(value.map(|v| (v.split_once('|').map(|(_, agent)| agent.to_string()).unwrap_or_default(), pttl.max(0))))
}

/// Compare-and-delete: a no-op if the lock expired and someone else holds it now.
pub async fn release(
    conn: &mut MultiplexedConnection,
    org_id: &str,
    resource_id: &str,
    token: &str,
) -> redis::RedisResult<()> {
    redis::Script::new("if redis.call('GET', KEYS[1]) == ARGV[1] then return redis.call('DEL', KEYS[1]) end return 0")
        .key(key(org_id, resource_id))
        .arg(token)
        .invoke_async::<i64>(conn)
        .await?;
    Ok(())
}

/// Longest a call may wait for a busy record (`lock_wait_ms`).
pub const MAX_WAIT_MS: u64 = 5000;

/// `acquire`, retried until the lock is free or `wait_ms` (capped at
/// `MAX_WAIT_MS`) has passed. 0 = one try.
pub async fn acquire_waiting(
    conn: &mut MultiplexedConnection,
    org_id: &str,
    resource_id: &str,
    agent_id: &str,
    ttl_seconds: i64,
    wait_ms: u64,
) -> redis::RedisResult<LockResult> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(wait_ms.min(MAX_WAIT_MS));
    loop {
        let got = acquire(conn, org_id, resource_id, agent_id, ttl_seconds).await?;
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if got.acquired || left.is_zero() {
            return Ok(got);
        }
        // ponytail: polling, no FIFO; whoever retries first after release wins.
        let held_ms = got.held_by.as_ref().map_or(50, |(_, ms)| (*ms).max(1) as u64);
        tokio::time::sleep(std::time::Duration::from_millis(held_ms.min(100)).min(left)).await;
    }
}

/// Days a record's last-write mark is kept (it only matters while a held
/// call waits for approval).
const WRITTEN_TTL_SECONDS: i64 = 30 * 86_400;

/// Notes that call `req_id` changed the record now, so a call held for
/// approval can tell the approver the record changed after it was made.
pub async fn mark_written(conn: &mut MultiplexedConnection, org_id: &str, resource_id: &str, req_id: &str) -> redis::RedisResult<()> {
    let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    redis::cmd("SET").arg(written_key(org_id, resource_id)).arg(format!("{now_ms}|{req_id}")).arg("EX").arg(WRITTEN_TTL_SECONDS).query_async(conn).await
}

/// For each (org_id, resource_id, since_ms, req_id): whether a call other
/// than `req_id` changed the record after `since_ms`. One round trip.
pub async fn written_since(conn: &mut MultiplexedConnection, checks: &[(&str, &str, i64, &str)]) -> redis::RedisResult<Vec<bool>> {
    if checks.is_empty() {
        return Ok(Vec::new());
    }
    let mut pipe = redis::pipe();
    for (org_id, resource_id, _, _) in checks {
        pipe.get(written_key(org_id, resource_id));
    }
    let values: Vec<Option<String>> = pipe.query_async(conn).await?;
    Ok(values
        .iter()
        .zip(checks)
        .map(|(v, (_, _, since_ms, req_id))| v.as_deref().and_then(|v| v.split_once('|')).is_some_and(|(ms, by)| by != *req_id && ms.parse::<i64>().is_ok_and(|ms| ms > *since_ms)))
        .collect())
}

/// Checks a `lock` spec submitted with a custom action.
pub fn validate(spec: &LockSpec) -> Result<(), String> {
    let ok_record = !spec.record.is_empty() && spec.record.len() <= 50 && spec.record.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !ok_record {
        return Err("lock.record must be 1-50 letters, digits or underscores (e.g. \"customer\").".into());
    }
    if spec.from.is_empty() || spec.from.len() > 5 || spec.from.iter().any(|p| p.is_empty() || p.len() > 200) {
        return Err("lock.from must list 1-5 payload paths (e.g. [\"customer\", \"billing.email\"]).".into());
    }
    Ok(())
}

fn key(org_id: &str, resource_id: &str) -> String {
    format!("resource_lock:{org_id}:{resource_id}")
}

fn written_key(org_id: &str, resource_id: &str) -> String {
    format!("record_written:{org_id}:{resource_id}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn derived_ids_name_the_record_not_the_call() {
        let spec = LockSpec { record: "customer".into(), from: vec!["customer".into(), "billing.email".into()] };
        let a = derived_id("stripe", &spec, &json!({"customer": "cus_1", "amount": 5})).unwrap();
        assert_eq!(a, derived_id("stripe", &spec, &json!({"customer": " cus_1 ", "amount": 900})).unwrap(), "same record, different call");
        assert!(a.starts_with("stripe:customer:") && !a.contains("cus_1"), "hashed, service-scoped");
        assert_ne!(a, derived_id("hubspot", &spec, &json!({"customer": "cus_1"})).unwrap());
        assert_eq!(
            derived_id("stripe", &spec, &json!({"billing": {"email": "A@x.io"}})),
            derived_id("stripe", &spec, &json!({"billing": {"email": "a@x.io"}})),
            "emails compare case-insensitively"
        );
        assert_eq!(derived_id("stripe", &spec, &json!({"amount": 5})), None, "no record named, no lock");
        assert_eq!(derived_id("stripe", &spec, &json!({"customer": ""})), None);
    }

    #[test]
    fn custom_lock_specs_are_checked() {
        assert!(validate(&LockSpec { record: "order".into(), from: vec!["order_id".into()] }).is_ok());
        assert!(validate(&LockSpec { record: "bad record".into(), from: vec!["x".into()] }).is_err());
        assert!(validate(&LockSpec { record: "order".into(), from: vec![] }).is_err());
        assert!(validate(&LockSpec { record: "order".into(), from: vec!["a".into(); 6] }).is_err());
    }
}
