//! Automatic record locks: two agents (or two calls of one agent) can't
//! change the same business record at the same moment. A catalog action
//! names the record it changes (`"lock"` in config/services.json: a Stripe
//! customer, a HubSpot contact, a Razorpay payment); with the org's switch
//! on, `agent::handle_request` locks `<service>:<record>:<hash of its id>`
//! for the call through `agentraas_core::resource_lock`, the same lock an
//! explicit `resource_id` takes; a call naming both takes both. A call
//! whose payload doesn't name the record runs without a lock.
//!
//! Custom actions name their record with `lock` when registered.
//!
//! On by default since 0.30.0 (an org without a `record_lock_settings` row
//! is on); an org turns it off when two legitimate calls on one customer at
//! the same moment must both run. A caller can wait for a busy record
//! (`lock_wait_ms`, up to 5 s) instead of getting a 409 at once.
//! Shadow calls never take a lock; they only report one that is held.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::agent::db::{check_org_write_permission, get_user_org_ids};
use crate::auth::{check_dashboard_rate_limit, is_valid_identifier, AuthUser};
use crate::state::{ApiError, SharedState};
use agentraas_core::resource_lock::LockSpec;

pub fn router() -> Router<SharedState> {
    Router::new().route("/api/v1/record-locks", get(get_settings).put(put_settings))
}

/// What a call locks: the id, and how to name it to the agent.
#[derive(Clone)]
pub struct Lock {
    pub id: String,
    pub label: String,
}

/// The locks for this call, in the order to take them: the record the route
/// names (unless the org turned automatic locks off) and an explicit
/// `resource_id`. Both, when both apply: a `resource_id` must not let a
/// call skip its record lock.
pub async fn locks_for(state: &SharedState, org_id: &str, service: &str, spec: Option<&LockSpec>, payload: &Value, explicit: Option<&str>) -> Vec<Lock> {
    let mut locks: Vec<Lock> = lock_for(state, org_id, service, spec, payload).await.into_iter().collect();
    if let Some(id) = explicit.filter(|id| !locks.iter().any(|l| l.id == *id)) {
        locks.push(Lock { id: id.to_string(), label: format!("resource_id {id}") });
    }
    locks.sort_by(|a, b| a.id.cmp(&b.id));
    locks
}

/// The automatic record lock: the record the route names (`spec`, from
/// `ResolvedRoute::lock`) unless the org turned automatic locks off. A
/// settings read that fails means no automatic lock (the call still runs).
pub async fn lock_for(state: &SharedState, org_id: &str, service: &str, spec: Option<&LockSpec>, payload: &Value) -> Option<Lock> {
    let spec = spec?;
    let id = agentraas_core::resource_lock::derived_id(service, spec, payload)?;
    match sqlx::query_scalar::<_, bool>("SELECT enabled FROM record_lock_settings WHERE org_id = $1").bind(org_id).fetch_optional(&state.pg).await {
        Ok(Some(true) | None) => Some(Lock { id, label: format!("{service} {}", spec.record.replace('_', " ")) }),
        Ok(Some(false)) => None,
        Err(err) => {
            tracing::warn!(?err, org_id, "record lock setting unreadable, running without an automatic lock");
            None
        }
    }
}

/// A held call, for `changed_since`.
pub struct Held<'a> {
    pub org_id: &'a str,
    pub service: &'a str,
    pub action: &'a str,
    pub payload: &'a Value,
    pub req_id: &'a str,
    pub held_at: chrono::DateTime<chrono::Utc>,
}

/// For each held call: whether another call changed its record after it
/// was held (for the approver). False when unknown. Each org's setting and
/// each route are looked up once, and Redis is asked once.
#[cfg_attr(not(feature = "enterprise"), allow(dead_code))]
pub async fn changed_since(state: &SharedState, held: &[Held<'_>]) -> Vec<bool> {
    let mut specs: std::collections::HashMap<(&str, &str, &str), Option<LockSpec>> = std::collections::HashMap::new();
    let mut enabled: std::collections::HashMap<&str, bool> = std::collections::HashMap::new();
    let mut checks = Vec::new();
    let mut slot = Vec::with_capacity(held.len());
    for h in held {
        let key = (h.org_id, h.service, h.action);
        if !specs.contains_key(&key) {
            let spec = crate::agent::db::resolve_route(state, h.service, h.action, h.org_id).await.ok().flatten().and_then(|r| r.lock);
            specs.insert(key, spec);
        }
        let id = specs[&key].as_ref().and_then(|spec| agentraas_core::resource_lock::derived_id(h.service, spec, h.payload));
        if id.is_some() && !enabled.contains_key(h.org_id) {
            let on = sqlx::query_scalar::<_, bool>("SELECT enabled FROM record_lock_settings WHERE org_id = $1").bind(h.org_id).fetch_optional(&state.pg).await;
            enabled.insert(h.org_id, matches!(on, Ok(Some(true) | None)));
        }
        slot.push(id.filter(|_| enabled.get(h.org_id) == Some(&true)).map(|id| {
            checks.push((h.org_id, id, h.held_at.timestamp_millis(), h.req_id));
            checks.len() - 1
        }));
    }
    let refs: Vec<(&str, &str, i64, &str)> = checks.iter().map(|(o, id, ms, r)| (*o, id.as_str(), *ms, *r)).collect();
    let answers = match state.redis_conn_result() {
        Ok(mut conn) => agentraas_core::resource_lock::written_since(&mut conn, &refs).await.unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    slot.into_iter().map(|i| i.and_then(|i| answers.get(i).copied()).unwrap_or(false)).collect()
}

/// `lock_wait_ms` from a request (body field, header or MCP argument): how
/// long to wait for a busy record. Capped in `resource_lock::acquire_waiting`.
pub fn wait_ms(raw: Option<&Value>) -> u64 {
    raw.and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))).unwrap_or(0)
}

/// The 409 body for a call refused because another call holds the record.
pub fn locked_body(req_id: &str, lock: &Lock, held_by: Option<(String, i64)>) -> Value {
    let record = &lock.label;
    let (agent, ms) = held_by.unwrap_or_default();
    json!({
        "error": format!("Another call is changing this {record} right now. Retry after it finishes."),
        "code": "resource_locked",
        "record": record,
        "held_by_agent": (!agent.is_empty()).then_some(agent),
        "retry_after_ms": ms,
        "reqId": req_id,
    })
}

#[derive(Deserialize)]
struct OrgQuery {
    org_id: String,
}

async fn check_member(state: &SharedState, user: &AuthUser, org_id: &str) -> Result<(), ApiError> {
    check_dashboard_rate_limit(state, user.sub).await?;
    if !is_valid_identifier(org_id) {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "org_id must be 1-100 characters, letters/numbers/underscore/hyphen only."));
    }
    if !get_user_org_ids(&state.pg, user.sub).await?.iter().any(|o| o == org_id) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "Not a member of this org."));
    }
    Ok(())
}

async fn get_settings(State(state): State<SharedState>, user: AuthUser, Query(q): Query<OrgQuery>) -> Result<Json<Value>, ApiError> {
    check_member(&state, &user, &q.org_id).await?;
    let enabled: Option<bool> = sqlx::query_scalar("SELECT enabled FROM record_lock_settings WHERE org_id = $1").bind(&q.org_id).fetch_optional(&state.pg).await?;
    let mut actions: Vec<String> = state.service_routes.iter().filter(|(_, r)| r.lock.is_some()).map(|(k, _)| k.clone()).collect();
    actions.sort();
    Ok(Json(json!({ "enabled": enabled.unwrap_or(true), "actions": actions })))
}

#[derive(Deserialize)]
struct SettingsBody {
    org_id: String,
    enabled: bool,
}

async fn put_settings(State(state): State<SharedState>, user: AuthUser, Json(b): Json<SettingsBody>) -> Result<Json<Value>, ApiError> {
    check_member(&state, &user, &b.org_id).await?;
    if !check_org_write_permission(&state.pg, user.sub, &b.org_id).await? {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "Auditors have read-only access to this org."));
    }
    sqlx::query(
        "INSERT INTO record_lock_settings (org_id, enabled, updated_at) VALUES ($1, $2, NOW())
         ON CONFLICT (org_id) DO UPDATE SET enabled = EXCLUDED.enabled, updated_at = NOW()",
    )
    .bind(&b.org_id)
    .bind(b.enabled)
    .execute(&state.pg)
    .await?;
    Ok(Json(json!({ "enabled": b.enabled })))
}
