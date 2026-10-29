//! What each agent is allowed to do. Rules use
//! the validation-rule field syntax (`agentraas_core::validator`), and every
//! matching policy applies. Not under `ee/`, same reasoning as spend caps:
//! a safety control on the org's own agents, available on every tier. Only
//! `on_violation = "hitl"` needs Team + the enterprise build, checked at
//! creation time so enforcement never meets a rule it can't act on.

use agentraas_core::validator::{host_of, is_valid_rule_definition, validate_fields};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::agent::db::{get_user_writable_org_ids, check_org_write_permission, get_user_org_ids, require_tier};
use crate::auth::{check_dashboard_rate_limit, is_valid_action_name, is_valid_identifier, AuthUser};
use crate::state::{ApiError, SharedState};

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/v1/action-policies", post(create_policy).get(list_policies))
        .route("/api/v1/action-policies/:id", axum::routing::delete(delete_policy))
}

#[derive(Deserialize)]
struct CreatePolicyBody {
    org_id: String,
    agent_id: Option<String>,
    service: String,
    action: String,
    effect: String,
    fields: Option<Value>,
    on_violation: Option<String>,
    /// Allowed hours, e.g. {"tz": "America/New_York", "days": ["mon", "tue",
    /// "wed", "thu", "fri"], "from": "09:30", "to": "16:00"}.
    hours: Option<Value>,
}

fn unprocessable(msg: &str) -> ApiError {
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, msg)
}

async fn create_policy(State(state): State<SharedState>, user: AuthUser, Json(body): Json<CreatePolicyBody>) -> Result<Json<Value>, ApiError> {
    check_dashboard_rate_limit(&state, user.sub).await?;
    if !is_valid_identifier(&body.org_id) {
        return Err(unprocessable("org_id must be 1-100 characters, letters/numbers/underscore/hyphen only."));
    }
    if !check_org_write_permission(&state.pg, user.sub, &body.org_id).await? {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "Auditors have read-only access to this org."));
    }
    if body.agent_id.as_deref().is_some_and(|a| !is_valid_identifier(a)) {
        return Err(unprocessable("agent_id must be 1-100 characters, letters/numbers/underscore/hyphen only."));
    }
    if !is_valid_identifier(&body.service) {
        return Err(unprocessable("service must be 1-100 characters, letters/numbers/underscore/hyphen only."));
    }
    if body.action != "*" && !is_valid_action_name(&body.action) {
        return Err(unprocessable("action must be \"*\" or 1-100 characters, letters/numbers/dots/underscore/hyphen only."));
    }
    let hours = body.hours.filter(|h| !h.is_null());
    if let Some(h) = &hours {
        if body.effect != "require" {
            return Err(unprocessable("hours go on a require policy: calls are allowed only inside them."));
        }
        if let Err(err) = parse_hours(h) {
            return Err(unprocessable(&err));
        }
    }
    let fields = match (body.effect.as_str(), &body.fields) {
        ("deny", None | Some(Value::Null)) => None,
        ("deny", Some(_)) => return Err(unprocessable("A deny policy has no fields; use effect \"require\" to limit what a call may contain.")),
        ("require", Some(f)) => {
            if let Some(err) = is_valid_rule_definition(f).or_else(|| invalid_lookup(f)) {
                return Err(unprocessable(&err));
            }
            Some(f.clone())
        }
        ("require", None) if hours.is_some() => None,
        ("require", None) => return Err(unprocessable("A require policy needs fields or hours.")),
        _ => return Err(unprocessable("effect must be \"deny\" or \"require\".")),
    };
    let on_violation = body.on_violation.unwrap_or_else(|| "block".to_string());
    if fields.as_ref().is_some_and(|f| !destination_fields(f).is_empty()) && on_violation != "hitl" {
        return Err(unprocessable("new_destination needs on_violation \"hitl\": approving a new destination is what adds it to the known list."));
    }
    match on_violation.as_str() {
        "block" => {}
        "hitl" => {
            require_tier(&state, &body.org_id, agentraas_core::tier::Tier::Team).await?;
            if !cfg!(feature = "enterprise") {
                return Err(ApiError::new(
                    StatusCode::NOT_IMPLEMENTED,
                    "Routing to approval needs the Enterprise-featured build (ee/); this deployment doesn't have it. Use on_violation=\"block\" instead.",
                ));
            }
        }
        _ => return Err(unprocessable("on_violation must be \"block\" or \"hitl\".")),
    }

    let id: i32 = sqlx::query_scalar(
        "INSERT INTO action_policies (org_id, agent_id, service, action, effect, fields, on_violation, created_by, hours)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
    )
    .bind(&body.org_id)
    .bind(&body.agent_id)
    .bind(&body.service)
    .bind(&body.action)
    .bind(&body.effect)
    .bind(&fields)
    .bind(&on_violation)
    .bind(user.sub)
    .bind(&hours)
    .fetch_one(&state.pg)
    .await?;
    Ok(Json(json!({ "id": id })))
}

async fn list_policies(State(state): State<SharedState>, user: AuthUser) -> Result<Json<Value>, ApiError> {
    check_dashboard_rate_limit(&state, user.sub).await?;
    let org_ids = get_user_org_ids(&state.pg, user.sub).await?;
    if org_ids.is_empty() {
        return Ok(Json(json!({ "policies": [] })));
    }
    let rows: Vec<Value> = sqlx::query_scalar(
        "SELECT jsonb_build_object('id', id, 'org_id', org_id, 'agent_id', agent_id, 'service', service,
                'action', action, 'effect', effect, 'fields', fields, 'on_violation', on_violation, 'hours', hours)
         FROM action_policies WHERE org_id = ANY($1) ORDER BY created_at DESC",
    )
    .bind(&org_ids)
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(json!({ "policies": rows })))
}

async fn delete_policy(State(state): State<SharedState>, user: AuthUser, Path(id): Path<i32>) -> Result<Json<Value>, ApiError> {
    check_dashboard_rate_limit(&state, user.sub).await?;
    let org_ids = get_user_writable_org_ids(&state.pg, user.sub).await?;
    let deleted: Option<i32> = sqlx::query_scalar("DELETE FROM action_policies WHERE id = $1 AND org_id = ANY($2) RETURNING id")
        .bind(id)
        .bind(&org_ids)
        .fetch_optional(&state.pg)
        .await?;
    if deleted.is_none() {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "Action policy not found."));
    }
    Ok(Json(json!({ "deleted": true })))
}

// ─── allowed hours ───

const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

/// (timezone, allowed weekdays as 0=Mon..6=Sun, from, to). `days` defaults
/// to every day; `from` must be before `to` (no overnight windows).
fn parse_hours(h: &Value) -> Result<(chrono_tz::Tz, Vec<usize>, chrono::NaiveTime, chrono::NaiveTime), String> {
    let tz: chrono_tz::Tz = h.get("tz").and_then(Value::as_str).ok_or("hours needs \"tz\", an IANA time zone such as \"America/New_York\" or \"Asia/Kolkata\".")?
        .parse().map_err(|_| "hours.tz is not a known time zone (use an IANA name such as \"America/New_York\" or \"Asia/Kolkata\").".to_string())?;
    let time = |k: &str| {
        h.get(k).and_then(Value::as_str).and_then(|t| chrono::NaiveTime::parse_from_str(t, "%H:%M").ok())
            .ok_or(format!("hours.{k} must be a time like \"09:30\" (24-hour)."))
    };
    let (from, to) = (time("from")?, time("to")?);
    if from >= to {
        return Err("hours.from must be before hours.to.".into());
    }
    let days = match h.get("days") {
        None | Some(Value::Null) => (0..7).collect(),
        Some(Value::Array(a)) if !a.is_empty() => a.iter().map(|d| d.as_str().and_then(|d| DAYS.iter().position(|x| *x == d.to_ascii_lowercase()))
            .ok_or("hours.days takes mon, tue, wed, thu, fri, sat, sun.".to_string())).collect::<Result<_, _>>()?,
        _ => return Err("hours.days must be a list such as [\"mon\", \"tue\", \"wed\", \"thu\", \"fri\"].".into()),
    };
    Ok((tz, days, from, to))
}

/// A malformed stored value fails closed: no hours are "inside".
fn within_hours(h: &Value, now: chrono::DateTime<chrono::Utc>) -> bool {
    use chrono::{Datelike, Timelike};
    let Ok((tz, days, from, to)) = parse_hours(h) else { return false };
    let local = now.with_timezone(&tz);
    let t = chrono::NaiveTime::from_hms_opt(local.hour(), local.minute(), local.second()).unwrap_or_default();
    days.contains(&(local.weekday().num_days_from_monday() as usize)) && t >= from && t < to
}

fn describe_hours(h: &Value) -> String {
    let Ok((tz, days, from, to)) = parse_hours(h) else { return "inside its configured hours".into() };
    let days: Vec<&str> = days.iter().map(|d| DAYS[*d]).collect();
    format!("on {} from {} to {} ({tz})", days.join(", "), from.format("%H:%M"), to.format("%H:%M"))
}

// ─── enforcement (called from agent::mod::handle_request) ───

#[derive(sqlx::FromRow)]
pub struct Policy {
    pub id: i32,
    pub effect: String,
    pub fields: Option<Value>,
    pub on_violation: String,
    pub hours: Option<Value>,
}

pub struct Violation {
    /// "policy_denied", "policy_violation", "policy_new_destination",
    /// "policy_outside_hours" or "policy_lookup_limit", the audit log's reason.
    pub reason: &'static str,
    pub hitl: bool,
    pub message: String,
}

/// Fields a policy marks `new_destination: true`.
fn destination_fields(fields: &Value) -> Vec<&str> {
    fields
        .as_object()
        .map(|o| o.iter().filter(|(_, r)| r.get("new_destination").and_then(Value::as_bool) == Some(true)).map(|(k, _)| k.as_str()).collect())
        .unwrap_or_default()
}

/// What counts as "the same destination": a URL's host, otherwise the
/// whole value (a full email address, phone number or account id), lowercased.
fn normalize_destination(s: &str) -> String {
    if s.contains("://") { host_of(s) } else { s.trim().to_ascii_lowercase() }
}

/// (field, normalized value) for every new_destination field one policy covers.
fn policy_destinations(p: &Policy, payload: &Value) -> Vec<(String, String)> {
    let Some(fields) = p.fields.as_ref().filter(|_| p.effect == "require") else { return vec![] };
    let mut out = vec![];
    for field in destination_fields(fields) {
        let values: Vec<&str> = match payload.get(field) {
            Some(Value::String(s)) => vec![s.as_str()],
            Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
            _ => vec![],
        };
        out.extend(values.into_iter().map(|v| (field.to_string(), normalize_destination(v))));
    }
    out
}

/// First violation that should win: any block beats any approval, so an
/// approver can never wave through a call another policy forbids.
/// `unknown` = destinations (field, value) this org has never approved.
pub fn evaluate(policies: &[Policy], service: &str, action: &str, payload: &Value, unknown: &[(String, String)], now: chrono::DateTime<chrono::Utc>) -> Option<Violation> {
    let mut hitl = None;
    for p in policies {
        let (reason, message) = if p.effect == "deny" {
            ("policy_denied", format!("This agent may not call {service}.{action} (action policy #{})", p.id))
        } else if let Some(h) = p.hours.as_ref().filter(|h| !within_hours(h, now)) {
            ("policy_outside_hours", format!("{service}.{action} is only allowed {} (action policy #{})", describe_hours(h), p.id))
        } else if let Some(err) = p.fields.as_ref().and_then(|f| validate_fields(payload, f)) {
            ("policy_violation", format!("{err} (action policy #{})", p.id))
        } else if let Some((field, value)) = policy_destinations(p, payload).into_iter().find(|d| unknown.contains(d)) {
            ("policy_new_destination", format!("{field} ({value}) is a new destination for {service}; it needs approval once (action policy #{})", p.id))
        } else {
            continue;
        };
        let v = Violation { reason, hitl: p.on_violation == "hitl", message };
        if !v.hitl {
            return Some(v);
        }
        hitl.get_or_insert(v);
    }
    hitl
}

async fn matching_policies(state: &SharedState, org_id: &str, agent_id: Option<&str>, service: &str, action: &str) -> Result<Vec<Policy>, sqlx::Error> {
    // agent_id None = every agent's policies (used when remembering).
    sqlx::query_as(
        "SELECT id, effect, fields, on_violation, hours FROM action_policies
         WHERE org_id = $1 AND service = $2 AND (action = $3 OR action = '*')
           AND ($4::text IS NULL OR agent_id = $4 OR agent_id IS NULL)
         ORDER BY id",
    )
    .bind(org_id)
    .bind(service)
    .bind(action)
    .bind(agent_id)
    .fetch_all(&state.pg)
    .await
}

/// `max_from`: a limit read from the provider before the call runs, so the
/// agent can't supply the number it is checked against. "A refund at most
/// half the order" on Cashfree:
/// `{"refund_amount": {"max_from": {"action": "order.get", "with": {"order_id": "order_id"},
///   "field": "order_amount", "factor": 0.5}}}`. `service` (default: the
/// call's own) can name another service, such as a custom action reading the
/// platform's own order API; `minus` subtracts a second field (Razorpay's
/// `amount_refunded`). A lookup that fails or an amount it can't read blocks
/// the call: an unchecked refund is not a pass.
fn lookups(fields: &Value) -> Vec<(&str, &Value)> {
    fields.as_object().map(|o| o.iter().filter_map(|(k, r)| r.get("max_from").map(|m| (k.as_str(), m))).collect()).unwrap_or_default()
}

fn invalid_lookup(fields: &Value) -> Option<String> {
    for (field, m) in lookups(fields) {
        let ok = m.get("action").and_then(Value::as_str).is_some_and(|a| is_valid_action_name(a))
            && m.get("field").and_then(Value::as_str).is_some_and(|f| !f.is_empty() && f.len() <= 200)
            && m.get("with").and_then(Value::as_object).is_some_and(|w| w.values().all(Value::is_string))
            && m.get("service").map_or(true, |v| v.as_str().is_some_and(is_valid_identifier))
            && m.get("minus").map_or(true, |v| v.as_str().is_some_and(|f| !f.is_empty()))
            && m.get("factor").map_or(true, |v| v.as_f64().is_some_and(|f| f > 0.0));
        if !ok {
            return Some(format!("Field \"{field}\": max_from needs action, field and with (an object of field names); service and minus are names, factor a number above 0."));
        }
    }
    None
}

/// A number at a dotted path ("order_amount", "data.total"); numeric strings count.
fn number_at(v: &Value, path: &str) -> Option<f64> {
    let found = path.split('.').try_fold(v, |cur, key| cur.get(key))?;
    found.as_f64().or_else(|| found.as_str().and_then(|s| s.trim().parse().ok())).filter(|n: &f64| n.is_finite())
}

async fn check_lookups(state: &SharedState, org_id: &str, service: &str, payload: &Value, p: &Policy) -> Option<Violation> {
    let fields = p.fields.as_ref().filter(|_| p.effect == "require")?;
    for (field, m) in lookups(fields) {
        let lookup_service = m.get("service").and_then(Value::as_str).unwrap_or(service);
        let lookup_action = m.get("action").and_then(Value::as_str).unwrap_or_default();
        let source = m.get("field").and_then(Value::as_str).unwrap_or_default();
        let factor = m.get("factor").and_then(Value::as_f64).unwrap_or(1.0);
        let fail = |message: String| Some(Violation { reason: "policy_lookup_limit", hitl: p.on_violation == "hitl", message: format!("{message} (action policy #{})", p.id) });

        let Some(asked) = number_at(payload, field) else {
            return fail(format!("{field} is missing, so it can't be checked against {lookup_service}.{lookup_action}; send it explicitly"));
        };
        let mut query = serde_json::Map::new();
        for (to, from) in m.get("with").and_then(Value::as_object).into_iter().flatten() {
            let from = from.as_str().unwrap_or_default();
            match payload.get(from) {
                Some(v) if !v.is_null() => { query.insert(to.clone(), v.clone()); }
                _ => return fail(format!("{from} is missing, so {field} can't be checked against {lookup_service}.{lookup_action}")),
            }
        }
        let route = match crate::agent::db::resolve_route(state, lookup_service, lookup_action, org_id).await {
            Ok(Some(r)) => r,
            _ => return fail(format!("{lookup_service}.{lookup_action} is not an action AgentRaaS knows, so {field} can't be checked")),
        };
        let answer = match crate::agent::forward::forward_action(state, &route, lookup_service, lookup_action, org_id, &Value::Object(query), "policy-lookup", None, None).await {
            Ok(a) => a,
            Err(e) => return fail(format!("Couldn't check {field} against {lookup_service}.{lookup_action}: {}. Nothing was sent", e.message)),
        };
        let body = answer.get("upstream_response").unwrap_or(&answer);
        let Some(total) = number_at(body, source) else {
            return fail(format!("{lookup_service}.{lookup_action} answered without a number at {source}, so {field} can't be checked"));
        };
        let minus = m.get("minus").and_then(Value::as_str);
        let base = total - minus.and_then(|f| number_at(body, f)).unwrap_or(0.0);
        let limit = base * factor;
        if asked > limit + 1e-9 {
            let of = match (factor != 1.0, minus) {
                (true, Some(mf)) => format!("{}% of {source} minus {mf}", fmt(factor * 100.0)),
                (true, None) => format!("{}% of {source}", fmt(factor * 100.0)),
                (false, Some(mf)) => format!("{source} minus {mf}"),
                (false, None) => source.to_string(),
            };
            return fail(format!("{field} {} is more than the {} allowed ({of}, read from {lookup_service}.{lookup_action})", fmt(asked), fmt(limit)));
        }
    }
    None
}

fn fmt(n: f64) -> String {
    if n.fract() == 0.0 { format!("{n:.0}") } else { format!("{n:.2}") }
}

pub async fn check(state: &SharedState, org_id: &str, agent_id: &str, service: &str, action: &str, payload: &Value) -> Result<Option<Violation>, sqlx::Error> {
    check_inner(state, org_id, agent_id, service, action, payload, true).await.map(|(v, _)| v)
}

/// `check` without `max_from` lookups, which would call the provider with
/// the org's credentials: shadow mode (`crate::shadow`) never does. The bool
/// is true when a lookup was left out, so the answer may be too lenient.
pub async fn check_offline(state: &SharedState, org_id: &str, agent_id: &str, service: &str, action: &str, payload: &Value) -> Result<(Option<Violation>, bool), sqlx::Error> {
    check_inner(state, org_id, agent_id, service, action, payload, false).await
}

async fn check_inner(state: &SharedState, org_id: &str, agent_id: &str, service: &str, action: &str, payload: &Value, run_lookups: bool) -> Result<(Option<Violation>, bool), sqlx::Error> {
    let policies = matching_policies(state, org_id, Some(agent_id), service, action).await?;
    let wanted: Vec<(String, String)> = policies.iter().flat_map(|p| policy_destinations(p, payload)).collect();
    let mut unknown = vec![];
    if !wanted.is_empty() {
        let (fields, values): (Vec<String>, Vec<String>) = wanted.iter().cloned().unzip();
        let known: Vec<(String, String)> = sqlx::query_as(
            "SELECT field, value FROM known_destinations WHERE org_id = $1 AND service = $2 AND field = ANY($3) AND value = ANY($4)",
        )
        .bind(org_id)
        .bind(service)
        .bind(&fields)
        .bind(&values)
        .fetch_all(&state.pg)
        .await?;
        unknown = wanted.into_iter().filter(|d| !known.contains(d)).collect();
    }
    let found = evaluate(&policies, service, action, payload, &unknown, chrono::Utc::now());
    // A block already decides; lookups (a network call each) only run when it could still change the answer.
    if found.as_ref().is_some_and(|v| !v.hitl) {
        return Ok((found, false));
    }
    let mut hitl = found;
    let mut skipped = false;
    for p in &policies {
        if !run_lookups {
            skipped |= p.fields.as_ref().is_some_and(|f| p.effect == "require" && !lookups(f).is_empty());
            continue;
        }
        if let Some(v) = check_lookups(state, org_id, service, payload, p).await {
            if !v.hitl {
                return Ok((Some(v), false));
            }
            hitl.get_or_insert(v);
        }
    }
    Ok((hitl, skipped))
}

/// Called once a human approved a call: its destinations become known, so
/// the next call to the same place goes straight through.
pub async fn remember_destinations(state: &SharedState, org_id: &str, service: &str, action: &str, payload: &Value) {
    let policies = match matching_policies(state, org_id, None, service, action).await {
        Ok(p) => p,
        Err(err) => return tracing::error!(?err, "remember_destinations: policy lookup failed"),
    };
    let (fields, values): (Vec<String>, Vec<String>) = policies.iter().flat_map(|p| policy_destinations(p, payload)).unzip();
    if fields.is_empty() {
        return;
    }
    let res = sqlx::query(
        "INSERT INTO known_destinations (org_id, service, field, value)
         SELECT $1, $2, f, v FROM unnest($3::text[], $4::text[]) AS t(f, v) ON CONFLICT DO NOTHING",
    )
    .bind(org_id)
    .bind(service)
    .bind(&fields)
    .bind(&values)
    .execute(&state.pg)
    .await;
    if let Err(err) = res {
        tracing::error!(?err, "remember_destinations insert failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(id: i32, effect: &str, fields: Option<Value>, on_violation: &str) -> Policy {
        Policy { id, effect: effect.into(), fields, on_violation: on_violation.into(), hours: None }
    }

    #[test]
    fn max_from_reads_numbers_by_path_and_checks_its_spec() {
        let body = json!({"order_amount": 1200, "data": {"total": "99.5"}, "note": "x"});
        assert_eq!(number_at(&body, "order_amount"), Some(1200.0));
        assert_eq!(number_at(&body, "data.total"), Some(99.5), "numeric strings count");
        assert_eq!(number_at(&body, "note"), None);
        assert_eq!(number_at(&body, "missing.path"), None);

        let ok = json!({"refund_amount": {"max_from": {"action": "order.get", "with": {"order_id": "order_id"}, "field": "order_amount", "factor": 0.5}}});
        assert_eq!(invalid_lookup(&ok), None);
        assert!(lookups(&json!({"amount": {"max": 5}})).is_empty(), "a plain rule is not a lookup");
        for bad in [
            json!({"amount": {"max_from": {"with": {"id": "id"}, "field": "amount"}}}),
            json!({"amount": {"max_from": {"action": "payment.get", "with": {"id": 5}, "field": "amount"}}}),
            json!({"amount": {"max_from": {"action": "payment.get", "with": {}, "field": "amount", "factor": 0}}}),
        ] {
            assert!(invalid_lookup(&bad).is_some(), "{bad}");
        }
    }

    #[test]
    fn no_policies_or_passing_payload_is_allowed() {
        assert!(evaluate(&[], "stripe", "refunds.create", &json!({}), &[], chrono::Utc::now()).is_none());
        let ps = [p(1, "require", Some(json!({ "amount": { "max": 50 } })), "block")];
        assert!(evaluate(&ps, "stripe", "refunds.create", &json!({ "amount": 20 }), &[], chrono::Utc::now()).is_none());
    }

    #[test]
    fn require_violation_and_deny_block() {
        let ps = [p(1, "require", Some(json!({ "amount": { "max": 50 } })), "block")];
        let v = evaluate(&ps, "stripe", "refunds.create", &json!({ "amount": 80 }), &[], chrono::Utc::now()).unwrap();
        assert_eq!((v.reason, v.hitl), ("policy_violation", false));
        assert!(v.message.ends_with("(action policy #1)"));
        assert!(v.message.contains("amount must be at most 50"));

        let v = evaluate(&[p(2, "deny", None, "block")], "hubspot", "contacts.delete", &json!({}), &[], chrono::Utc::now()).unwrap();
        assert_eq!(v.reason, "policy_denied");
    }

    #[test]
    fn a_block_beats_an_approval_whatever_the_order() {
        let ps = [p(1, "require", Some(json!({ "amount": { "max": 50 } })), "hitl"), p(2, "deny", None, "block")];
        let v = evaluate(&ps, "stripe", "refunds.create", &json!({ "amount": 80 }), &[], chrono::Utc::now()).unwrap();
        assert!(!v.hitl && v.message.contains("#2"));

        let v = evaluate(&ps[..1], "stripe", "refunds.create", &json!({ "amount": 80 }), &[], chrono::Utc::now()).unwrap();
        assert!(v.hitl);
    }

    #[test]
    fn new_destination_flags_only_unknown_values() {
        let ps = [p(1, "require", Some(json!({ "to": { "new_destination": true } })), "hitl")];
        let payload = json!({ "to": ["Boss@Acme.com", "x@evil.io"] });
        let wanted = policy_destinations(&ps[0], &payload);
        assert_eq!(wanted, vec![("to".to_string(), "boss@acme.com".to_string()), ("to".to_string(), "x@evil.io".to_string())]);

        assert!(evaluate(&ps, "resend", "emails.send", &payload, &[], chrono::Utc::now()).is_none(), "all known");
        let v = evaluate(&ps, "resend", "emails.send", &payload, &wanted[1..], chrono::Utc::now()).unwrap();
        assert_eq!((v.reason, v.hitl), ("policy_new_destination", true));
        assert!(v.message.contains("x@evil.io"));
        assert_eq!(normalize_destination("https://Hooks.Example.com/a?b"), "hooks.example.com");
    }

    #[test]
    fn hours_follow_the_market_clock_including_daylight_saving() {
        use chrono::TimeZone;
        let nyse = json!({ "tz": "America/New_York", "days": ["mon", "tue", "wed", "thu", "fri"], "from": "09:30", "to": "16:00" });
        let at = |y, m, d, h, min| chrono::Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap();
        // Winter (EST, UTC-5): 14:29 UTC is 09:29, closed; 14:30 is the open.
        assert!(!within_hours(&nyse, at(2026, 1, 13, 14, 29)));
        assert!(within_hours(&nyse, at(2026, 1, 13, 14, 30)));
        // Summer (EDT, UTC-4): the open moves to 13:30 UTC.
        assert!(within_hours(&nyse, at(2026, 7, 14, 13, 30)));
        assert!(!within_hours(&nyse, at(2026, 7, 14, 20, 0)), "16:00 New York is the close");
        assert!(!within_hours(&nyse, at(2026, 7, 18, 15, 0)), "Saturday");
        // NSE: 09:15 to 15:30 in Asia/Kolkata (UTC+5:30) is 03:45 to 10:00 UTC.
        let nse = json!({ "tz": "Asia/Kolkata", "days": ["mon", "tue", "wed", "thu", "fri"], "from": "09:15", "to": "15:30" });
        assert!(within_hours(&nse, at(2026, 10, 19, 3, 45)));
        assert!(!within_hours(&nse, at(2026, 10, 19, 10, 0)));

        let mut ps = [p(1, "require", None, "block")];
        ps[0].hours = Some(nyse);
        let v = evaluate(&ps, "alpaca", "order.create", &json!({}), &[], at(2026, 7, 18, 15, 0)).unwrap();
        assert_eq!(v.reason, "policy_outside_hours");
        assert!(v.message.contains("from 09:30 to 16:00 (America/New_York)"), "{}", v.message);
        assert!(evaluate(&ps, "alpaca", "order.create", &json!({}), &[], at(2026, 7, 14, 15, 0)).is_none());
    }

    #[test]
    fn bad_hours_are_refused_and_fail_closed() {
        for bad in [json!({ "from": "09:00", "to": "17:00" }), json!({ "tz": "Mars/Base", "from": "09:00", "to": "17:00" }),
                    json!({ "tz": "UTC", "from": "17:00", "to": "09:00" }), json!({ "tz": "UTC", "from": "9am", "to": "17:00" }),
                    json!({ "tz": "UTC", "days": ["funday"], "from": "09:00", "to": "17:00" })] {
            assert!(parse_hours(&bad).is_err(), "{bad}");
            assert!(!within_hours(&bad, chrono::Utc::now()));
        }
    }
}
