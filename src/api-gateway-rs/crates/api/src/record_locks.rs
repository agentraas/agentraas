//! Automatic record locks: two agents (or two calls of one agent) can't
//! change the same business record at the same moment. A catalog action
//! names the record it changes (`"lock"` in config/services.json: a Stripe
//! customer, a HubSpot contact, a Razorpay payment); with the org's switch
//! on, `agent::handle_request` locks `<service>:<record>:<hash of its id>`
//! for the call through `agentraas_core::resource_lock`, the same lock an
//! explicit `resource_id` takes. Explicit `resource_id` still wins. A call
//! whose payload doesn't name the record runs without a lock.
//!
//! Off by default (`record_lock_settings`): two legitimate calls on one
//! customer at the same moment would get a 409 and have to retry.
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

pub fn router() -> Router<SharedState> {
    Router::new().route("/api/v1/record-locks", get(get_settings).put(put_settings))
}

/// What a call locks: the id, and how to name it to the agent.
#[derive(Clone)]
pub struct Lock {
    pub id: String,
    pub label: String,
}

/// The lock for this call: the explicit `resource_id`, or the record the
/// catalog action names when the org has automatic locks on. A settings read
/// that fails means no automatic lock (the call still runs, as before).
pub async fn lock_for(state: &SharedState, org_id: &str, service: &str, action: &str, payload: &Value, explicit: Option<&str>) -> Option<Lock> {
    if let Some(id) = explicit {
        return Some(Lock { id: id.to_string(), label: format!("resource_id {id}") });
    }
    let route = state.service_routes.get(&format!("{service}.{action}"))?;
    let spec = route.lock.as_ref()?;
    let id = agentraas_core::resource_lock::derived_id(service, spec, payload)?;
    match sqlx::query_scalar::<_, bool>("SELECT enabled FROM record_lock_settings WHERE org_id = $1").bind(org_id).fetch_optional(&state.pg).await {
        Ok(Some(true)) => Some(Lock { id, label: format!("{service} {}", spec.record.replace('_', " ")) }),
        Ok(_) => None,
        Err(err) => {
            tracing::warn!(?err, org_id, "record lock setting unreadable, running without an automatic lock");
            None
        }
    }
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
    Ok(Json(json!({ "enabled": enabled.unwrap_or(false), "actions": actions })))
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
