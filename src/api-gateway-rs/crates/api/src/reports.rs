//! Savings and reliability reports: what AgentRaaS caught for one org over
//! a period, for people who never open the dashboard (an agency's client).
//! JSON for the dashboard, CSV and a self-contained HTML page for export,
//! and an opt-in monthly email of last month's page.
//!
//! Everything comes from tables the gateway already writes: `audit_log`
//! (plus the amount of a caught duplicate payment, `money_of`, stored on its
//! row since migration 061), `hitl_requests`, `dead_letter_queue`,
//! `circuit_breaker_events` and `schema_drift_events`. Only ids, names,
//! counts and amounts leave the server: no payloads, error texts or end-user
//! ids, so there is nothing for DLP redaction to miss.
//!
//! White-label (Enterprise): the org's `org_branding` name, logo and accent
//! colour replace AgentRaaS's on the HTML page and the email.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use chrono::{Datelike, NaiveDate, NaiveDateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::agent::db::{check_org_write_permission, get_user_org_ids};
use crate::auth::{check_dashboard_rate_limit, is_valid_identifier, AuthUser};
use crate::email::escape_html;
use crate::state::{ApiError, SharedState};

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/v1/reports", get(report))
        .route("/api/v1/reports/settings", get(get_settings).put(put_settings))
}

// ─── money of a payment call ───

/// Where each payment service's request body keeps its amount and currency,
/// and whether the amount is in minor units (cents, paise).
const PAYMENT_FIELDS: &[(&str, &str, &str, bool)] = &[
    ("stripe", "amount", "currency", true),
    ("razorpay", "amount", "currency", true),
    ("razorpayx", "amount", "currency", true),
    ("adyen", "amount.value", "amount.currency", true),
    ("mollie", "amount.value", "amount.currency", false),
    ("airwallex", "amount", "currency", false),
    ("xendit", "amount", "currency", false),
    ("paypal", "purchase_units.0.amount.value", "purchase_units.0.amount.currency_code", false),
    ("paypal_sandbox", "purchase_units.0.amount.value", "purchase_units.0.amount.currency_code", false),
    ("klarna", "order_amount", "purchase_currency", true),
    ("paystack", "amount", "currency", true),
    ("gocardless", "payments.amount", "payments.currency", true),
    ("opn_payments", "amount", "currency", true),
    ("cashfree", "order_amount", "order_currency", false),
    ("cashfree_sandbox", "order_amount", "order_currency", false),
    ("mockpay", "amount", "currency", false),
];

/// ISO 4217 currencies without two decimals; everything else has two.
// ponytail: fixed list (Stripe's), add a currency when a provider sends one not here.
const ZERO_DECIMAL: &[&str] = &["BIF", "CLP", "DJF", "GNF", "ISK", "JPY", "KMF", "KRW", "MGA", "PYG", "RWF", "UGX", "VND", "VUV", "XAF", "XOF", "XPF"];
const THREE_DECIMAL: &[&str] = &["BHD", "IQD", "JOD", "KWD", "LYD", "OMR", "TND"];

fn at<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(v, |cur, key| cur.get(key).or_else(|| key.parse::<usize>().ok().and_then(|i| cur.get(i))))
}

/// The amount (major units) and currency of a payment call, when its body
/// names both. Never converts between currencies and never guesses one.
pub fn money_of(service: &str, payload: &Value) -> Option<(f64, String)> {
    let &(_, amount_path, currency_path, minor) = PAYMENT_FIELDS.iter().find(|f| f.0 == service)?;
    let currency = at(payload, currency_path)?.as_str()?.trim().to_ascii_uppercase();
    if currency.len() != 3 || !currency.bytes().all(|b| b.is_ascii_uppercase()) {
        return None;
    }
    let raw = at(payload, amount_path)?;
    let amount = raw.as_f64().or_else(|| raw.as_str().and_then(|s| s.trim().parse().ok())).filter(|a: &f64| a.is_finite() && *a > 0.0)?;
    let exponent = if ZERO_DECIMAL.contains(&currency.as_str()) { 0 } else if THREE_DECIMAL.contains(&currency.as_str()) { 3 } else { 2 };
    Some((if minor { amount / 10f64.powi(exponent) } else { amount }, currency))
}

pub fn is_hex_color(s: &str) -> bool {
    s.len() == 7 && s.starts_with('#') && s[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

// ─── the report ───

/// Rows that are a caught duplicate: answered from cache, or refused while
/// the first copy was running or had timed out.
const DUPLICATE: &str = "(status = 'deduplicated' OR (status = 'blocked' AND error_type IN ('duplicate_in_progress', 'outcome_unknown', 'duplicate_of_streamed_response')))";

struct Period {
    label: String,
    from: NaiveDateTime,
    to: NaiveDateTime,
}

fn period(kind: &str, month: Option<&str>, now: NaiveDateTime) -> Result<Period, String> {
    let days = |n: i64| Period { label: format!("Last {n} days"), from: now - chrono::Duration::days(n), to: now };
    match kind {
        "7d" => Ok(days(7)),
        "30d" => Ok(days(30)),
        "month" => {
            let first = match month {
                Some(m) => NaiveDate::parse_from_str(&format!("{m}-01"), "%Y-%m-%d").map_err(|_| "month must look like 2026-10.".to_string())?,
                None => now.date().with_day(1).expect("day 1 exists"),
            };
            let next = first.checked_add_months(chrono::Months::new(1)).ok_or("month out of range")?;
            Ok(Period { label: first.format("%B %Y").to_string(), from: first.and_hms_opt(0, 0, 0).unwrap(), to: next.and_hms_opt(0, 0, 0).unwrap().min(now) })
        }
        _ => Err("period must be 7d, 30d or month.".into()),
    }
}

fn rate(part: i64, whole: i64) -> Option<f64> {
    (whole > 0).then(|| (part as f64 / whole as f64 * 10000.0).round() / 100.0)
}

async fn build(state: &SharedState, org_id: &str, p: &Period) -> Result<Value, sqlx::Error> {
    #[derive(sqlx::FromRow)]
    struct AgentRow {
        agent_id: Option<String>,
        total: i64,
        duplicates: i64,
        blocked: i64,
        by_policy: i64,
        by_spend_cap: i64,
        held: i64,
        success: i64,
        errors: i64,
        shadow: i64,
    }
    let agents: Vec<AgentRow> = sqlx::query_as(&format!(
        "SELECT agent_id,
                COUNT(DISTINCT req_id) FILTER (WHERE status <> 'shadow') AS total,
                COUNT(*) FILTER (WHERE {DUPLICATE}) AS duplicates,
                COUNT(*) FILTER (WHERE status = 'blocked' AND NOT {DUPLICATE} AND error_type NOT LIKE 'hitl%') AS blocked,
                COUNT(*) FILTER (WHERE status = 'blocked' AND error_type LIKE 'policy%') AS by_policy,
                COUNT(*) FILTER (WHERE status = 'blocked' AND error_type = 'spend_cap_exceeded') AS by_spend_cap,
                COUNT(*) FILTER (WHERE status = 'pending_approval') AS held,
                COUNT(*) FILTER (WHERE status = 'success') AS success,
                COUNT(*) FILTER (WHERE status = 'error') AS errors,
                COUNT(*) FILTER (WHERE status = 'shadow') AS shadow
         FROM audit_log WHERE org_id = $1 AND created_at >= $2 AND created_at < $3
         GROUP BY agent_id ORDER BY total DESC"
    ))
    .bind(org_id)
    .bind(p.from)
    .bind(p.to)
    .fetch_all(&state.pg)
    .await?;

    #[derive(sqlx::FromRow)]
    struct ServiceRow {
        service: String,
        total: i64,
        success: i64,
        errors: i64,
    }
    let services: Vec<ServiceRow> = sqlx::query_as(
        "SELECT service, COUNT(DISTINCT req_id) FILTER (WHERE status <> 'shadow') AS total,
                COUNT(*) FILTER (WHERE status = 'success') AS success, COUNT(*) FILTER (WHERE status = 'error') AS errors
         FROM audit_log WHERE org_id = $1 AND created_at >= $2 AND created_at < $3 GROUP BY service ORDER BY total DESC",
    )
    .bind(org_id)
    .bind(p.from)
    .bind(p.to)
    .fetch_all(&state.pg)
    .await?;
    let names: Vec<String> = services.iter().map(|s| s.service.clone()).collect();
    let (from_utc, to_utc) = (p.from.and_utc(), p.to.and_utc());
    let counts = |rows: Vec<(String, i64)>| rows.into_iter().collect::<BTreeMap<String, i64>>();
    let timeouts = counts(
        sqlx::query_as("SELECT service, COUNT(*) FROM dead_letter_queue WHERE org_id = $1 AND created_at >= $2 AND created_at < $3 AND starts_with(error_message, 'outcome unknown:') GROUP BY service")
            .bind(org_id).bind(from_utc).bind(to_utc).fetch_all(&state.pg).await?,
    );
    // Breakers are per service for the whole server, not per org.
    let trips = counts(
        sqlx::query_as("SELECT service, COUNT(*) FROM circuit_breaker_events WHERE service = ANY($1) AND to_state = 'open' AND occurred_at >= $2 AND occurred_at < $3 GROUP BY service")
            .bind(&names).bind(from_utc).bind(to_utc).fetch_all(&state.pg).await?,
    );
    let drift = counts(
        sqlx::query_as("SELECT service, COUNT(*) FROM schema_drift_events WHERE org_id = $1 AND detected_at >= $2 AND detected_at < $3 GROUP BY service")
            .bind(org_id).bind(from_utc).bind(to_utc).fetch_all(&state.pg).await?,
    );

    let top: Vec<(String, String, i64)> = sqlx::query_as(&format!(
        "SELECT service, action, COUNT(*) AS n FROM audit_log WHERE org_id = $1 AND created_at >= $2 AND created_at < $3 AND {DUPLICATE}
         GROUP BY service, action ORDER BY n DESC, service, action LIMIT 5"
    ))
    .bind(org_id)
    .bind(p.from)
    .bind(p.to)
    .fetch_all(&state.pg)
    .await?;
    // Once per payment (dedup hash), however often it was retried: an agent
    // stuck retrying one timed-out payment must not multiply the estimate.
    let money: Vec<(String, f64, i64)> = sqlx::query_as(&format!(
        "SELECT money_currency, SUM(money_amount)::float8, COUNT(*) FROM (
           SELECT DISTINCT ON (payload_hash) money_currency, money_amount FROM audit_log
           WHERE org_id = $1 AND created_at >= $2 AND created_at < $3 AND money_amount IS NOT NULL AND {DUPLICATE}
           ORDER BY payload_hash, created_at
         ) once GROUP BY money_currency ORDER BY money_currency"
    ))
    .bind(org_id)
    .bind(p.from)
    .bind(p.to)
    .fetch_all(&state.pg)
    .await?;
    let approvals = counts(
        sqlx::query_as("SELECT status, COUNT(*) FROM hitl_requests WHERE org_id = $1 AND created_at >= $2 AND created_at < $3 GROUP BY status")
            .bind(org_id).bind(from_utc).bind(to_utc).fetch_all(&state.pg).await?,
    );
    let approval = |k: &str| approvals.get(k).copied().unwrap_or(0);
    let decided = approval("approved") + approval("denied") + approval("failed");

    let sum = |f: fn(&AgentRow) -> i64| agents.iter().map(f).sum::<i64>();
    Ok(json!({
        "org_id": org_id,
        "period": { "label": p.label, "from": p.from.and_utc(), "to": p.to.and_utc() },
        "totals": {
            "actions": sum(|a| a.total),
            "duplicates_caught": sum(|a| a.duplicates),
            "blocked": sum(|a| a.blocked),
            "blocked_by_policy": sum(|a| a.by_policy),
            "blocked_by_spend_cap": sum(|a| a.by_spend_cap),
            "held_for_approval": sum(|a| a.held),
            "shadow_calls": sum(|a| a.shadow),
        },
        "approvals": {
            "approved": approval("approved"),
            "denied": approval("denied"),
            "failed_after_approval": approval("failed"),
            "still_pending": approvals.values().sum::<i64>() - decided,
        },
        "money_protected": {
            "estimate": true,
            "note": "Sum of the amounts of payments AgentRaaS stopped a duplicate of, each payment counted once however often it was retried, per currency, never converted. Recorded from the release that added this report; a duplicate whose request had no amount and currency is not counted.",
            "by_currency": money.iter().map(|(c, a, n)| json!({ "currency": c, "amount": (a * 100.0).round() / 100.0, "payments": n })).collect::<Vec<_>>(),
        },
        "agents": agents.iter().map(|a| json!({
            "agent_id": a.agent_id,
            "actions": a.total,
            "duplicate_rate": rate(a.duplicates, a.total),
            "block_rate": rate(a.blocked, a.total),
            "error_rate": rate(a.errors, a.success + a.errors),
            "shadow_calls": a.shadow,
        })).collect::<Vec<_>>(),
        "services": services.iter().map(|s| json!({
            "service": s.service,
            "actions": s.total,
            "error_rate": rate(s.errors, s.success + s.errors),
            "timeouts": timeouts.get(&s.service).copied().unwrap_or(0),
            "circuit_breaker_trips": trips.get(&s.service).copied().unwrap_or(0),
            "schema_drift_alerts": drift.get(&s.service).copied().unwrap_or(0),
        })).collect::<Vec<_>>(),
        "circuit_breaker_note": "Circuit breaker trips are per service for the whole server, not only this org's calls.",
        "top_duplicated": top.iter().map(|(s, a, n)| json!({ "service": s, "action": a, "duplicates": n })).collect::<Vec<_>>(),
    }))
}

// ─── exports ───

fn csv_field(s: &str) -> String {
    // A leading =, +, -, @, tab or CR would run as a formula in a spreadsheet.
    let s = if s.starts_with(['=', '+', '-', '@', '\t', '\r']) { format!("'{s}") } else { s.to_string() };
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn cell(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Number(n) => match n.as_f64() {
            Some(f) if n.is_f64() && f.fract() == 0.0 => format!("{f:.0}"),
            _ => n.to_string(),
        },
        other => other.to_string(),
    }
}

/// One flat table: section, name, metric, value.
fn to_csv(r: &Value) -> String {
    let mut rows = vec![["section", "name", "metric", "value"].map(csv_field).join(",")];
    let mut push = |section: &str, name: &str, metric: &str, value: &Value| rows.push([section, name, metric, &cell(value)].map(csv_field).join(","));
    push("period", &cell(&r["period"]["label"]), "from", &r["period"]["from"]);
    push("period", &cell(&r["period"]["label"]), "to", &r["period"]["to"]);
    for section in ["totals", "approvals"] {
        for (k, v) in r[section].as_object().into_iter().flatten() {
            push(section, "", k, v);
        }
    }
    for m in r["money_protected"]["by_currency"].as_array().into_iter().flatten() {
        push("money_protected_estimate", &cell(&m["currency"]), "amount", &m["amount"]);
        push("money_protected_estimate", &cell(&m["currency"]), "payments", &m["payments"]);
    }
    for (section, key) in [("agents", "agent_id"), ("services", "service")] {
        for row in r[section].as_array().into_iter().flatten() {
            for (k, v) in row.as_object().into_iter().flatten().filter(|(k, _)| *k != key) {
                push(section, &cell(&row[key]), k, v);
            }
        }
    }
    for t in r["top_duplicated"].as_array().into_iter().flatten() {
        push("top_duplicated", &format!("{}.{}", cell(&t["service"]), cell(&t["action"])), "duplicates", &t["duplicates"]);
    }
    rows.join("\n") + "\n"
}

struct Brand {
    name: String,
    logo_url: Option<String>,
    accent: String,
}

async fn brand(state: &SharedState, org_id: &str) -> Brand {
    let default = Brand { name: "AgentRaaS".into(), logo_url: None, accent: "#1f6feb".into() };
    if crate::agent::db::effective_tier(state, org_id).await < agentraas_core::tier::Tier::Enterprise {
        return default;
    }
    let row: Option<(Option<String>, Option<String>, Option<String>)> =
        sqlx::query_as("SELECT display_name, logo_url, accent_color FROM org_branding WHERE org_id = $1").bind(org_id).fetch_optional(&state.pg).await.ok().flatten();
    let Some((name, logo_url, accent)) = row else { return default };
    Brand {
        name: name.filter(|n| !n.trim().is_empty()).unwrap_or(default.name),
        logo_url: logo_url.filter(|u| u.starts_with("https://")),
        accent: accent.filter(|c| is_hex_color(c)).unwrap_or(default.accent),
    }
}

fn pct(v: &Value) -> String {
    v.as_f64().map(|p| format!("{p}%")).unwrap_or_else(|| "-".into())
}

/// A page that opens on its own: inline CSS, no scripts, no remote assets
/// except an Enterprise org's own logo.
fn to_html(r: &Value, b: &Brand) -> String {
    let e = |v: &Value| escape_html(&cell(v));
    let t = &r["totals"];
    let a = &r["approvals"];
    let mut h = String::new();
    let _ = write!(
        h,
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>{name} report: {period}</title><style>
body{{font:15px/1.5 system-ui,-apple-system,Segoe UI,sans-serif;color:#1d232b;background:#fff;margin:0;padding:24px 16px}}
main{{max-width:860px;margin:0 auto}}h1{{font-size:24px;margin:8px 0 2px}}h2{{font-size:17px;margin:28px 0 8px;border-bottom:2px solid {accent};padding-bottom:4px}}
.muted{{color:#5b6573;font-size:13px}}.tiles{{display:grid;grid-template-columns:repeat(auto-fit,minmax(150px,1fr));gap:10px}}
.tile{{border:1px solid #d9dee5;border-radius:6px;padding:10px 12px}}.tile b{{display:block;font-size:22px;color:{accent}}}
table{{border-collapse:collapse;width:100%;font-size:14px}}th,td{{text-align:left;padding:6px 8px;border-bottom:1px solid #e6e9ee}}td.n,th.n{{text-align:right}}
img.logo{{max-height:40px}}@media (prefers-color-scheme:dark){{body{{background:#14181d;color:#e4e8ee}}.tile,th,td{{border-color:#2c333c}}.muted{{color:#9aa4b1}}}}
</style></head><body><main>"#,
        name = escape_html(&b.name),
        period = e(&r["period"]["label"]),
        accent = b.accent,
    );
    if let Some(logo) = &b.logo_url {
        let _ = write!(h, r#"<img class="logo" src="{}" alt="">"#, escape_html(logo));
    }
    let _ = write!(
        h,
        r#"<h1>{} for {}</h1><p class="muted">{} &middot; {} to {} (UTC)</p><div class="tiles">"#,
        escape_html(&b.name),
        e(&r["org_id"]),
        e(&r["period"]["label"]),
        e(&r["period"]["from"]),
        e(&r["period"]["to"]),
    );
    for (label, v) in [
        ("Actions", &t["actions"]),
        ("Duplicates caught", &t["duplicates_caught"]),
        ("Blocked by policy", &t["blocked_by_policy"]),
        ("Blocked by spend cap", &t["blocked_by_spend_cap"]),
        ("Held for approval", &t["held_for_approval"]),
        ("Shadow calls (not sent)", &t["shadow_calls"]),
    ] {
        let _ = write!(h, "<div class=\"tile\"><b>{}</b>{label}</div>", e(v));
    }
    h.push_str("</div><h2>Money protected (estimate)</h2>");
    let money = r["money_protected"]["by_currency"].as_array().cloned().unwrap_or_default();
    if money.is_empty() {
        h.push_str("<p>No duplicate payment with an amount and currency in this period.</p>");
    } else {
        h.push_str("<table><tr><th>Currency</th><th class=\"n\">Amount</th><th class=\"n\">Payments</th></tr>");
        for m in &money {
            let amount = m["amount"].as_f64().map(|x| format!("{x:.2}")).unwrap_or_default();
            let _ = write!(h, "<tr><td>{}</td><td class=\"n\">{}</td><td class=\"n\">{}</td></tr>", e(&m["currency"]), amount, e(&m["payments"]));
        }
        h.push_str("</table>");
    }
    let _ = write!(h, "<p class=\"muted\">{}</p>", e(&r["money_protected"]["note"]));
    let _ = write!(
        h,
        "<h2>Approvals</h2><p>Approved {}, denied {}, failed after approval {}, still pending {}.</p>",
        e(&a["approved"]),
        e(&a["denied"]),
        e(&a["failed_after_approval"]),
        e(&a["still_pending"])
    );
    h.push_str("<h2>Agents</h2><table><tr><th>Agent</th><th class=\"n\">Actions</th><th class=\"n\">Duplicate rate</th><th class=\"n\">Block rate</th><th class=\"n\">Error rate</th></tr>");
    for g in r["agents"].as_array().into_iter().flatten() {
        let _ = write!(h, "<tr><td>{}</td><td class=\"n\">{}</td><td class=\"n\">{}</td><td class=\"n\">{}</td><td class=\"n\">{}</td></tr>", e(&g["agent_id"]), e(&g["actions"]), pct(&g["duplicate_rate"]), pct(&g["block_rate"]), pct(&g["error_rate"]));
    }
    h.push_str("</table><h2>Services</h2><table><tr><th>Service</th><th class=\"n\">Actions</th><th class=\"n\">Error rate</th><th class=\"n\">Timeouts</th><th class=\"n\">Breaker trips</th><th class=\"n\">Drift alerts</th></tr>");
    for s in r["services"].as_array().into_iter().flatten() {
        let _ = write!(h, "<tr><td>{}</td><td class=\"n\">{}</td><td class=\"n\">{}</td><td class=\"n\">{}</td><td class=\"n\">{}</td><td class=\"n\">{}</td></tr>", e(&s["service"]), e(&s["actions"]), pct(&s["error_rate"]), e(&s["timeouts"]), e(&s["circuit_breaker_trips"]), e(&s["schema_drift_alerts"]));
    }
    let _ = write!(h, "</table><p class=\"muted\">{}</p><h2>Most duplicated actions</h2>", e(&r["circuit_breaker_note"]));
    let top = r["top_duplicated"].as_array().cloned().unwrap_or_default();
    if top.is_empty() {
        h.push_str("<p>No duplicates in this period.</p>");
    } else {
        h.push_str("<table><tr><th>Action</th><th class=\"n\">Duplicates</th></tr>");
        for d in &top {
            let _ = write!(h, "<tr><td>{}.{}</td><td class=\"n\">{}</td></tr>", e(&d["service"]), e(&d["action"]), e(&d["duplicates"]));
        }
        h.push_str("</table>");
    }
    h.push_str("</main></body></html>\n");
    h
}

// ─── API ───

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

#[derive(Deserialize)]
struct ReportQuery {
    org_id: String,
    period: Option<String>,
    month: Option<String>,
    format: Option<String>,
}

async fn report(State(state): State<SharedState>, user: AuthUser, Query(q): Query<ReportQuery>) -> Result<axum::response::Response, ApiError> {
    check_member(&state, &user, &q.org_id).await?;
    let p = period(q.period.as_deref().unwrap_or("30d"), q.month.as_deref(), Utc::now().naive_utc()).map_err(|m| ApiError::new(StatusCode::BAD_REQUEST, m))?;
    let r = build(&state, &q.org_id, &p).await?;
    let file = format!("agentraas-report-{}-{}", q.org_id, p.from.format("%Y-%m-%d"));
    let download = |content_type: &str, ext: &str, body: String| {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, content_type.parse().unwrap());
        headers.insert(header::CONTENT_DISPOSITION, format!("attachment; filename=\"{file}.{ext}\"").parse().unwrap());
        (headers, body).into_response()
    };
    match q.format.as_deref().unwrap_or("json") {
        "json" => Ok(Json(r).into_response()),
        "csv" => Ok(download("text/csv; charset=utf-8", "csv", to_csv(&r))),
        "html" => Ok(download("text/html; charset=utf-8", "html", to_html(&r, &brand(&state, &q.org_id).await))),
        _ => Err(ApiError::new(StatusCode::BAD_REQUEST, "format must be json, csv or html.")),
    }
}

#[derive(Deserialize)]
struct OrgQuery {
    org_id: String,
}

async fn get_settings(State(state): State<SharedState>, user: AuthUser, Query(q): Query<OrgQuery>) -> Result<Json<Value>, ApiError> {
    check_member(&state, &user, &q.org_id).await?;
    let row: Option<(bool, Vec<String>, Option<String>)> =
        sqlx::query_as("SELECT enabled, recipients, last_sent_period FROM report_settings WHERE org_id = $1").bind(&q.org_id).fetch_optional(&state.pg).await?;
    let (enabled, recipients, last_sent) = row.unwrap_or_default();
    Ok(Json(json!({ "enabled": enabled, "recipients": recipients, "last_sent_period": last_sent, "email_configured": state.mailer.is_configured() })))
}

#[derive(Deserialize)]
struct SettingsBody {
    org_id: String,
    enabled: bool,
    #[serde(default)]
    recipients: Vec<String>,
}

async fn put_settings(State(state): State<SharedState>, user: AuthUser, Json(b): Json<SettingsBody>) -> Result<Json<Value>, ApiError> {
    check_member(&state, &user, &b.org_id).await?;
    if !check_org_write_permission(&state.pg, user.sub, &b.org_id).await? {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "Auditors have read-only access to this org."));
    }
    let recipients: Vec<String> = b.recipients.iter().map(|r| r.trim().to_string()).filter(|r| !r.is_empty()).collect();
    if recipients.len() > 10 || recipients.iter().any(|r| r.len() > 254 || r.parse::<lettre::Address>().is_err()) {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "recipients: up to 10 valid email addresses."));
    }
    if b.enabled && recipients.is_empty() {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "Add at least one recipient to turn the monthly email on."));
    }
    sqlx::query(
        "INSERT INTO report_settings (org_id, enabled, recipients, updated_at) VALUES ($1, $2, $3, NOW())
         ON CONFLICT (org_id) DO UPDATE SET enabled = EXCLUDED.enabled, recipients = EXCLUDED.recipients, updated_at = NOW()",
    )
    .bind(&b.org_id)
    .bind(b.enabled)
    .bind(&recipients)
    .execute(&state.pg)
    .await?;
    Ok(Json(json!({ "enabled": b.enabled, "recipients": recipients })))
}

// ─── monthly email ───

const EMAIL_LOOP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3600);

/// Hourly: every opted-in org that hasn't had last month's report gets it.
/// Hourly rather than on the 1st, so a server that was off then still sends.
pub fn spawn_monthly_email_loop(state: SharedState) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(EMAIL_LOOP_INTERVAL);
        loop {
            interval.tick().await;
            if state.mailer.is_configured() {
                send_due(&state).await;
            }
        }
    });
}

async fn send_due(state: &SharedState) {
    let now = Utc::now().naive_utc();
    let last_month = (now.date().with_day(1).expect("day 1 exists") - chrono::Duration::days(1)).format("%Y-%m").to_string();
    // One org claimed at a time, before its send: two servers never send the
    // same month, and a crash loses at most the one email in flight (a
    // failed send is logged, not retried, rather than risk sending twice).
    loop {
        let claimed: Option<(String, Vec<String>)> = match sqlx::query_as(
            "UPDATE report_settings SET last_sent_period = $1 WHERE org_id = (
               SELECT org_id FROM report_settings WHERE enabled AND cardinality(recipients) > 0
                 AND last_sent_period IS DISTINCT FROM $1 LIMIT 1 FOR UPDATE SKIP LOCKED)
             RETURNING org_id, recipients",
        )
        .bind(&last_month)
        .fetch_optional(&state.pg)
        .await
        {
            Ok(row) => row,
            Err(err) => {
                tracing::error!(?err, "monthly report: claim failed");
                return;
            }
        };
        let Some((org_id, recipients)) = claimed else { return };
        let Ok(p) = period("month", Some(&last_month), now) else { continue };
        let report = match build(state, &org_id, &p).await {
            Ok(r) => r,
            Err(err) => {
                tracing::error!(?err, org_id, "monthly report: build failed");
                continue;
            }
        };
        let b = brand(state, &org_id).await;
        let html = to_html(&report, &b);
        let t = &report["totals"];
        let text = format!(
            "{} report for {}, {}: {} actions, {} duplicates caught, {} blocked by policy, {} blocked by spend cap. Open the HTML part of this email for the full report.",
            b.name, org_id, p.label, t["actions"], t["duplicates_caught"], t["blocked_by_policy"], t["blocked_by_spend_cap"]
        );
        for to in &recipients {
            state.mailer.send_report(to, &format!("{} report: {}", b.name, p.label), &text, &html).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_reads_each_providers_fields_and_units() {
        assert_eq!(money_of("stripe", &json!({"amount": 2500, "currency": "usd"})), Some((25.0, "USD".into())));
        assert_eq!(money_of("stripe", &json!({"amount": 500, "currency": "jpy"})), Some((500.0, "JPY".into())));
        assert_eq!(money_of("razorpay", &json!({"amount": 12345, "currency": "INR"})), Some((123.45, "INR".into())));
        assert_eq!(money_of("adyen", &json!({"amount": {"value": 1000, "currency": "KWD"}})), Some((1.0, "KWD".into())));
        assert_eq!(money_of("mollie", &json!({"amount": {"value": "10.50", "currency": "EUR"}})), Some((10.5, "EUR".into())));
        assert_eq!(
            money_of("paypal", &json!({"purchase_units": [{"amount": {"value": "99.99", "currency_code": "USD"}}]})),
            Some((99.99, "USD".into()))
        );
        assert_eq!(money_of("cashfree", &json!({"order_amount": 1200, "order_currency": "INR"})), Some((1200.0, "INR".into())));
    }

    #[test]
    fn money_never_guesses() {
        assert_eq!(money_of("stripe", &json!({"amount": 2500})), None, "no currency, no amount");
        assert_eq!(money_of("stripe", &json!({"amount": 2500, "currency": "dollars"})), None);
        assert_eq!(money_of("stripe", &json!({"amount": -5, "currency": "usd"})), None);
        assert_eq!(money_of("slack", &json!({"amount": 5, "currency": "usd"})), None, "not a payment service");
    }

    #[test]
    fn periods() {
        let now = NaiveDate::from_ymd_opt(2026, 10, 29).unwrap().and_hms_opt(12, 0, 0).unwrap();
        let p = period("month", Some("2026-09"), now).unwrap();
        assert_eq!((p.from.to_string(), p.to.to_string()), ("2026-09-01 00:00:00".into(), "2026-10-01 00:00:00".into()));
        let current = period("month", None, now).unwrap();
        assert_eq!((current.from.day(), current.to), (1, now), "this month ends now");
        assert_eq!(period("7d", None, now).unwrap().from, now - chrono::Duration::days(7));
        assert!(period("month", Some("2026-13"), now).is_err());
        assert!(period("1y", None, now).is_err());
    }

    #[test]
    fn rates_round_and_skip_empty() {
        assert_eq!(rate(1, 3), Some(33.33));
        assert_eq!(rate(0, 0), None);
    }

    #[test]
    fn csv_quotes_and_defuses_formulas() {
        assert_eq!(csv_field("a\"b"), "\"a\"\"b\"");
        assert_eq!(csv_field("=cmd()"), "\"'=cmd()\"");
    }

    #[test]
    fn html_escapes_names_and_uses_the_brand() {
        let r = json!({"org_id": "o", "period": {"label": "x", "from": "f", "to": "t"}, "totals": {}, "approvals": {},
            "money_protected": {"by_currency": [], "note": ""}, "agents": [{"agent_id": "<script>", "actions": 1}], "services": [], "top_duplicated": []});
        let h = to_html(&r, &Brand { name: "Acme & Co".into(), logo_url: None, accent: "#ff0000".into() });
        assert!(h.contains("&lt;script&gt;") && !h.contains("<script>"));
        assert!(h.contains("Acme &amp; Co") && h.contains("#ff0000"));
        assert!(is_hex_color("#a1B2c3") && !is_hex_color("red") && !is_hex_color("#12345g"));
    }
}
