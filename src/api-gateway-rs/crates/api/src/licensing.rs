//! Self-host license handling: verification/refresh (this deployment's
//! own license, see the top half below) and issuance (a customer's
//! license, only ever done on agentraas.io — the bottom half).
//!
//! **Verification** reads and periodically re-verifies `LICENSE_TOKEN`
//! (see `agentraas_core::license` for the token format and signature
//! verification itself), keeping `SharedState::license_tier` current.
//! Cloud never touches this at all (see `agent::db::effective_tier`,
//! which branches on `deployment_mode` before ever reaching here).
//!
//! **Issuance** signs a token on demand (`GET /api/v1/licensing/token`)
//! rather than pre-computing one at webhook time — `users.plan` and
//! `subscriptions.current_period_end` are already exactly the inputs a
//! license needs, and both are already kept correct by the existing
//! `paddle_webhook` handler in `long_tail.rs` (untouched by this file).
//! Signing lazily means there's no separate token storage to keep in
//! sync or invalidate — every dashboard fetch just re-signs fresh claims
//! over whatever the billing tables currently say, up to a week past the
//! subscription's own period end.

use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::agent::db::get_user_org_ids;
use crate::auth::{check_dashboard_rate_limit, is_valid_identifier, AuthUser};
use crate::state::{ApiError, SharedState};

// Day/month-scale TTLs don't need frequent re-checking — twice a day is
// plenty to pick up a renewal or catch an expiry without a restart.
const REFRESH_INTERVAL: Duration = Duration::from_secs(6 * 3600);

/// Self-hosting without a key gets Team for free; only Enterprise needs
/// a `LICENSE_TOKEN` (from agentraas.io, on request).
const SELF_HOST_FREE_TIER: agentraas_core::tier::Tier = agentraas_core::tier::Tier::Team;

fn resolve_from_env() -> agentraas_core::tier::Tier {
    let Ok(token) = std::env::var("LICENSE_TOKEN") else {
        return SELF_HOST_FREE_TIER;
    };
    if token.trim().is_empty() {
        return SELF_HOST_FREE_TIER;
    }
    match agentraas_core::license::verify(&token) {
        Some(license) => {
            tracing::info!(tier = ?license.tier, customer_id = %license.customer_id, "license token verified");
            license.tier.max(SELF_HOST_FREE_TIER)
        }
        None => {
            tracing::warn!("LICENSE_TOKEN is set but invalid, expired, or tampered — falling back to the free tier");
            SELF_HOST_FREE_TIER
        }
    }
}

/// Initial value for `AppState::license_tier`, read once at startup —
/// never blocks/fails startup regardless of what `LICENSE_TOKEN` holds.
pub fn initial_tier() -> agentraas_core::tier::Tier {
    resolve_from_env()
}

/// Background loop — re-verifies `LICENSE_TOKEN` periodically so a
/// renewed, corrected, or removed token takes effect without a restart.
/// Mirrors `health_checks::spawn_health_check_loop`'s pattern. A no-op
/// for cloud deployments, which never read `license_tier` at all.
pub fn spawn_license_refresh_loop(state: SharedState) {
    if state.deployment_mode == "cloud" {
        return;
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(REFRESH_INTERVAL);
        interval.tick().await; // tokio's first tick fires immediately — startup already resolved the initial value, skip re-doing it right away
        loop {
            interval.tick().await;
            let tier = resolve_from_env();
            *state.license_tier.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = tier;
        }
    });
}

// ─── issuance (agentraas.io only) ───

pub fn router() -> Router<SharedState> {
    Router::new().route("/api/v1/licensing/token", get(get_license_token))
}

#[derive(Deserialize)]
struct OrgQuery {
    org_id: String,
}

/// Default license window when the org's plan has no matching active
/// subscription row to read a real `current_period_end` from — an
/// inconsistent state that shouldn't normally happen (plan is only ever
/// set alongside a subscription by the webhook), but if it does, a short
/// fallback window self-corrects on the next dashboard fetch rather than
/// either erroring out or handing out a long-lived token with nothing
/// backing it.
const FALLBACK_LICENSE_HOURS: i64 = 24;

/// A key stays valid this long past the billing period's end, so a
/// self-hoster has a week to paste the renewed key instead of losing
/// features on the renewal day.
const LICENSE_GRACE_DAYS: i64 = 7;

/// When a key for a subscription ending at `period_end` expires, or `None`
/// when that period (plus grace) is already over: never sign a key that is
/// dead on arrival.
fn license_expiry(period_end: Option<chrono::DateTime<chrono::Utc>>, now: chrono::DateTime<chrono::Utc>) -> Option<chrono::DateTime<chrono::Utc>> {
    let expires_at = match period_end {
        Some(end) => end + chrono::Duration::days(LICENSE_GRACE_DAYS),
        None => now + chrono::Duration::hours(FALLBACK_LICENSE_HOURS),
    };
    (expires_at > now).then_some(expires_at)
}

async fn get_license_token(State(state): State<SharedState>, user: AuthUser, Query(q): Query<OrgQuery>) -> Result<Json<Value>, ApiError> {
    check_dashboard_rate_limit(&state, user.sub).await?;
    if !is_valid_identifier(&q.org_id) {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "org_id must be 1-100 characters, letters/numbers/underscore/hyphen only."));
    }
    let org_ids = get_user_org_ids(&state.pg, user.sub).await?;
    if !org_ids.contains(&q.org_id) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "Not a member of this org."));
    }

    // Same "which row wins" hazard as `get_org_owner_plan` in agent/db.rs:
    // an org with an invited teammate has 2+ user rows sharing this
    // org_id with divergent `plan` values, so this can't just take
    // whichever row `fetch_optional` happens to return first. Fetch every
    // match and keep the highest-tier one — its `id` is also the one to
    // look up a real `subscriptions` row for below, since that's the
    // specific user the Paddle webhook set `plan` on.
    let owners: Vec<(i32, String)> = sqlx::query_as("SELECT id, plan FROM users WHERE org_id = $1").bind(&q.org_id).fetch_all(&state.pg).await?;
    let Some((owner_id, plan)) = owners.into_iter().max_by_key(|(_, plan)| agentraas_core::tier::Tier::from_plan_str(plan)) else {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "Org not found."));
    };
    let tier = agentraas_core::tier::Tier::from_plan_str(&plan);
    if tier == agentraas_core::tier::Tier::Community {
        // Nothing to license — Community doesn't gate anything a
        // self-hoster would need a token for.
        return Ok(Json(json!({ "tier": "free", "token": null, "expires_at": null })));
    }

    let period_end: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT current_period_end FROM subscriptions WHERE user_id = $1 AND status IN ('active', 'trialing') ORDER BY current_period_end DESC NULLS LAST LIMIT 1")
            .bind(owner_id)
            .fetch_optional(&state.pg)
            .await?
            .flatten();
    let Some(expires_at) = license_expiry(period_end, chrono::Utc::now()) else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "This subscription's billing period has ended and no renewal has been recorded, so there is no valid license key. Check the subscription, or contact support@agentraas.io.",
        ));
    };
    let ttl_seconds = (expires_at - chrono::Utc::now()).num_seconds() as u64;

    let Some(private_key_pem) = crate::util::configured_env("LICENSE_SIGNING_PRIVATE_KEY") else {
        return Err(ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "Licensing is not configured on this deployment."));
    };
    // PEM keys don't fit on one line, but most .env-file parsers (this
    // deployment's included) only support single-line values — so this
    // secret is stored with real newlines escaped as literal `\n` and
    // unescaped here, the same convention several cloud platforms use
    // for shipping PEM/JSON service-account keys through env vars.
    let private_key_pem = private_key_pem.replace("\\n", "\n");
    let token = agentraas_core::license::sign(&q.org_id, tier, ttl_seconds, &private_key_pem)
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "An internal error occurred."))?;

    Ok(Json(json!({ "tier": tier.as_plan_str(), "token": token, "expires_at": expires_at })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_run_a_week_past_the_period_and_never_start_expired() {
        let now = chrono::Utc::now();
        let end = now + chrono::Duration::days(20);
        assert_eq!(license_expiry(Some(end), now), Some(end + chrono::Duration::days(7)));
        // Period ended 3 days ago: still inside the grace week.
        let recent = now - chrono::Duration::days(3);
        assert_eq!(license_expiry(Some(recent), now), Some(recent + chrono::Duration::days(7)));
        // Ended 9 days ago (test3's case): no key at all, not an expired one.
        assert_eq!(license_expiry(Some(now - chrono::Duration::days(9)), now), None);
        assert_eq!(license_expiry(None, now), Some(now + chrono::Duration::hours(24)));
    }
}
