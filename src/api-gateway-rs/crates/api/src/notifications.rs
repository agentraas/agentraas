//! Notification Webhooks (`/api/v1/notification-webhooks`): instant outage,
//! schema-drift and outcome-unknown notifications to Slack, Discord,
//! Telegram, or as Jira tickets.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::agent::db::{get_user_writable_org_ids, check_org_write_permission, get_user_org_ids};
use crate::auth::{check_dashboard_rate_limit, is_valid_identifier, AuthUser};
use crate::state::{ApiError, SharedState};
use crate::util::validate_target_url;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/v1/notification-webhooks", post(create_webhook).get(list_webhooks))
        .route("/api/v1/notification-webhooks/:id", delete(delete_webhook))
        .route("/api/v1/notification-webhooks/:id/test", post(test_webhook))
}

/// Fires when the circuit breaker blocks a request for a service this org
/// actually uses. Rate-limited to once per org+service per 60s window via
/// Redis SETNX, so a burst of blocked requests during one outage sends one
/// notification, not one per request.
pub async fn notify_circuit_open(state: &SharedState, org_id: &str, service: &str) {
    let Ok(mut conn) = state.redis_conn_result() else { return };
    let key = format!("circuit-notified:{org_id}:{service}");
    let claimed: Option<String> = redis::cmd("SET")
        .arg(&key)
        .arg("1")
        .arg("EX")
        .arg(60)
        .arg("NX")
        .query_async(&mut conn)
        .await
        .unwrap_or(None);
    if claimed.as_deref() != Some("OK") {
        return;
    }
    send_outage_notification(
        state,
        org_id,
        &format!("⚠️ AgentRaaS Circuit Breaker Activated: {service} is unresponsive. Requests to it are being shielded for your org until it recovers."),
    )
    .await;
}

/// Fires when `crate::schema_drift` detects a real change (a field
/// removed or its type changed) in a service's response shape. Same
/// claim-key rate-limit shape as
/// `notify_circuit_open` above, 1 hour instead of 60s since a shape
/// change is a slower-moving signal than an outage.
pub async fn notify_schema_drift(state: &SharedState, org_id: &str, service: &str, action: &str, removed: &[String], type_changed: &[String]) {
    let Ok(mut conn) = state.redis_conn_result() else { return };
    let key = format!("schema-drift-notified:{service}:{action}");
    let claimed: Option<String> = redis::cmd("SET").arg(&key).arg("1").arg("EX").arg(3600).arg("NX").query_async(&mut conn).await.unwrap_or(None);
    if claimed.as_deref() != Some("OK") {
        return;
    }
    let mut details = Vec::new();
    if !removed.is_empty() {
        details.push(format!("removed: {}", removed.join(", ")));
    }
    if !type_changed.is_empty() {
        details.push(format!("type changed: {}", type_changed.join(", ")));
    }
    send_outage_notification(
        state,
        org_id,
        &format!("🔧 AgentRaaS Schema Drift Detected: {service}.{action}'s response shape changed ({}). Code that reads fields from this response may be affected.", details.join("; ")),
    )
    .await;
}

/// A call timed out after it was sent (or the server stopped mid-call), so
/// it may have run, and it is deliberately not retried: someone should look.
/// Once an hour per org and action. Spawned so the caller's response never
/// waits on Slack or Jira.
pub fn spawn_outcome_unknown(state: &SharedState, org_id: &str, service: &str, action: &str) {
    let (state, org_id, service, action) = (state.clone(), org_id.to_string(), service.to_string(), action.to_string());
    tokio::spawn(async move {
        let Ok(mut conn) = state.redis_conn_result() else { return };
        let key = format!("outcome-unknown-notified:{org_id}:{service}.{action}");
        let claimed: Option<String> = redis::cmd("SET").arg(&key).arg("1").arg("EX").arg(3600).arg("NX").query_async(&mut conn).await.unwrap_or(None);
        if claimed.as_deref() != Some("OK") {
            return;
        }
        let message = format!(
            "AgentRaaS: a {service}.{action} call may have run. It timed out after it was sent, so it was not retried. Check {service} before replaying it from Failed requests."
        );
        send_outage_notification(&state, &org_id, &message).await;
    });
}

/// Jira settings kept encrypted in `encrypted_target`. `extra` holds
/// `PROJECT_KEY|Issue type`.
#[derive(serde::Serialize, Deserialize)]
struct JiraTarget {
    site: String,
    email: String,
    token: String,
}

/// One ticket per org per alert text per day: Jira is a work queue, so a
/// flapping service must not open a ticket every minute like a chat ping.
async fn create_jira_ticket(state: &SharedState, org_id: &str, target: &str, extra: Option<&str>, message: &str) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    let jira: JiraTarget = serde_json::from_str(target).map_err(|e| e.to_string())?;
    let (project, issue_type) = extra.and_then(|e| e.split_once('|')).ok_or("missing project key")?;
    let mut conn = state.redis_conn_result().map_err(|_| "redis unavailable")?;
    let key = format!("jira-ticket:{org_id}:{}", hex::encode(&Sha256::digest(message.as_bytes())[..12]));
    let claimed: Option<String> = redis::cmd("SET").arg(&key).arg("1").arg("EX").arg(86400).arg("NX").query_async(&mut conn).await.unwrap_or(None);
    if claimed.as_deref() != Some("OK") {
        return Ok(());
    }
    let summary: String = message.chars().take(250).collect();
    let body = json!({ "fields": {
        "project": { "key": project },
        "issuetype": { "name": issue_type },
        "summary": summary,
        "description": format!("{message}\n\nOrg: {org_id}\nDashboard: {}/dashboard", state.public_url),
        "labels": ["agentraas"],
    }});
    let resp = state
        .http_client
        .post(format!("https://{}.atlassian.net/rest/api/2/issue", jira.site))
        .basic_auth(&jira.email, Some(&jira.token))
        .json(&body)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let status = resp.status();
        // Let the next alert try again instead of waiting out the day.
        let _: Result<(), _> = redis::cmd("DEL").arg(&key).query_async(&mut conn).await;
        return Err(format!("Jira answered {status}: {}", resp.text().await.unwrap_or_default().chars().take(300).collect::<String>()));
    }
    Ok(())
}

/// Jira site names are subdomains of atlassian.net, project keys are
/// uppercase identifiers, issue types are short names ("Task", "Bug").
fn jira_settings_error(site: &str, email: &str, token: &str, project: &str, issue_type: &str) -> Option<&'static str> {
    if site.is_empty() || site.len() > 63 || !site.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Some("site is the part before .atlassian.net, lowercase letters, digits and dashes (e.g. \"yourcompany\").");
    }
    if !crate::auth::is_valid_email(email) {
        return Some("email must be the Atlassian account email the API token belongs to.");
    }
    if token.is_empty() || token.len() > 500 {
        return Some("target (the Jira API token) is required.");
    }
    if project.is_empty() || project.len() > 20 || !project.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') || !project.starts_with(|c: char| c.is_ascii_uppercase()) {
        return Some("project_key must be the Jira project key, e.g. \"OPS\".");
    }
    if issue_type.is_empty() || issue_type.len() > 50 || issue_type.contains('|') {
        return Some("issue_type must be a Jira issue type name, e.g. \"Task\".");
    }
    None
}

pub async fn send_outage_notification(state: &SharedState, org_id: &str, message: &str) {
    #[derive(sqlx::FromRow)]
    struct Row {
        r#type: String,
        encrypted_target: String,
        extra: Option<String>,
    }
    let rows: Vec<Row> = match sqlx::query_as(
        "SELECT type, encrypted_target, extra FROM notification_webhooks WHERE org_id = $1",
    )
    .bind(org_id)
    .fetch_all(&state.pg)
    .await
    {
        Ok(r) => r,
        Err(err) => {
            tracing::warn!(?err, org_id, "failed to load notification webhooks");
            return;
        }
    };

    for row in rows {
        let target = match state.cipher.decrypt(&row.encrypted_target) {
            Ok(t) => t,
            Err(err) => {
                tracing::warn!(?err, org_id, r#type = %row.r#type, "failed to decrypt notification target");
                continue;
            }
        };
        let result = match row.r#type.as_str() {
            "slack" => state.http_client.post(&target).json(&json!({ "text": message })).timeout(std::time::Duration::from_secs(5)).send().await,
            "discord" => state.http_client.post(&target).json(&json!({ "content": message })).timeout(std::time::Duration::from_secs(5)).send().await,
            "telegram" => {
                let url = format!("https://api.telegram.org/bot{target}/sendMessage");
                state.http_client.post(&url).json(&json!({ "chat_id": row.extra, "text": message })).timeout(std::time::Duration::from_secs(5)).send().await
            }
            "jira" => {
                if let Err(err) = create_jira_ticket(state, org_id, &target, row.extra.as_deref(), message).await {
                    tracing::warn!(org_id, error = %err, "Jira ticket creation failed");
                }
                continue;
            }
            _ => continue,
        };
        if let Err(err) = result {
            tracing::warn!(org_id, r#type = %row.r#type, error = %err, "outage notification failed");
        }
    }
}

#[derive(Deserialize)]
struct CreateWebhookBody {
    org_id: Option<String>,
    r#type: Option<String>,
    target: Option<String>,
    chat_id: Option<String>,
    site: Option<String>,
    email: Option<String>,
    project_key: Option<String>,
    issue_type: Option<String>,
}

async fn create_webhook(
    State(state): State<SharedState>,
    user: AuthUser,
    Json(body): Json<CreateWebhookBody>,
) -> Result<Json<Value>, ApiError> {
    check_dashboard_rate_limit(&state, user.sub).await?;

    let org_id = body.org_id.unwrap_or_default();
    if !is_valid_identifier(&org_id) {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "org_id must be 1-100 characters, letters/numbers/underscore/hyphen only."));
    }
    let webhook_type = body.r#type.unwrap_or_default();
    if !["slack", "discord", "telegram", "jira"].contains(&webhook_type.as_str()) {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "type must be one of: slack, discord, telegram, jira."));
    }
    if !check_org_write_permission(&state.pg, user.sub, &org_id).await? {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "Auditors have read-only access to this org."));
    }
    let target = body.target.unwrap_or_default();
    if target.is_empty() || target.chars().count() > 500 {
        let msg = match webhook_type.as_str() {
            "telegram" => "target (bot token) is required.",
            "jira" => "target (the Jira API token) is required.",
            _ => "target (webhook URL) is required.",
        };
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, msg));
    }
    if webhook_type == "slack" || webhook_type == "discord" {
        if let Some(err) = validate_target_url(&target).await {
            return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, err));
        }
    }
    if webhook_type == "telegram" && body.chat_id.as_deref().unwrap_or("").is_empty() {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "chat_id is required for type \"telegram\"."));
    }

    let (stored_target, extra) = match webhook_type.as_str() {
        "telegram" => (target, body.chat_id.clone()),
        "jira" => {
            let site = body.site.unwrap_or_default().trim().to_ascii_lowercase();
            let site = site.trim_end_matches(".atlassian.net").trim_start_matches("https://").to_string();
            let email = crate::auth::normalize_email(&body.email.unwrap_or_default());
            let project = body.project_key.unwrap_or_default().trim().to_ascii_uppercase();
            let issue_type = body.issue_type.filter(|t| !t.trim().is_empty()).unwrap_or_else(|| "Task".to_string()).trim().to_string();
            if let Some(err) = jira_settings_error(&site, &email, &target, &project, &issue_type) {
                return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, err));
            }
            let settings = serde_json::to_string(&JiraTarget { site, email, token: target }).map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "An internal error occurred."))?;
            (settings, Some(format!("{project}|{issue_type}")))
        }
        _ => (target, None),
    };
    let encrypted_target = state.cipher.encrypt(&stored_target);

    #[derive(sqlx::FromRow)]
    struct Row {
        id: i32,
        org_id: String,
        r#type: String,
        created_at: chrono::DateTime<chrono::Utc>,
    }
    let row = sqlx::query_as::<_, Row>(
        "INSERT INTO notification_webhooks (org_id, type, encrypted_target, extra, created_by)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (org_id, type) DO UPDATE SET encrypted_target = EXCLUDED.encrypted_target, extra = EXCLUDED.extra
         RETURNING id, org_id, type, created_at",
    )
    .bind(&org_id)
    .bind(&webhook_type)
    .bind(&encrypted_target)
    .bind(&extra)
    .bind(user.sub)
    .fetch_one(&state.pg)
    .await?;

    Ok(Json(json!({
        "saved": true,
        "webhook": { "id": row.id, "org_id": row.org_id, "type": row.r#type, "created_at": row.created_at },
    })))
}

async fn list_webhooks(State(state): State<SharedState>, user: AuthUser) -> Result<Json<Vec<Value>>, ApiError> {
    check_dashboard_rate_limit(&state, user.sub).await?;
    let org_ids = get_user_org_ids(&state.pg, user.sub).await?;
    if org_ids.is_empty() {
        return Ok(Json(vec![]));
    }
    #[derive(sqlx::FromRow)]
    struct Row {
        id: i32,
        org_id: String,
        r#type: String,
        created_at: chrono::DateTime<chrono::Utc>,
    }
    let rows = sqlx::query_as::<_, Row>(
        "SELECT id, org_id, type, created_at FROM notification_webhooks WHERE org_id = ANY($1) ORDER BY created_at DESC",
    )
    .bind(&org_ids)
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| json!({ "id": r.id, "org_id": r.org_id, "type": r.r#type, "created_at": r.created_at }))
            .collect(),
    ))
}

async fn delete_webhook(State(state): State<SharedState>, user: AuthUser, Path(id): Path<i32>) -> Result<Json<Value>, ApiError> {
    check_dashboard_rate_limit(&state, user.sub).await?;
    let org_ids = get_user_writable_org_ids(&state.pg, user.sub).await?;
    if org_ids.is_empty() {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "Notification webhook not found."));
    }
    let deleted: Option<i32> = sqlx::query_scalar("DELETE FROM notification_webhooks WHERE id = $1 AND org_id = ANY($2) RETURNING id")
        .bind(id)
        .bind(&org_ids)
        .fetch_optional(&state.pg)
        .await?;
    if deleted.is_none() {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "Notification webhook not found."));
    }
    Ok(Json(json!({ "deleted": true })))
}

async fn test_webhook(State(state): State<SharedState>, user: AuthUser, Path(id): Path<i32>) -> Result<Json<Value>, ApiError> {
    check_dashboard_rate_limit(&state, user.sub).await?;
    let org_ids = get_user_org_ids(&state.pg, user.sub).await?;
    let org_id: Option<String> = sqlx::query_scalar("SELECT org_id FROM notification_webhooks WHERE id = $1 AND org_id = ANY($2)")
        .bind(id)
        .bind(&org_ids)
        .fetch_optional(&state.pg)
        .await?;
    let Some(org_id) = org_id else {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "Notification webhook not found."));
    };
    send_outage_notification(&state, &org_id, "🔔 AgentRaaS test notification — if you can see this, outage alerts are wired up correctly.").await;
    Ok(Json(json!({ "sent": true })))
}

#[cfg(test)]
mod tests {
    use super::jira_settings_error;

    #[test]
    fn jira_settings_keep_the_request_on_atlassian() {
        assert_eq!(jira_settings_error("acme", "a@acme.com", "tok", "OPS", "Task"), None);
        // A site can't move the host off <site>.atlassian.net.
        for site in ["evil.com/x", "acme.evil", "a@b", "ACME", ""] {
            assert!(jira_settings_error(site, "a@acme.com", "tok", "OPS", "Task").is_some(), "{site}");
        }
        assert!(jira_settings_error("acme", "a@acme.com", "tok", "ops", "Task").is_some());
        assert!(jira_settings_error("acme", "a@acme.com", "tok", "OPS", "Task|x").is_some());
        assert!(jira_settings_error("acme", "a@acme.com", "", "OPS", "Task").is_some());
    }
}
