//! Ports `forwardAction`/`forwardWithRetry`/`isRetryableError` from
//! `src/core/proxy/index.js` — the actual outbound call to a curated
//! service or custom action, with circuit-breaker-aware retry.

use std::time::Duration;

use agentraas_core::circuit_breaker;
use serde_json::{json, Value};

use super::db::{extract_upstream_error_message, get_credential_checked, ResolvedMcpRoute, ResolvedRoute};
use crate::state::SharedState;

pub struct ForwardError {
    pub message: String,
    /// Present only when the upstream itself responded (vs. a network/
    /// internal error).
    pub upstream_status: Option<u16>,
    pub upstream_body: Option<Value>,
    /// Set once `recordFailure` has already been called for this error, so
    /// the caller's own catch block doesn't double-count it — mirrors
    /// `err.circuitAlreadyRecorded`.
    pub circuit_already_recorded: bool,
    /// The request may have reached the upstream (a timeout, or a connection
    /// that broke after sending) and no response came back, so whether the
    /// action ran is unknown. Never retried automatically; the caller keeps
    /// the dedup slot instead of releasing it (see `mark_dedup_slot_unknown`).
    pub outcome_unknown: bool,
    /// The org hasn't finished setting the service up (no key saved). Nothing
    /// was sent, a retry can't help, and it says nothing about the provider's
    /// health, so it is neither retried nor counted against the shared circuit.
    pub setup_error: bool,
}

/// The longest a forward can take: every attempt timing out, plus backoff.
fn forward_work_ms(timeout_seconds: u64, attempts: u32, base_delay_ms: u64) -> i64 {
    let attempts = attempts.max(1) as i64;
    timeout_seconds as i64 * 1000 * attempts + base_delay_ms as i64 * (1i64 << attempts.min(20))
}

/// When a claimer should have finished forwarding (epoch ms): the longest
/// forward plus a minute of slack. A slot still pending after this means the
/// process handling it stopped mid-call.
pub fn forward_lease_until_ms(state: &SharedState) -> i64 {
    let work_ms = forward_work_ms(state.proxy_timeout_seconds, state.proxy_retry_max_attempts, state.proxy_retry_base_delay_ms) + 60_000;
    let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
    now_ms + work_ms
}

/// How long a resource lock lives: the longest forward plus 5 s, so a slow
/// call never loses its lock while it is still running. It's released as
/// soon as the call ends; the TTL only matters after a crash or an
/// outcome-unknown timeout, which keeps it.
pub fn resource_lock_ttl_seconds(state: &SharedState) -> i64 {
    forward_work_ms(state.proxy_timeout_seconds, state.proxy_retry_max_attempts, state.proxy_retry_base_delay_ms) / 1000 + 5
}

/// `send()` failed. A refused connection or DNS failure means nothing was
/// sent: a plain failure, safe to retry. Anything else (a timeout, a reset
/// mid-request) may have executed upstream.
fn send_error(err: reqwest::Error) -> ForwardError {
    ForwardError {
        outcome_unknown: !(err.is_connect() || err.is_builder()),
        message: err.to_string(),
        upstream_status: None,
        upstream_body: None,
        circuit_already_recorded: false,
        setup_error: false,
    }
}

fn is_retryable(err: &ForwardError) -> bool {
    if err.setup_error {
        return false;
    }
    match err.upstream_status {
        // nothing sent (refused, DNS): retry; possibly executed: never
        None => !err.outcome_unknown,
        Some(status) => status == 429 || (500..=599).contains(&status),
    }
}

/// Everything `forward_action` and `forward_action_streaming` share: SSRF
/// re-check, credential lookup, and building the outbound request up to
/// (but not including) `.send()` — so the two response-handling strategies
/// (buffer-and-parse vs. pass-through) don't have to duplicate the request
/// side.
#[allow(clippy::too_many_arguments)]
async fn build_request(
    state: &SharedState,
    route: &ResolvedRoute,
    service_name: &str,
    org_id: &str,
    payload: &Value,
    req_id: &str,
    end_user_id: Option<&str>,
    upstream_key: Option<&str>,
) -> Result<reqwest::RequestBuilder, ForwardError> {
    // Custom actions and inbound-webhook destinations are validated for
    // SSRF (`validate_target_url`) once, at registration time — a
    // destination's DNS record can change afterward (a low-TTL rebind to
    // an internal/metadata IP), and a stored destination gets called
    // indefinitely, not just once. Re-checking here closes that standing
    // window instead of only protecting the moment of registration. Not
    // applied to curated services, whose URLs come from this app's own
    // static config, not user input.
    if service_name == "custom" {
        if let Some(err) = crate::util::validate_target_url(&route.url).await {
            return Err(ForwardError {
                message: format!("Target URL failed a safety re-check: {err}"),
                upstream_status: None,
                upstream_body: None,
                circuit_already_recorded: false,
                outcome_unknown: false,
                setup_error: false,
            });
        }
    }

    let credential = match get_credential_checked(state, &route.credential_key, org_id, end_user_id).await {
        Ok(c) => c,
        Err(message) => {
            return Err(ForwardError { message, upstream_status: None, upstream_body: None, circuit_already_recorded: false, outcome_unknown: false, setup_error: false })
        }
    };

    if !route.internal && route.auth_type != "none" && credential.is_none() {
        let message = match end_user_id {
            Some(uid) => format!("No credentials connected for end-user \"{uid}\" on {service_name}. This never falls back to a shared org-wide credential — connect one for this specific end-user from the dashboard's Credentials panel."),
            None => format!("No credentials configured for {service_name}. Add them from the dashboard's Credentials panel."),
        };
        return Err(ForwardError {
            message,
            upstream_status: None,
            upstream_body: None,
            circuit_already_recorded: false,
            outcome_unknown: false,
            setup_error: true,
        });
    }

    // A curated route's URL may name fields of the call ({owner}) or the
    // stored account ({ACCOUNT}); those fields leave the body. Custom URLs are
    // user-typed, so they are never filled in: a `{JWT_SECRET}` in one would
    // otherwise send this server's secrets to the user's host.
    let mut body = payload.clone();
    let url = if service_name == "custom" {
        route.url.clone()
    } else {
        let account = credential.as_ref().and_then(|c| c.username.as_deref());
        fill_url_placeholders(&route.url, &mut body, account).map_err(|message| ForwardError {
            message,
            upstream_status: Some(400),
            upstream_body: None,
            circuit_already_recorded: false,
            outcome_unknown: false,
            setup_error: false,
        })?
    };
    let payload = &body;

    let mut builder = state
        .http_client
        .request(
            route
                .method
                .parse()
                .unwrap_or(reqwest::Method::POST),
            &url,
        )
        .header("Content-Type", &route.content_type)
        .header("X-AgentRaaS-ReqId", req_id)
        .timeout(Duration::from_secs(state.proxy_timeout_seconds));

    if let Some(extra) = &route.extra_headers {
        if let Some(obj) = extra.as_object() {
            for (k, v) in obj {
                match (v.as_str(), upstream_key) {
                    // A provider with its own idempotency header (RazorpayX
                    // X-Payout-Idempotency) gets the same stable key.
                    (Some("{idempotency_key}"), Some(key)) => builder = builder.header(k, key),
                    (Some("{idempotency_key}"), None) => {}
                    (Some(s), _) => builder = builder.header(k, s),
                    _ => match v.get("encrypted").and_then(Value::as_str).map(|c| state.cipher.decrypt(c)) {
                        Some(Ok(secret)) => builder = builder.header(k, secret),
                        Some(Err(err)) => tracing::error!(?err, header = %k, "failed to decrypt custom-action secret header"),
                        None => {}
                    },
                }
            }
        }
    }

    // One stable key per logical action (derived from the dedup hash), so a
    // provider that honors Idempotency-Key (Stripe and others) runs it once
    // even when our retry, the agent's retry or a DLQ replay reaches it
    // again. A Custom Action that sets its own Idempotency-Key keeps it.
    if let Some(key) = upstream_key {
        let overridden = route
            .extra_headers
            .as_ref()
            .and_then(Value::as_object)
            .is_some_and(|obj| obj.keys().any(|k| k.eq_ignore_ascii_case("idempotency-key")));
        if !overridden {
            builder = builder.header("Idempotency-Key", key);
        }
    }

    if let Some(cred) = &credential {
        builder = match state.token_logins.get(service_name).filter(|_| route.auth_type == "login") {
            Some(spec) => {
                let token = super::token_login::token(state, spec, service_name, org_id, end_user_id, cred).await.map_err(|message| ForwardError {
                    message,
                    upstream_status: None,
                    upstream_body: None,
                    circuit_already_recorded: false,
                    outcome_unknown: false,
                    setup_error: true,
                })?;
                builder.header("Authorization", format!("{} {token}", spec.prefix))
            }
            None => apply_auth(builder, &route.auth_type, route.auth_header.as_deref(), cred),
        };
    }

    let builder = if route.method.eq_ignore_ascii_case("GET") {
        // A read sends what is left of the payload (after {path} fields) as a query string, never a body.
        let mut form = Vec::new();
        flatten_form("", payload, &mut form);
        builder.query(&form)
    } else if route.content_type == "application/x-www-form-urlencoded" {
        // Stripe-style services expect form-encoded bodies — flatten the
        // (already-validated) JSON payload into form fields, same as
        // axios's `application/x-www-form-urlencoded` content type would
        // when handed a plain object.
        let mut form = Vec::new();
        flatten_form("", payload, &mut form);
        builder.form(&form)
    } else {
        builder.json(payload)
    };

    Ok(builder)
}

#[allow(clippy::too_many_arguments)]
pub async fn forward_action(
    state: &SharedState,
    route: &ResolvedRoute,
    service_name: &str,
    action_name: &str,
    org_id: &str,
    payload: &Value,
    req_id: &str,
    end_user_id: Option<&str>,
    upstream_key: Option<&str>,
) -> Result<Value, ForwardError> {
    // Chaos mode — checked before the real call, once per attempt (not
    // once per top-level request), so a fail_rate under 1.0 lets a retry
    // legitimately succeed. Off (no Redis key) for every org/service by
    // default; see agentraas_core::chaos.
    if let Ok(mut conn) = state.redis_conn_result() {
        if agentraas_core::chaos::should_fail(&mut conn, org_id, service_name).await {
            return Err(ForwardError {
                message: "Chaos mode: synthetic upstream failure injected for resilience testing".to_string(),
                upstream_status: Some(503),
                upstream_body: None,
                circuit_already_recorded: false,
                outcome_unknown: false,
                setup_error: false,
            });
        }
    }

    let mut relogin = route.auth_type == "login";
    let (status, body) = loop {
        let builder = build_request(state, route, service_name, org_id, payload, req_id, end_user_id, upstream_key).await?;
        let response = builder.send().await.map_err(send_error)?;
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        if status.as_u16() == 401 && route.auth_type == "login" {
            super::token_login::forget(state, service_name, org_id, end_user_id).await;
            // The token expired or was revoked early. A 401 means the call
            // was refused, not run, so it is safe to log in and send it once more.
            if std::mem::take(&mut relogin) {
                continue;
            }
        }
        break (status, body);
    };

    if status.as_u16() >= 400 {
        let message = extract_upstream_error_message(&body)
            .unwrap_or_else(|| format!("HTTP {}", status.as_u16()));
        return Err(ForwardError {
            message,
            upstream_status: Some(status.as_u16()),
            upstream_body: Some(body),
            circuit_already_recorded: false,
            outcome_unknown: false,
            setup_error: false,
        });
    }

    // Slack's Web API always returns HTTP 200, even on failure.
    if service_name == "slack" && body.get("ok") == Some(&Value::Bool(false)) {
        let message = body
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("Slack API returned ok:false")
            .to_string();
        return Err(ForwardError {
            message,
            upstream_status: Some(status.as_u16()),
            upstream_body: Some(body),
            circuit_already_recorded: false,
            outcome_unknown: false,
            setup_error: false,
        });
    }

    let upstream_id = body
        .get("id")
        .or_else(|| body.get("object_id"))
        .or_else(|| body.get("sid"))
        .cloned()
        .unwrap_or(Value::Null);

    // Tool Output Sanitization (Enterprise, opt-in per org): applied only to
    // what's actually surfaced to the calling agent below, never to
    // `upstream_id` above — that's extracted from the real response for
    // internal tracking/audit and must stay accurate regardless of this
    // per-org toggle.
    #[cfg(feature = "enterprise")]
    let body = if state.enterprise_mode
        && super::db::is_output_sanitization_enabled(&state.pg, org_id).await.unwrap_or(false)
    {
        agentraas_core::output_sanitize::sanitize_output(&body)
    } else {
        body
    };

    // Tool Result & Context Pruner — Community + Enterprise both get this,
    // applied after sanitization (order doesn't matter for correctness,
    // just picking one) and only to what's surfaced to the agent below.
    let body = if super::db::is_pruning_enabled(&state.pg, org_id).await.unwrap_or(false) {
        agentraas_core::pruner::prune_output(&body)
    } else {
        body
    };

    Ok(json!({
        "service": service_name,
        "action": action_name,
        "forwarded": true,
        "upstream_status": status.as_u16(),
        "upstream_id": upstream_id,
        "upstream_response": body,
        "timestamp": crate::util::iso_now(),
    }))
}

/// MCP Custom Actions' forwarder — the JSON-RPC counterpart to
/// `forward_action` above. Builds a `tools/call` envelope against a
/// registered third-party MCP server and unwraps its response into the
/// same `Result<Value, ForwardError>` shape `forward_action` returns, so
/// `mcp.rs::handle_tools_call` can call this in place of
/// `forward_with_retry` without needing a second reliability pipeline —
/// the dedup claim, circuit-breaker check, and audit logging around this
/// call are all unchanged, they just don't know the difference.
///
/// v1 deliberately does a single attempt, no retry-with-backoff loop like
/// `forward_with_retry` has — still wrapped in circuit-breaker record_success/
/// record_failure by the caller either way. Add a retrying variant once this
/// is proven in real use; not worth duplicating the backoff loop for a v1.
pub async fn forward_mcp_tool_call(
    state: &SharedState,
    route: &ResolvedMcpRoute,
    org_id: &str,
    payload: &Value,
    req_id: &str,
    end_user_id: Option<&str>,
) -> Result<Value, ForwardError> {
    if let Some(err) = crate::util::validate_target_url(&route.target_url).await {
        return Err(ForwardError {
            message: format!("Target URL failed a safety re-check: {err}"),
            upstream_status: None,
            upstream_body: None,
            circuit_already_recorded: false,
            outcome_unknown: false,
            setup_error: false,
        });
    }

    let credential = match get_credential_checked(state, &route.credential_key, org_id, end_user_id).await {
        Ok(c) => c,
        Err(message) => {
            return Err(ForwardError { message, upstream_status: None, upstream_body: None, circuit_already_recorded: false, outcome_unknown: false, setup_error: false })
        }
    };
    if route.auth_type != "none" && credential.is_none() {
        let message = match end_user_id {
            Some(uid) => format!("No credentials connected for end-user \"{uid}\" on this MCP server. Connect one for this specific end-user from the dashboard's Credentials panel."),
            None => "No credentials configured for this MCP server. Add them from the dashboard's MCP Servers panel.".to_string(),
        };
        return Err(ForwardError {
            message,
            upstream_status: None,
            upstream_body: None,
            circuit_already_recorded: false,
            outcome_unknown: false,
            setup_error: true,
        });
    }

    let mut builder = state
        .http_client
        .post(&route.target_url)
        .header("Content-Type", "application/json")
        .header("X-AgentRaaS-ReqId", req_id)
        .timeout(Duration::from_secs(state.proxy_timeout_seconds));

    if let Some(cred) = &credential {
        builder = apply_auth(builder, &route.auth_type, route.auth_header.as_deref(), cred);
    }

    let rpc_request = json!({
        "jsonrpc": "2.0",
        "id": req_id,
        "method": "tools/call",
        "params": { "name": route.remote_tool_name, "arguments": payload },
    });

    let response = builder.json(&rpc_request).send().await.map_err(send_error)?;

    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);

    if status.as_u16() >= 400 {
        let message = extract_upstream_error_message(&body).unwrap_or_else(|| format!("HTTP {}", status.as_u16()));
        return Err(ForwardError { message, upstream_status: Some(status.as_u16()), upstream_body: Some(body), circuit_already_recorded: false, outcome_unknown: false, setup_error: false });
    }
    // JSON-RPC-level error — a 200 with an "error" field, distinct from a
    // transport-level 4xx/5xx above.
    if body.get("error").is_some() {
        let message = extract_upstream_error_message(&body).unwrap_or_else(|| "MCP server returned a JSON-RPC error".to_string());
        return Err(ForwardError { message, upstream_status: Some(status.as_u16()), upstream_body: Some(body), circuit_already_recorded: false, outcome_unknown: false, setup_error: false });
    }
    let result = body.get("result").cloned().unwrap_or(Value::Null);
    // MCP's own convention: a successful JSON-RPC envelope whose result
    // still carries isError:true means the TOOL failed, not the transport —
    // same "genuine failure" treatment forward_action gives Slack's
    // ok:false (which is also always HTTP 200).
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        let message = result
            .get("content")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
            .and_then(|c| c.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("MCP tool call failed")
            .to_string();
        return Err(ForwardError { message, upstream_status: Some(status.as_u16()), upstream_body: Some(body), circuit_already_recorded: false, outcome_unknown: false, setup_error: false });
    }

    Ok(json!({
        "service": format!("mcp:{}", route.credential_key.trim_start_matches("mcp:")),
        "action": route.remote_tool_name,
        "forwarded": true,
        "upstream_status": status.as_u16(),
        "mcp_result": result,
        "timestamp": crate::util::iso_now(),
    }))
}

/// A streaming upstream response: status/headers are already available, but
/// the body is deliberately left unconsumed so the caller can pipe it
/// through live (`Response::bytes_stream()`/`chunk()`) instead of buffering
/// it the way `forward_action` does at its `response.json().await` line —
/// the one thing that makes SSE/chunked upstream responses (an LLM tool
/// streaming tokens, for example) currently unsupported.
pub struct StreamingForward {
    pub status: reqwest::StatusCode,
    pub content_type: Option<String>,
    pub response: reqwest::Response,
}

/// Streaming counterpart to `forward_action`. Shares SSRF re-check,
/// credential lookup, and request-building via `build_request`; diverges
/// only at response time: a 2xx response is handed back with its body
/// untouched, a 4xx/5xx response is buffered (small, error bodies only) so
/// error handling/retry behaves identically to the non-streaming path.
/// Deliberately skips: enterprise output sanitization, the pruner, and the
/// Slack `ok:false` check — all of those need the parsed body, which a
/// stream doesn't have; a streaming route is expected to be a raw
/// token/event feed, not a structured API response those checks apply to.
#[allow(clippy::too_many_arguments)]
pub async fn forward_action_streaming(
    state: &SharedState,
    route: &ResolvedRoute,
    service_name: &str,
    org_id: &str,
    payload: &Value,
    req_id: &str,
    end_user_id: Option<&str>,
    upstream_key: Option<&str>,
) -> Result<StreamingForward, ForwardError> {
    let builder = build_request(state, route, service_name, org_id, payload, req_id, end_user_id, upstream_key).await?;

    let response = builder.send().await.map_err(send_error)?;

    let status = response.status();

    if status.as_u16() >= 400 {
        let body: Value = response.json().await.unwrap_or(Value::Null);
        let message = extract_upstream_error_message(&body)
            .unwrap_or_else(|| format!("HTTP {}", status.as_u16()));
        return Err(ForwardError {
            message,
            upstream_status: Some(status.as_u16()),
            upstream_body: Some(body),
            circuit_already_recorded: false,
            outcome_unknown: false,
            setup_error: false,
        });
    }

    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    Ok(StreamingForward { status, content_type, response })
}

/// Streaming counterpart to `forward_with_retry`. Retry only ever applies to
/// the pre-stream `Err` case (a network error, or a buffered 4xx/5xx before
/// any bytes reached the client) — once a `StreamingForward` is returned
/// here, the caller is already committed to forwarding it, so there is no
/// "retry after the first byte" case to guard against separately; it falls
/// out of `Ok`/`Err` never being retried once `Ok`.
#[allow(clippy::too_many_arguments)]
pub async fn forward_with_retry_streaming(
    state: &SharedState,
    route: &ResolvedRoute,
    service_name: &str,
    org_id: &str,
    payload: &Value,
    req_id: &str,
    circuit_key: &str,
    end_user_id: Option<&str>,
    upstream_key: Option<&str>,
) -> Result<StreamingForward, ForwardError> {
    let mut last_error = None;

    for attempt in 1..=state.proxy_retry_max_attempts {
        match forward_action_streaming(state, route, service_name, org_id, payload, req_id, end_user_id, upstream_key).await {
            Ok(result) => return Ok(result),
            Err(mut err) => {
                err.circuit_already_recorded = true;
                if let (false, Ok(mut conn)) = (err.setup_error, state.redis_conn_result()) {
                    if let Ok(Some(transition)) = circuit_breaker::record_failure(&mut conn, circuit_key).await {
                        log_circuit_transition(state, transition).await;
                    }
                }

                let retryable = is_retryable(&err);
                let is_last_attempt = attempt == state.proxy_retry_max_attempts;

                if is_last_attempt || !retryable {
                    last_error = Some(err);
                    break;
                }

                let circuit_open = {
                    if let Ok(mut conn) = state.redis_conn_result() {
                        circuit_breaker::get_circuit_state(&mut conn, circuit_key)
                            .await
                            .map(|(s, _)| s == "open")
                            .unwrap_or(false)
                    } else {
                        false
                    }
                };
                if circuit_open {
                    last_error = Some(err);
                    break;
                }

                let delay_ms = state.proxy_retry_base_delay_ms * 2u64.pow(attempt - 1)
                    + (rand::random::<u32>() % 100) as u64;
                tracing::warn!(
                    req_id,
                    service = service_name,
                    attempt,
                    delay_ms,
                    error = %err.message,
                    "AgentRaaS: retrying transient upstream failure (streaming route)"
                );
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                last_error = Some(err);
            }
        }
    }

    Err(last_error.expect("loop always sets last_error before exiting without returning Ok"))
}
/// Puts the stored credential on the outbound request the way the service
/// expects it. Shared by tool forwards and MCP calls.
fn apply_auth(builder: reqwest::RequestBuilder, auth_type: &str, auth_header: Option<&str>, cred: &super::db::Credential) -> reqwest::RequestBuilder {
    let key = || cred.api_key.clone().or_else(|| cred.username.clone()).unwrap_or_default();
    match auth_type {
        "basic" => {
            let username = cred.username.clone().or_else(|| cred.api_key.clone()).unwrap_or_default();
            builder.basic_auth(username, Some(cred.password.clone().unwrap_or_default()))
        }
        // A key id and a secret in two headers (Alpaca): authHeader names
        // both, "<id header>, <secret header>".
        "header-pair" => match auth_header.and_then(|h| h.split_once(',')) {
            Some((id_header, secret_header)) => {
                let id = cred.username.clone().or_else(|| cred.api_key.clone()).unwrap_or_default();
                builder.header(id_header.trim(), id).header(secret_header.trim(), cred.password.clone().unwrap_or_default())
            }
            None => builder,
        },
        "custom-header" => match auth_header {
            Some(name) => builder.header(name, key()),
            None => builder,
        },
        // "Authorization: Token <key>" (Delhivery).
        "token" => builder.header("Authorization", format!("Token {}", key())),
        _ => match auth_header {
            Some("Authorization") => builder.header("Authorization", format!("Bearer {}", key())),
            Some(name) => builder.header(name, key()),
            None => builder,
        },
    }
}


/// Fills `{name}` placeholders in a curated route's URL. A lowercase or
/// camelCase name comes from the call's own field of that name, which is
/// then removed from the body (GitHub's `{owner}/{repo}`, Zendesk's
/// `{subdomain}`); its value may only use letters, digits, `.`, `_` and `-`,
/// with no `..`, so it can't change the host or climb the path. `{ACCOUNT}`
/// is the stored credential's username (Twilio's Account SID). Any other
/// UPPERCASE name reads the env var of that name (self-host settings).
fn fill_url_placeholders(url: &str, body: &mut Value, account: Option<&str>) -> Result<String, String> {
    let mut result = String::with_capacity(url.len());
    let mut rest = url;
    while let Some(start) = rest.find('{') {
        result.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            result.push_str(&rest[start..]);
            return Ok(result);
        };
        let key = &after[..end];
        rest = &after[end + 1..];
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            result.push('{');
            result.push_str(key);
            result.push('}');
            continue;
        }
        if key.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') {
            let value = if key == "ACCOUNT" { account.map(str::to_string) } else { std::env::var(key).ok() };
            match value {
                Some(v) if key == "ACCOUNT" && !is_safe_url_part(&v) => return Err("The stored credential's account ID (username) may only contain letters, digits, '.', '_' and '-'.".to_string()),
                Some(v) => result.push_str(&v),
                None if key == "ACCOUNT" => return Err("The stored credential has no account ID (username) for this service.".to_string()),
                None => {
                    result.push('{');
                    result.push_str(key);
                    result.push('}');
                }
            }
            continue;
        }
        let value = body.as_object_mut().and_then(|o| o.remove(key));
        let text = match value {
            Some(Value::String(s)) => s,
            Some(Value::Number(n)) => n.to_string(),
            _ => return Err(format!("Missing field \"{key}\": this action needs it in the URL.")),
        };
        if !is_safe_url_part(&text) {
            return Err(format!("Field \"{key}\" may only contain letters, digits, '.', '_' and '-'."));
        }
        result.push_str(&text);
    }
    result.push_str(rest);
    Ok(result)
}

/// A value that can't change the host or climb the path once in a URL.
fn is_safe_url_part(text: &str) -> bool {
    !text.is_empty() && text.len() <= 200 && !text.contains("..") && text.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Form encoding the way Stripe reads it: nested objects and arrays become
/// bracketed keys (`items[0][price]=...`, `metadata[order]=...`).
fn flatten_form(prefix: &str, v: &Value, out: &mut Vec<(String, String)>) {
    match v {
        Value::Object(obj) => {
            for (k, inner) in obj {
                let key = if prefix.is_empty() { k.clone() } else { format!("{prefix}[{k}]") };
                flatten_form(&key, inner, out);
            }
        }
        Value::Array(items) => {
            for (i, inner) in items.iter().enumerate() {
                flatten_form(&format!("{prefix}[{i}]"), inner, out);
            }
        }
        other => out.push((prefix.to_string(), value_to_form_string(other))),
    }
}

fn value_to_form_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Retry-with-backoff, circuit-breaker-aware in both directions: every
/// failed attempt records a real failure, and a retry is abandoned the
/// moment the breaker trips open mid-sequence.
#[allow(clippy::too_many_arguments)]
pub async fn forward_with_retry(
    state: &SharedState,
    route: &ResolvedRoute,
    service_name: &str,
    action_name: &str,
    org_id: &str,
    payload: &Value,
    req_id: &str,
    circuit_key: &str,
    end_user_id: Option<&str>,
    upstream_key: Option<&str>,
) -> Result<Value, ForwardError> {
    let mut last_error = None;

    for attempt in 1..=state.proxy_retry_max_attempts {
        match forward_action(state, route, service_name, action_name, org_id, payload, req_id, end_user_id, upstream_key).await {
            Ok(mut result) => {
                if attempt > 1 {
                    result["retried"] = json!(attempt - 1);
                }
                return Ok(result);
            }
            Err(mut err) => {
                err.circuit_already_recorded = true;
                if let (false, Ok(mut conn)) = (err.setup_error, state.redis_conn_result()) {
                    if let Ok(Some(transition)) = circuit_breaker::record_failure(&mut conn, circuit_key).await {
                        log_circuit_transition(state, transition).await;
                    }
                }

                let retryable = is_retryable(&err);
                let is_last_attempt = attempt == state.proxy_retry_max_attempts;

                if is_last_attempt || !retryable {
                    last_error = Some(err);
                    break;
                }

                let circuit_open = {
                    if let Ok(mut conn) = state.redis_conn_result() {
                        circuit_breaker::get_circuit_state(&mut conn, circuit_key)
                            .await
                            .map(|(s, _)| s == "open")
                            .unwrap_or(false)
                    } else {
                        false
                    }
                };
                if circuit_open {
                    last_error = Some(err);
                    break;
                }

                let delay_ms = state.proxy_retry_base_delay_ms * 2u64.pow(attempt - 1)
                    + (rand::random::<u32>() % 100) as u64;
                tracing::warn!(
                    req_id,
                    service = service_name,
                    action = action_name,
                    attempt,
                    delay_ms,
                    error = %err.message,
                    "AgentRaaS: retrying transient upstream failure"
                );
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                last_error = Some(err);
            }
        }
    }

    Err(last_error.expect("loop always sets last_error before exiting without returning Ok"))
}

/// Multi-Destination Fan-Out (event broadcasting) — best-effort copies of
/// the same payload to every configured `fanout_urls` destination, after
/// the primary target_url call has already succeeded. Never affects the
/// primary response, the dedup outcome, or the audit log status.
pub fn broadcast_fanout(state: &SharedState, route: &ResolvedRoute, payload: &Value, req_id: &str) {
    for url in &route.fanout_urls {
        let client = state.http_client.clone();
        let payload = payload.clone();
        let req_id = req_id.to_string();
        let url = url.clone();
        tokio::spawn(async move {
            let result = client
                .post(&url)
                .header("X-AgentRaaS-ReqId", &req_id)
                .header("X-AgentRaaS-Fanout", "true")
                .json(&payload)
                .timeout(Duration::from_secs(10))
                .send()
                .await;
            if let Err(err) = result {
                tracing::warn!(url, req_id, error = %err, "fan-out broadcast failed (best-effort, not retried)");
            }
        });
    }
}

/// Retry-with-backoff counterpart to `forward_mcp_tool_call`, same shape as
/// `forward_with_retry` above (fast-follow noted when MCP Custom Actions
/// first shipped as a single-attempt-only v1 — this replaces that).
#[allow(clippy::too_many_arguments)]
pub async fn forward_mcp_with_retry(
    state: &SharedState,
    route: &ResolvedMcpRoute,
    org_id: &str,
    payload: &Value,
    req_id: &str,
    circuit_key: &str,
    end_user_id: Option<&str>,
) -> Result<Value, ForwardError> {
    let mut last_error = None;

    for attempt in 1..=state.proxy_retry_max_attempts {
        match forward_mcp_tool_call(state, route, org_id, payload, req_id, end_user_id).await {
            Ok(mut result) => {
                if attempt > 1 {
                    result["retried"] = json!(attempt - 1);
                }
                return Ok(result);
            }
            Err(mut err) => {
                err.circuit_already_recorded = true;
                if let (false, Ok(mut conn)) = (err.setup_error, state.redis_conn_result()) {
                    if let Ok(Some(transition)) = circuit_breaker::record_failure(&mut conn, circuit_key).await {
                        log_circuit_transition(state, transition).await;
                    }
                }

                let retryable = is_retryable(&err);
                let is_last_attempt = attempt == state.proxy_retry_max_attempts;

                if is_last_attempt || !retryable {
                    last_error = Some(err);
                    break;
                }

                let circuit_open = {
                    if let Ok(mut conn) = state.redis_conn_result() {
                        circuit_breaker::get_circuit_state(&mut conn, circuit_key)
                            .await
                            .map(|(s, _)| s == "open")
                            .unwrap_or(false)
                    } else {
                        false
                    }
                };
                if circuit_open {
                    last_error = Some(err);
                    break;
                }

                let delay_ms = state.proxy_retry_base_delay_ms * 2u64.pow(attempt - 1)
                    + (rand::random::<u32>() % 100) as u64;
                tracing::warn!(
                    req_id,
                    tool = route.remote_tool_name,
                    attempt,
                    delay_ms,
                    error = %err.message,
                    "AgentRaaS: retrying transient MCP upstream failure"
                );
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                last_error = Some(err);
            }
        }
    }

    Err(last_error.expect("loop always sets last_error before exiting without returning Ok"))
}

pub async fn log_circuit_transition(state: &SharedState, transition: circuit_breaker::Transition) {
    if let Err(err) = sqlx::query(
        "INSERT INTO circuit_breaker_events (service, from_state, to_state) VALUES ($1, $2, $3)",
    )
    .bind(&transition.service)
    .bind(&transition.from_state)
    .bind(&transition.to_state)
    .execute(&state.pg)
    .await
    {
        tracing::warn!(?err, service = %transition.service, "circuit transition log failed");
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_forward_never_outlives_its_resource_lock() {
        // 30 s timeout, 3 attempts, 200 ms base backoff: 90 s + 1.6 s.
        assert_eq!(super::forward_work_ms(30, 3, 200), 91_600);
        // The old fixed 15 s lock was shorter than a single attempt.
        assert!(super::forward_work_ms(30, 1, 0) / 1000 + 5 > 30);
        assert_eq!(super::forward_work_ms(30, 0, 0), 30_000, "zero attempts still means one");
    }

    #[test]
    fn credentials_land_in_the_headers_each_service_expects() {
        use super::super::db::Credential;
        let headers = |auth_type: &str, auth_header: Option<&str>, cred: &Credential| {
            let builder = reqwest::Client::new().post("http://example.test/x");
            super::apply_auth(builder, auth_type, auth_header, cred).build().unwrap().headers().clone()
        };
        let pair = Credential { api_key: None, username: Some("PKID".into()), password: Some("SECRET".into()) };
        let h = headers("header-pair", Some("APCA-API-KEY-ID, APCA-API-SECRET-KEY"), &pair);
        assert_eq!(h["apca-api-key-id"], "PKID");
        assert_eq!(h["apca-api-secret-key"], "SECRET");
        assert!(h.get("authorization").is_none());

        let key = Credential { api_key: Some("sk_test_1".into()), username: None, password: None };
        assert_eq!(headers("bearer", Some("Authorization"), &key)["authorization"], "Bearer sk_test_1");
        assert_eq!(headers("bearer", Some("X-API-Key"), &key)["x-api-key"], "sk_test_1");
        // Dhan: the raw token in its own header, no "Bearer".
        let dhan = headers("custom-header", Some("access-token"), &key);
        assert_eq!(dhan["access-token"], "sk_test_1");
        assert!(dhan.get("authorization").is_none());
        assert_eq!(headers("token", None, &key)["authorization"], "Token sk_test_1");
        assert!(headers("basic", None, &pair)["authorization"].to_str().unwrap().starts_with("Basic "));
    }

    #[test]
    fn url_placeholders_come_from_the_call_and_leave_the_body() {
        let mut body = serde_json::json!({"owner": "acme", "repo": "site.web", "title": "Bug"});
        let url = fill_url_placeholders("https://api.github.com/repos/{owner}/{repo}/issues", &mut body, None).unwrap();
        assert_eq!(url, "https://api.github.com/repos/acme/site.web/issues");
        assert_eq!(body, serde_json::json!({"title": "Bug"}));
    }

    #[test]
    fn url_placeholders_refuse_values_that_could_move_the_request() {
        for bad in ["evil.com/x", "a..b", "x?y=1", "x#y", "x@evil.com", ""] {
            let mut body = serde_json::json!({"subdomain": bad});
            assert!(fill_url_placeholders("https://{subdomain}.zendesk.com/api/v2/tickets.json", &mut body, None).is_err(), "{bad}");
        }
        let mut body = serde_json::json!({});
        assert!(fill_url_placeholders("https://x/{owner}", &mut body, None).unwrap_err().contains("owner"));
    }

    #[test]
    fn account_placeholder_is_the_credential_username_not_an_env_var() {
        let mut body = serde_json::json!({});
        let url = fill_url_placeholders("https://api.twilio.com/2010-04-01/Accounts/{ACCOUNT}/Messages.json", &mut body, Some("AC123")).unwrap();
        assert_eq!(url, "https://api.twilio.com/2010-04-01/Accounts/AC123/Messages.json");
        assert!(fill_url_placeholders("https://x/{ACCOUNT}", &mut body, None).is_err());
        assert!(fill_url_placeholders("https://x/{ACCOUNT}", &mut body, Some("AC1/../x")).is_err());
        assert!(fill_url_placeholders("https://x/{ACCOUNT}", &mut body, Some("x@evil.example")).is_err());
    }

    #[test]
    fn stripe_form_encoding_uses_brackets_for_nested_values() {
        let mut out = Vec::new();
        flatten_form("", &serde_json::json!({"customer": "cus_1", "items": [{"price": "price_9"}], "metadata": {"order": 7}}), &mut out);
        out.sort();
        assert_eq!(out, vec![
            ("customer".to_string(), "cus_1".to_string()),
            ("items[0][price]".to_string(), "price_9".to_string()),
            ("metadata[order]".to_string(), "7".to_string()),
        ]);
    }

    use super::*;

    fn err(upstream_status: Option<u16>, outcome_unknown: bool) -> ForwardError {
        ForwardError { message: String::new(), upstream_status, upstream_body: None, circuit_already_recorded: false, outcome_unknown, setup_error: false }
    }

    #[test]
    fn a_call_that_may_have_run_is_never_retried() {
        assert!(!is_retryable(&err(None, true)), "timeout after sending");
        assert!(is_retryable(&err(None, false)), "refused / DNS: nothing sent");
        assert!(is_retryable(&err(Some(503), false)));
        assert!(is_retryable(&err(Some(429), false)));
        assert!(!is_retryable(&err(Some(402), false)), "card declined");
    }

    #[test]
    fn a_missing_key_is_not_retried() {
        let missing_key = ForwardError { setup_error: true, ..err(None, false) };
        assert!(!is_retryable(&missing_key), "nothing was sent, but a retry can't find the key either");
    }
}
