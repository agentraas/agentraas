//! Shadow mode: an agent runs against real traffic, and every write it makes
//! is evaluated and recorded but never sent. Set per agent, optionally
//! narrowed to one service or one service.action (`shadow_rules`, the most
//! specific row wins).
//!
//! `agent::handle_request` asks `rule_for` after auth and the rate limit,
//! before anything that claims, counts or forwards. A shadowed call then
//! goes through `answer`: the live gates are evaluated without their side
//! effects (the dedup slot is read, never claimed; spend caps are peeked,
//! never counted; policy `max_from` lookups, which call the provider, are
//! skipped), the outcome is written to the audit log as status `shadow`
//! with the decision in `error_type`, and the agent gets a synthetic answer
//! marked `"shadow": true`. Credentials are only ever read by the forwarder,
//! which a shadowed call never reaches. Shadow rows are not usage: billing
//! counts `success` rows only.
//!
//! Reads (GET/HEAD routes) are forwarded as usual unless the rule sets
//! `block_reads`. MCP tools have no method, so they always count as writes.
//! If the rule can't be read, the caller refuses the call (fail closed).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use agentraas_core::dedup;

use crate::agent::db::{check_org_write_permission, get_effective_dedup_rule, get_effective_validation_rule, get_user_org_ids, log_audit, select_dedup_hash_mode, DedupHashMode};
use crate::agent::{Response, Target};
use crate::auth::{check_dashboard_rate_limit, is_valid_identifier, AuthUser};
use crate::state::{ApiError, SharedState};

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/v1/shadow-rules", get(list_rules).post(create_rule))
        .route("/api/v1/shadow-rules/:id", axum::routing::delete(delete_rule))
        .route("/api/v1/shadow-rules/promote", post(promote))
        .route("/api/v1/shadow-calls", get(list_calls))
}

pub(crate) struct Rule {
    pub block_reads: bool,
    pub stub: Value,
}

/// The shadow rule covering this call, if any. An error means "can't tell":
/// the caller must not forward.
pub(crate) async fn rule_for(state: &SharedState, org_id: &str, agent_id: &str, service: &str, action: &str) -> Result<Option<Rule>, sqlx::Error> {
    let row: Option<(bool, Value)> = sqlx::query_as(
        "SELECT block_reads, stub FROM shadow_rules
         WHERE org_id = $1 AND agent_id = $2 AND (service IS NULL OR service = $3) AND (action IS NULL OR action = $4)
         ORDER BY action NULLS LAST, service NULLS LAST LIMIT 1",
    )
    .bind(org_id)
    .bind(agent_id)
    .bind(service)
    .bind(action)
    .fetch_optional(&state.pg)
    .await?;
    Ok(row.map(|(block_reads, stub)| Rule { block_reads, stub }))
}

pub(crate) fn is_read(target: &Target) -> bool {
    matches!(target, Target::Http(r) if r.method.eq_ignore_ascii_case("GET") || r.method.eq_ignore_ascii_case("HEAD"))
}

/// What the live pipeline would have done, gate by gate in the same order,
/// as (decision, detail). Nothing here writes to Redis or calls out.
#[allow(clippy::too_many_arguments)]
#[cfg_attr(not(feature = "enterprise"), allow(unused_variables))]
async fn decide(
    state: &SharedState,
    webhook: bool,
    mcp: bool,
    org_id: &str,
    agent_id: &str,
    service: &str,
    action: &str,
    payload: &Value,
    dedup_hash: &str,
) -> (String, String) {
    let unknown = |gate: &str| ("would_error".to_string(), format!("The {gate} check could not be read; the live call would have failed."));

    let Ok(mut conn) = state.redis_conn_result() else { return unknown("duplicate") };
    match dedup::read_dedup_slot(&mut conn, &format!("dedup:{dedup_hash}")).await {
        Ok(Some(_)) => return ("would_deduplicate".into(), "An identical live call is already recorded; it would have been answered from cache.".into()),
        Ok(None) => {}
        Err(_) => return unknown("duplicate"),
    }

    if let Ok(Some(rule)) = get_effective_validation_rule(state, org_id, service, action).await {
        if let Some(first) = agentraas_core::validator::validate_all(payload, &rule.fields).first() {
            return ("would_block:validation_failed".into(), first.message.clone());
        }
    }

    // As live: a policy or spend cap can hold any non-MCP call for approval
    // (enterprise build); approval rules (below) only match webhook calls.
    let can_hold = !mcp && cfg!(feature = "enterprise");
    let mut note = String::new();
    match crate::action_policies::check_offline(state, org_id, agent_id, service, action, payload).await {
        Ok((Some(v), _)) if v.hitl && can_hold => return (format!("would_need_approval:{}", v.reason), v.message),
        Ok((Some(v), _)) => return (format!("would_block:{}", v.reason), v.message),
        Ok((None, true)) => note = " A policy limit read from the provider (max_from) was not checked in shadow mode.".into(),
        Ok((None, false)) => {}
        Err(_) => return unknown("action policy"),
    }

    match crate::spend_caps::peek(state, org_id, agent_id, service, action, payload).await {
        Ok(Some((on_exceed, message))) if on_exceed == "hitl" && can_hold => return ("would_need_approval:spend_cap_exceeded".into(), message),
        Ok(Some((_, message))) => return ("would_block:spend_cap_exceeded".into(), message),
        Ok(None) => {}
        Err(_) => return unknown("spend cap"),
    }

    #[cfg(feature = "enterprise")]
    if webhook && crate::agent::db::effective_tier(state, org_id).await >= agentraas_core::tier::Tier::Team {
        match crate::ee::hitl::match_rule(&state.pg, org_id, service, action, payload).await {
            Ok(Some(_)) => return ("would_need_approval:hitl_rule".into(), "An approval rule matches this call.".into()),
            Ok(None) => {}
            Err(_) => return unknown("approval rule"),
        }
    }
    ("would_execute".into(), format!("Every gate passed; the live call would have been sent.{note}"))
}

/// Records a shadowed call and answers it. Never forwards.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn answer(
    state: &SharedState,
    webhook: bool,
    mcp: bool,
    req_id: &str,
    api_key: &str,
    org_id: &str,
    agent_id: &str,
    service: &str,
    action: &str,
    payload: &Value,
    idempotency_key: Option<&str>,
    end_user_id: Option<&str>,
    run_id: Option<&str>,
    step_id: Option<&str>,
    rule: Rule,
) -> Response {
    let start = std::time::Instant::now();
    // The same hash the live call would claim, so a would-be duplicate is seen.
    let dedup_rule = get_effective_dedup_rule(&state.pg, org_id, service, action).await.unwrap_or(None);
    let dedup_hash = match select_dedup_hash_mode(idempotency_key, dedup_rule.as_ref()) {
        DedupHashMode::IdempotencyKey(idem) => dedup::hash_idempotency_key(api_key, service, action, idem, end_user_id),
        DedupHashMode::Fields { fields, normalize } => dedup::hash_field_values(api_key, service, action, payload, fields, normalize, end_user_id),
        DedupHashMode::Payload => dedup::hash_payload(api_key, service, action, payload, end_user_id),
    };
    let (decision, detail) = decide(state, webhook, mcp, org_id, agent_id, service, action, payload, &dedup_hash).await;
    log_audit(&state.pg, req_id, api_key, org_id, agent_id, service, action, "shadow", Some(&decision), start.elapsed().as_millis() as i64, Some(&dedup_hash), state.enterprise_mode, Some(payload), run_id, step_id, end_user_id).await;
    (
        StatusCode::OK,
        Json(json!({
            "shadow": true,
            "decision": decision,
            "detail": detail,
            "reqId": req_id,
            "response": rule.stub,
            "message": format!("Shadow mode: nothing was sent to {service}. This is a placeholder answer, not the provider's."),
        })),
    )
        .into()
}

// ─── dashboard API ───

async fn check_member(state: &SharedState, user: &AuthUser, org_id: &str, write: bool) -> Result<(), ApiError> {
    check_dashboard_rate_limit(state, user.sub).await?;
    if !is_valid_identifier(org_id) {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "org_id must be 1-100 characters, letters/numbers/underscore/hyphen only."));
    }
    if !get_user_org_ids(&state.pg, user.sub).await?.iter().any(|o| o == org_id) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "Not a member of this org."));
    }
    if write && !check_org_write_permission(&state.pg, user.sub, org_id).await? {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "Auditors have read-only access to this org."));
    }
    Ok(())
}

#[derive(Deserialize)]
struct OrgQuery {
    org_id: String,
}

async fn list_rules(State(state): State<SharedState>, user: AuthUser, Query(q): Query<OrgQuery>) -> Result<Json<Value>, ApiError> {
    check_member(&state, &user, &q.org_id, false).await?;
    let rows: Vec<(i32, String, Option<String>, Option<String>, bool, Value, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT id, agent_id, service, action, block_reads, stub, created_at FROM shadow_rules WHERE org_id = $1 ORDER BY agent_id, service NULLS FIRST, action NULLS FIRST",
    )
    .bind(&q.org_id)
    .fetch_all(&state.pg)
    .await?;
    let rules: Vec<Value> = rows
        .into_iter()
        .map(|(id, agent_id, service, action, block_reads, stub, created_at)| {
            json!({ "id": id, "agent_id": agent_id, "service": service, "action": action, "block_reads": block_reads, "stub": stub, "created_at": created_at })
        })
        .collect();
    Ok(Json(json!({ "rules": rules })))
}

#[derive(Deserialize)]
struct CreateBody {
    org_id: String,
    agent_id: String,
    service: Option<String>,
    action: Option<String>,
    #[serde(default)]
    block_reads: bool,
    stub: Option<Value>,
}

async fn create_rule(State(state): State<SharedState>, user: AuthUser, Json(b): Json<CreateBody>) -> Result<Json<Value>, ApiError> {
    check_member(&state, &user, &b.org_id, true).await?;
    let blank = |s: Option<String>| s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let (service, action) = (blank(b.service), blank(b.action));
    if !is_valid_identifier(&b.agent_id) {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "agent_id must be 1-100 characters, letters/numbers/underscore/hyphen only."));
    }
    if action.is_some() && service.is_none() {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "An action needs its service."));
    }
    if [&service, &action].iter().any(|s| s.as_ref().is_some_and(|s| s.len() > 100)) {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "service and action are at most 100 characters."));
    }
    let stub = b.stub.unwrap_or_else(|| json!({}));
    if stub.to_string().len() > 10_000 {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "stub is at most 10,000 characters of JSON."));
    }
    let id: i32 = sqlx::query_scalar(
        "INSERT INTO shadow_rules (org_id, agent_id, service, action, block_reads, stub, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT (org_id, agent_id, COALESCE(service, ''), COALESCE(action, '')) DO UPDATE SET block_reads = EXCLUDED.block_reads, stub = EXCLUDED.stub
         RETURNING id",
    )
    .bind(&b.org_id)
    .bind(&b.agent_id)
    .bind(&service)
    .bind(&action)
    .bind(b.block_reads)
    .bind(&stub)
    .bind(user.sub)
    .fetch_one(&state.pg)
    .await?;
    Ok(Json(json!({ "id": id })))
}

async fn delete_rule(State(state): State<SharedState>, user: AuthUser, Path(id): Path<i32>, Query(q): Query<OrgQuery>) -> Result<Json<Value>, ApiError> {
    check_member(&state, &user, &q.org_id, true).await?;
    let deleted = sqlx::query("DELETE FROM shadow_rules WHERE id = $1 AND org_id = $2").bind(id).bind(&q.org_id).execute(&state.pg).await?.rows_affected();
    Ok(Json(json!({ "deleted": deleted })))
}

#[derive(Deserialize)]
struct PromoteBody {
    org_id: String,
    agent_id: String,
}

/// Takes the agent out of shadow mode. Recorded calls are not replayed: the
/// agent's next real call is its first live one.
async fn promote(State(state): State<SharedState>, user: AuthUser, Json(b): Json<PromoteBody>) -> Result<Json<Value>, ApiError> {
    check_member(&state, &user, &b.org_id, true).await?;
    let removed = sqlx::query("DELETE FROM shadow_rules WHERE org_id = $1 AND agent_id = $2").bind(&b.org_id).bind(&b.agent_id).execute(&state.pg).await?.rows_affected();
    Ok(Json(json!({ "agent_id": b.agent_id, "rules_removed": removed, "replayed": 0 })))
}

#[derive(Deserialize)]
struct CallsQuery {
    org_id: String,
    agent_id: Option<String>,
    service: Option<String>,
    decision: Option<String>,
    limit: Option<i64>,
}

async fn list_calls(State(state): State<SharedState>, user: AuthUser, Query(q): Query<CallsQuery>) -> Result<Json<Value>, ApiError> {
    check_member(&state, &user, &q.org_id, false).await?;
    #[derive(sqlx::FromRow)]
    struct Row {
        req_id: String,
        agent_id: Option<String>,
        service: String,
        action: String,
        error_type: Option<String>,
        redacted_payload_preview: Option<String>,
        created_at: chrono::NaiveDateTime,
    }
    // `decision` filters by prefix: "would_block" matches every block reason.
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT req_id, agent_id, service, action, error_type, redacted_payload_preview, created_at FROM audit_log
         WHERE org_id = $1 AND status = 'shadow' AND ($2::text IS NULL OR agent_id = $2) AND ($3::text IS NULL OR service = $3)
           AND ($4::text IS NULL OR starts_with(error_type, $4))
         ORDER BY created_at DESC LIMIT $5",
    )
    .bind(&q.org_id)
    .bind(&q.agent_id)
    .bind(&q.service)
    .bind(&q.decision)
    .bind(q.limit.unwrap_or(100).clamp(1, 1000))
    .fetch_all(&state.pg)
    .await?;
    let calls: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({ "req_id": r.req_id, "agent_id": r.agent_id, "service": r.service, "action": r.action, "decision": r.error_type,
                    "payload_preview": r.redacted_payload_preview, "created_at": r.created_at.and_utc() })
        })
        .collect();
    Ok(Json(json!({ "calls": calls })))
}
