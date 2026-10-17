//! Per-agent action spend/call caps: a number of calls, a total of a
//! payload field (money: "no more than 10,000 in notional a day"), or
//! both, per hour or day. Windowed Redis counters (INCR / INCRBYFLOAT +
//! EXPIRE), the same shape as `increment_monthly_usage` (agent/db.rs). A
//! rule's timezone sets where its day starts (default UTC).
//!
//! Deliberately NOT under `ee/`, unlike HITL: block-only rules must work
//! on the Community/public build too (this is a safety control on the
//! org's own spend, not a paid feature to gate). The `on_exceed = "hitl"`
//! option requires Tier::Team at creation time (checked here) AND the
//! `enterprise` Cargo feature to be compiled in (also checked here, since
//! the actual freeze call in `agent/mod.rs` is `#[cfg(feature =
//! "enterprise")]`-gated like every other HITL call site) — rejecting at
//! creation time means the enforcement path never has to guess what to do
//! with an 'hitl' rule it can't act on.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::agent::db::{get_user_writable_org_ids, check_org_write_permission, get_user_org_ids, require_tier};
use crate::auth::{check_dashboard_rate_limit, is_valid_identifier, AuthUser};
use crate::state::{ApiError, SharedState};

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/v1/spend-cap-rules", post(create_rule).get(list_rules))
        .route("/api/v1/spend-cap-rules/:id", axum::routing::delete(delete_rule))
}

#[derive(Deserialize)]
struct CreateRuleBody {
    org_id: String,
    agent_id: Option<String>,
    service: String,
    action: String,
    window: String,
    max_calls: Option<i32>,
    /// Payload field whose values are summed per window (e.g. "notional").
    amount_field: Option<String>,
    max_amount: Option<f64>,
    /// IANA zone where the window starts, e.g. "Asia/Kolkata". Default UTC.
    timezone: Option<String>,
    on_exceed: String,
}

async fn create_rule(State(state): State<SharedState>, user: AuthUser, Json(body): Json<CreateRuleBody>) -> Result<Json<Value>, ApiError> {
    check_dashboard_rate_limit(&state, user.sub).await?;
    if !is_valid_identifier(&body.org_id) {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "org_id must be 1-100 characters, letters/numbers/underscore/hyphen only."));
    }
    if !check_org_write_permission(&state.pg, user.sub, &body.org_id).await? {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "Auditors have read-only access to this org."));
    }
    if body.service.is_empty() || body.action.is_empty() {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "service and action are required."));
    }
    if let Some(agent_id) = &body.agent_id {
        if !is_valid_identifier(agent_id) {
            return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "agent_id must be 1-100 characters, letters/numbers/underscore/hyphen only."));
        }
    }
    if !matches!(body.window.as_str(), "hour" | "day") {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "window must be \"hour\" or \"day\"."));
    }
    if body.max_calls.is_some_and(|m| m <= 0) {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "max_calls must be a positive number."));
    }
    if body.amount_field.is_some() != body.max_amount.is_some() {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "amount_field and max_amount go together: which payload field to add up, and the most it may add up to."));
    }
    if body.max_calls.is_none() && body.max_amount.is_none() {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "Set max_calls, max_amount with amount_field, or both."));
    }
    if body.max_amount.is_some_and(|m| !(m.is_finite() && m > 0.0)) {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "max_amount must be a positive number."));
    }
    if body.amount_field.as_deref().is_some_and(|f| f.is_empty() || f.len() > 100 || !f.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')) {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "amount_field must be a payload field name (letters, numbers, underscore)."));
    }
    if body.timezone.as_deref().is_some_and(|tz| tz.parse::<chrono_tz::Tz>().is_err()) {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "timezone must be an IANA time zone such as \"America/New_York\" or \"Asia/Kolkata\"."));
    }
    match body.on_exceed.as_str() {
        "block" => {}
        "hitl" => {
            require_tier(&state, &body.org_id, agentraas_core::tier::Tier::Team).await?;
            if !cfg!(feature = "enterprise") {
                return Err(ApiError::new(
                    StatusCode::NOT_IMPLEMENTED,
                    "Routing to HITL approval needs the Enterprise-featured build (ee/) - this deployment doesn't have it. Use on_exceed=\"block\" instead.",
                ));
            }
        }
        _ => return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "on_exceed must be \"block\" or \"hitl\".")),
    }

    let id: i32 = sqlx::query_scalar(
        "INSERT INTO spend_cap_rules (org_id, agent_id, service, action, time_window, max_calls, on_exceed, created_by, amount_field, max_amount, timezone)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) RETURNING id",
    )
    .bind(&body.org_id)
    .bind(&body.agent_id)
    .bind(&body.service)
    .bind(&body.action)
    .bind(&body.window)
    .bind(body.max_calls)
    .bind(&body.on_exceed)
    .bind(user.sub)
    .bind(&body.amount_field)
    .bind(body.max_amount)
    .bind(&body.timezone)
    .fetch_one(&state.pg)
    .await?;
    Ok(Json(json!({ "id": id })))
}

async fn list_rules(State(state): State<SharedState>, user: AuthUser) -> Result<Json<Value>, ApiError> {
    check_dashboard_rate_limit(&state, user.sub).await?;
    // No tier gate here (same reasoning as hitl::list_rules): spans every
    // org the user belongs to, and a downgraded org should still be able
    // to see/clean up rules it made while on Team. Access stays scoped by
    // get_user_org_ids below.
    let org_ids = get_user_org_ids(&state.pg, user.sub).await?;
    if org_ids.is_empty() {
        return Ok(Json(json!({ "rules": [] })));
    }
    let rows: Vec<Value> = sqlx::query_scalar(
        "SELECT jsonb_build_object('id', id, 'org_id', org_id, 'agent_id', agent_id, 'service', service,
                'action', action, 'window', time_window, 'max_calls', max_calls, 'on_exceed', on_exceed,
                'amount_field', amount_field, 'max_amount', max_amount, 'timezone', timezone)
         FROM spend_cap_rules WHERE org_id = ANY($1) ORDER BY created_at DESC",
    )
    .bind(&org_ids)
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(json!({ "rules": rows })))
}

async fn delete_rule(State(state): State<SharedState>, user: AuthUser, Path(id): Path<i32>) -> Result<Json<Value>, ApiError> {
    check_dashboard_rate_limit(&state, user.sub).await?;
    let org_ids = get_user_writable_org_ids(&state.pg, user.sub).await?;
    if org_ids.is_empty() {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "Spend-cap rule not found."));
    }
    let deleted: Option<i32> = sqlx::query_scalar("DELETE FROM spend_cap_rules WHERE id = $1 AND org_id = ANY($2) RETURNING id")
        .bind(id)
        .bind(&org_ids)
        .fetch_optional(&state.pg)
        .await?;
    if deleted.is_none() {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "Spend-cap rule not found."));
    }
    Ok(Json(json!({ "deleted": true })))
}

// ─── enforcement (called from agent::mod::handle_request) ───

#[derive(sqlx::FromRow, Clone)]
pub struct MatchedSpendCapRule {
    pub id: i32,
    pub max_calls: Option<i32>,
    pub window: String,
    pub on_exceed: String,
    pub amount_field: Option<String>,
    pub max_amount: Option<f64>,
    pub timezone: Option<String>,
}

pub struct SpendCapCheck {
    pub allowed: bool,
    pub rule: Option<MatchedSpendCapRule>,
    /// Why the call was refused, for the caller.
    pub message: String,
}

/// The current window's label in the rule's zone ("2026-10-17" or
/// "2026-10-17T14"), so a day starts at local midnight.
fn window_bucket(window: &str, timezone: Option<&str>, now: chrono::DateTime<chrono::Utc>) -> String {
    let tz: chrono_tz::Tz = timezone.and_then(|t| t.parse().ok()).unwrap_or(chrono_tz::UTC);
    let local = now.with_timezone(&tz);
    if window == "hour" { local.format("%Y-%m-%dT%H").to_string() } else { local.format("%Y-%m-%d").to_string() }
}

fn window_ttl_seconds(window: &str) -> i64 {
    // A little slack past the window boundary so a key never expires
    // mid-check due to clock/Redis timing skew - matches
    // increment_monthly_usage's own "longer than the window" TTL choice.
    // A day with a daylight-saving change can be 25 hours, hence 26.
    if window == "hour" { 3600 + 300 } else { 26 * 3600 }
}

/// A payload number, or a number sent as text ("2500.50").
fn amount_of(payload: &serde_json::Value, field: &str) -> Option<f64> {
    let v = payload.get(field)?;
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())).filter(|a: &f64| a.is_finite() && *a >= 0.0)
}

fn fmt_amount(a: f64) -> String {
    if a.fract() == 0.0 { format!("{a:.0}") } else { format!("{a:.2}") }
}

/// Looks up the most specific matching rule (a per-agent row wins over an
/// org-wide `agent_id IS NULL` row for the same service.action - same
/// "most specific match" convention `resolve_route` already uses for
/// Custom Actions), then counts this call against it. A money cap needs
/// the amount: a call without a usable value in `amount_field` is refused
/// (otherwise leaving the field out would skip the cap). A refused call
/// gives back what it added, so it doesn't use up the window.
pub async fn check_and_increment(state: &SharedState, org_id: &str, agent_id: &str, service: &str, action: &str, payload: &serde_json::Value) -> Result<SpendCapCheck, ApiError> {
    let rule: Option<MatchedSpendCapRule> = sqlx::query_as(
        "SELECT id, max_calls, time_window AS window, on_exceed, amount_field, max_amount, timezone FROM spend_cap_rules
         WHERE org_id = $1 AND service = $2 AND action = $3 AND (agent_id = $4 OR agent_id IS NULL)
         ORDER BY agent_id NULLS LAST LIMIT 1",
    )
    .bind(org_id)
    .bind(service)
    .bind(action)
    .bind(agent_id)
    .fetch_optional(&state.pg)
    .await?;

    let Some(rule) = rule else {
        return Ok(SpendCapCheck { allowed: true, rule: None, message: String::new() });
    };
    let refuse = |message: String, rule: MatchedSpendCapRule| Ok(SpendCapCheck { allowed: false, rule: Some(rule), message });
    let period = if rule.window == "hour" { "hour" } else { "day" };
    let bucket = window_bucket(&rule.window, rule.timezone.as_deref(), chrono::Utc::now());
    let base = format!("spendcap:{}:{}:{}:{}:{}", org_id, agent_id, service, action, bucket);
    let mut conn = state
        .redis_conn_result()
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "Spend-cap counter unavailable."))?;
    let counter_failed = |_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "Spend-cap counter failed.");

    let mut added_amount = None;
    if let (Some(field), Some(max)) = (rule.amount_field.clone(), rule.max_amount) {
        let Some(amount) = amount_of(payload, &field) else {
            return refuse(format!("{service}.{action} needs a number in \"{field}\": spend cap #{} limits the total {field} per {period}.", rule.id), rule);
        };
        let key = format!("{base}:amount");
        let total: String = redis::cmd("INCRBYFLOAT").arg(&key).arg(amount).query_async(&mut conn).await.map_err(counter_failed)?;
        let total: f64 = total.parse().unwrap_or(f64::MAX);
        let _: Result<(), _> = redis::cmd("EXPIRE").arg(&key).arg(window_ttl_seconds(&rule.window)).query_async(&mut conn).await;
        if total > max {
            let _: Result<String, _> = redis::cmd("INCRBYFLOAT").arg(&key).arg(-amount).query_async(&mut conn).await;
            return refuse(format!("Spend cap exceeded for {service}.{action}: {field} {} would bring this {period}'s total to {}, above {} (rule #{}).", fmt_amount(amount), fmt_amount(total), fmt_amount(max), rule.id), rule);
        }
        added_amount = Some((key, amount));
    }

    if let Some(max_calls) = rule.max_calls {
        let count: i64 = redis::cmd("INCR").arg(&base).query_async(&mut conn).await.map_err(counter_failed)?;
        if count == 1 {
            let _: Result<(), _> = redis::cmd("EXPIRE").arg(&base).arg(window_ttl_seconds(&rule.window)).query_async(&mut conn).await;
        }
        if count > max_calls as i64 {
            if let Some((key, amount)) = added_amount {
                let _: Result<String, _> = redis::cmd("INCRBYFLOAT").arg(&key).arg(-amount).query_async(&mut conn).await;
            }
            return refuse(format!("Spend cap exceeded for {service}.{action}: {count}/{max_calls} calls this {period} (rule #{}).", rule.id), rule);
        }
    }
    Ok(SpendCapCheck { allowed: true, rule: Some(rule), message: String::new() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn a_day_starts_at_midnight_where_the_rule_says() {
        // 20:00 UTC on Oct 16 is already Oct 17 in India (UTC+5:30).
        let now = chrono::Utc.with_ymd_and_hms(2026, 10, 16, 20, 0, 0).unwrap();
        assert_eq!(window_bucket("day", None, now), "2026-10-16");
        assert_eq!(window_bucket("day", Some("Asia/Kolkata"), now), "2026-10-17");
        assert_eq!(window_bucket("hour", Some("America/New_York"), now), "2026-10-16T16");
    }

    #[test]
    fn amounts_come_from_numbers_or_numeric_text() {
        let p = serde_json::json!({ "notional": "2500.50", "qty": 3, "bad": "lots", "neg": -5 });
        assert_eq!(amount_of(&p, "notional"), Some(2500.5));
        assert_eq!(amount_of(&p, "qty"), Some(3.0));
        assert_eq!(amount_of(&p, "bad"), None);
        assert_eq!(amount_of(&p, "neg"), None, "a negative amount can't free up budget");
        assert_eq!(amount_of(&p, "missing"), None);
    }
}
