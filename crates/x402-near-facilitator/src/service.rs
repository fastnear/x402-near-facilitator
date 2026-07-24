//! Custom HTTP surface and durable settlement orchestration.
//!
//! `FacilitatorLocal` remains the scheme router.  The HTTP handlers are custom
//! because x402 protocol failures are successful HTTP exchanges, whereas
//! malformed/authentication/availability failures use ordinary HTTP status
//! codes.  Settlement is spawned into a detached task before the handler waits,
//! so dropping or timing out the HTTP request never cancels an in-flight
//! broadcast.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE, RETRY_AFTER};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use chrono::Utc;
use near_primitives::action::Action;
use near_primitives::hash::CryptoHash;
use near_primitives::transaction::Transaction;
use near_primitives::types::AccountId;
use near_primitives::views::FinalExecutionOutcomeView;
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::{Mutex, Semaphore};
use tower::ServiceBuilder;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::sensitive_headers::SetSensitiveRequestHeadersLayer;
use tower_http::trace::{DefaultOnResponse, TraceLayer};
use tracing::Instrument as _;
use uuid::Uuid;
use x402_chain_near::{
    NearChainProvider, NearRpcError, RelayerHead, RelayerStatus, TransactionLookup,
    VerificationFailure, VerificationPolicy, VerifiedPayment, decode_signed_delegate,
    decode_signed_transaction, interpret_final_outcome, signed_delegate_hash,
    signed_transaction_hash, validate_final_outcome_identity,
};
use x402_facilitator_local::FacilitatorLocal;
use x402_types::facilitator::Facilitator;
use x402_types::scheme::SchemeRegistry;

use crate::VERSION;
use crate::auth::{ApiKeyAuthenticator, AuthError, AuthenticatedClient};
use crate::config::ServiceConfig;
use crate::leadership::ReadinessState;
use crate::protocol::{
    ParsedRequest, SettleResponse, VerifyResponse, decimal_is_at_least, parse_request,
    request_fingerprint,
};
use crate::store::{
    ClaimOutcome, NewSettlement, PgStore, PreparedJournalEntry, SettlementRecord, SettlementState,
    StoreError, TerminalJournalEntry,
};
use crate::telemetry::Metrics;

const SERVICE_NAME: &str = "x402-near-facilitator";
const RETRY_SECONDS: &str = "1";

#[derive(Clone)]
#[allow(missing_debug_implementations)]
pub struct AppState {
    config: Arc<ServiceConfig>,
    store: PgStore,
    auth: ApiKeyAuthenticator,
    facilitator: Arc<FacilitatorLocal<SchemeRegistry>>,
    provider: Arc<NearChainProvider>,
    readiness: ReadinessState,
    rates: Arc<RateLimiter>,
    verify_slots: Arc<Semaphore>,
    relayer_lock: Arc<Mutex<()>>,
    metrics: Metrics,
}

impl AppState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: ServiceConfig,
        store: PgStore,
        auth: ApiKeyAuthenticator,
        facilitator: FacilitatorLocal<SchemeRegistry>,
        provider: NearChainProvider,
        readiness: ReadinessState,
        metrics: Metrics,
    ) -> Self {
        let max_concurrent_verify = config.request_limits.max_concurrent_verify;
        Self {
            config: Arc::new(config),
            store,
            auth,
            facilitator: Arc::new(facilitator),
            provider: Arc::new(provider),
            readiness,
            rates: Arc::new(RateLimiter::default()),
            verify_slots: Arc::new(Semaphore::new(max_concurrent_verify)),
            relayer_lock: Arc::new(Mutex::new(())),
            metrics,
        }
    }

    pub fn readiness(&self) -> &ReadinessState {
        &self.readiness
    }

    pub fn store(&self) -> &PgStore {
        &self.store
    }

    pub fn provider(&self) -> &NearChainProvider {
        &self.provider
    }

    pub fn relayer_lock(&self) -> Arc<Mutex<()>> {
        Arc::clone(&self.relayer_lock)
    }

    /// Refresh the chain-dependent readiness gates.  Both independent RPCs
    /// must report the configured network and finality, and the configured
    /// relayer key must be `FullAccess`, active in policy, and funded above the
    /// hard-stop threshold.
    pub async fn refresh_chain_readiness(&self) -> bool {
        let expected_chain_id = match self.config.environment {
            crate::config::Environment::Mainnet => "mainnet",
            crate::config::Environment::Testnet => "testnet",
        };
        let rpc_ready = matches!(
            self.provider.rpc_network_id().await,
            Ok(network) if network == expected_chain_id
        ) && matches!(
            self.provider.backup_rpc_network_id().await,
            Ok(network) if network == expected_chain_id
        ) && self.provider.rpc_final_block().await.is_ok()
            && self.provider.backup_rpc_final_block().await.is_ok();
        self.readiness.set_rpc(rpc_ready);

        let relayer_status = self.provider.relayer_status().await;
        let policy_active = self
            .store
            .relayer_is_active(
                &self.config.network,
                &self.config.relayer_account_id,
                &self.provider.relayer_public_key().to_string(),
            )
            .await
            .unwrap_or(false);
        if let Ok(status) = &relayer_status
            && let Ok(balance_yocto_near) = status
                .account
                .amount
                .as_yoctonear()
                .to_string()
                .parse::<f64>()
        {
            self.metrics.record_relayer(
                balance_yocto_near / 1_000_000_000_000_000_000_000_000_f64,
                !policy_active,
            );
        }
        let relayer_ready = relayer_status.is_ok_and(|status| {
            decimal_is_at_least(
                &status.account.amount.as_yoctonear().to_string(),
                &self.config.sponsorship.balance_hard_stop_yocto_near,
            )
        }) && policy_active;
        self.readiness.set_relayer(relayer_ready);

        if let Ok(summary) = self.store.journal_summary().await {
            let total = summary
                .reserved
                .saturating_add(summary.prepared)
                .saturating_add(summary.submitted);
            self.metrics.record_pending_settlements(total);
            self.metrics
                .record_journal_state("reserved", summary.reserved);
            self.metrics
                .record_journal_state("prepared", summary.prepared);
            self.metrics
                .record_journal_state("submitted", summary.submitted);
            let age = summary
                .oldest_created_at
                .and_then(|created| (Utc::now() - created).to_std().ok())
                .map_or(0.0, |duration| duration.as_secs_f64());
            self.metrics.record_oldest_pending_age(age);
        }
        if let Ok(usage) = self.store.global_sponsorship_usage_today().await
            && let Some(ratio) = decimal_usage_ratio(
                &usage.reserved_yocto_near,
                &usage.spent_yocto_near,
                &self.config.sponsorship.global_daily_yocto_near,
            )
        {
            self.metrics.record_budget_used_ratio(ratio);
        }
        rpc_ready && relayer_ready
    }
}

fn decimal_usage_ratio(reserved: &str, spent: &str, limit: &str) -> Option<f64> {
    let limit = limit.parse::<f64>().ok()?;
    if limit <= 0.0 {
        return None;
    }
    Some((reserved.parse::<f64>().ok()? + spent.parse::<f64>().ok()?) / limit)
}

pub fn router(state: AppState) -> Router {
    let request_id = HeaderName::from_static("x-request-id");
    Router::new()
        .route("/supported", get(supported))
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .route("/verify", post(verify))
        .route("/settle", post(settle))
        .with_state(state)
        .layer(
            ServiceBuilder::new()
                .layer(SetRequestIdLayer::new(request_id.clone(), MakeRequestUuid))
                .layer(SetSensitiveRequestHeadersLayer::new([
                    AUTHORIZATION,
                    HeaderName::from_static("x-api-key"),
                ]))
                .layer(
                    TraceLayer::new_for_http()
                        .make_span_with(|request: &Request| {
                            tracing::info_span!(
                                "http_request",
                                method = %request.method()
                            )
                        })
                        .on_response(DefaultOnResponse::new().include_headers(false)),
                )
                .layer(PropagateRequestIdLayer::new(request_id)),
        )
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
    retry: bool,
}

impl ApiError {
    const fn new(status: StatusCode, code: &'static str, message: &'static str) -> Self {
        Self {
            status,
            code,
            message,
            retry: false,
        }
    }

    const fn unavailable(code: &'static str, message: &'static str) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code,
            message,
            retry: true,
        }
    }

    const fn rate_limited(code: &'static str, message: &'static str) -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            code,
            message,
            retry: true,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = json!({
            "error": {
                "code": self.code,
                "message": self.message,
            }
        });
        let mut response = (self.status, axum::Json(body)).into_response();
        if self.retry {
            response
                .headers_mut()
                .insert(RETRY_AFTER, HeaderValue::from_static(RETRY_SECONDS));
        }
        response
    }
}

#[derive(Serialize)]
struct HealthResponse<'a> {
    status: &'a str,
    service: &'a str,
    version: &'a str,
}

#[derive(Serialize)]
struct ReadyResponse {
    ready: bool,
    checks: ReadyChecks,
}

#[derive(Serialize)]
struct ReadyChecks {
    database: &'static str,
    leadership: &'static str,
    reconciliation: &'static str,
    rpc: &'static str,
    relayer: &'static str,
}

async fn health() -> axum::Json<HealthResponse<'static>> {
    axum::Json(HealthResponse {
        status: "ok",
        service: SERVICE_NAME,
        version: VERSION,
    })
}

async fn ready(State(state): State<AppState>) -> Response {
    let database = state
        .store
        .operationally_ready(&state.config.network, &state.config.asset)
        .await
        .unwrap_or(false);
    let snapshot = state.readiness.snapshot();
    let is_ready = database
        && snapshot.leadership
        && snapshot.reconciliation
        && snapshot.rpc
        && snapshot.relayer;
    let body = ReadyResponse {
        ready: is_ready,
        checks: ReadyChecks {
            database: readiness_word(database),
            leadership: readiness_word(snapshot.leadership),
            reconciliation: readiness_word(snapshot.reconciliation),
            rpc: readiness_word(snapshot.rpc),
            relayer: readiness_word(snapshot.relayer),
        },
    };
    let status = if is_ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    let mut response = (status, axum::Json(body)).into_response();
    if !is_ready {
        response
            .headers_mut()
            .insert(RETRY_AFTER, HeaderValue::from_static(RETRY_SECONDS));
    }
    response
}

const fn readiness_word(value: bool) -> &'static str {
    if value { "ready" } else { "not_ready" }
}

async fn supported(State(state): State<AppState>) -> Response {
    match state.facilitator.supported().await {
        Ok(response) => axum::Json(response).into_response(),
        Err(_) => ApiError::unavailable(
            "facilitator_unavailable",
            "supported payment methods are temporarily unavailable",
        )
        .into_response(),
    }
}

async fn verify(State(state): State<AppState>, request: Request) -> Response {
    let started = Instant::now();
    let deadline = Duration::from_secs(state.config.request_limits.verify_timeout_seconds);
    let response = match tokio::time::timeout(deadline, verify_inner(&state, request)).await {
        Ok(response) => response,
        Err(_) => ApiError::unavailable("verification_timeout", "NEAR verification timed out")
            .into_response(),
    };
    state.metrics.record_request(
        "verify",
        if response.status().is_success() {
            "completed"
        } else {
            "rejected"
        },
        started.elapsed().as_secs_f64(),
    );
    response
}

async fn verify_inner(state: &AppState, request: Request) -> Response {
    let authenticated = match authenticate(state, request.headers()).await {
        Ok(authenticated) => authenticated,
        Err(error) => return error.into_response(),
    };
    let client = authenticated.client;
    if !state
        .rates
        .check(
            &authenticated.key_prefix,
            Operation::Verify,
            client
                .verify_rate_per_minute
                .min(state.config.request_limits.verify_per_minute),
        )
        .await
    {
        return ApiError::rate_limited("rate_limit_exceeded", "verification rate limit exceeded")
            .into_response();
    }
    let parsed = match read_and_parse(state, request).await {
        Ok(parsed) => parsed,
        Err(error) => return error.into_response(),
    };
    if let Some(response) = static_verify_failure(state, &parsed) {
        return protocol_json(StatusCode::OK, &response);
    }
    match state
        .store
        .payee_allowed(
            client.id,
            &parsed.meta.network,
            &parsed.meta.asset,
            &parsed.meta.pay_to,
        )
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return protocol_json(
                StatusCode::OK,
                &VerifyResponse::invalid("payee_not_allowed", None, None),
            );
        }
        Err(_) => {
            return ApiError::unavailable(
                "database_unavailable",
                "verification policy is temporarily unavailable",
            )
            .into_response();
        }
    }

    let Ok(permit) = state.verify_slots.clone().try_acquire_owned() else {
        return ApiError::unavailable(
            "verification_capacity_exhausted",
            "verification capacity is temporarily exhausted",
        )
        .into_response();
    };
    let deadline = Duration::from_secs(state.config.request_limits.verify_timeout_seconds);
    let result = tokio::time::timeout(deadline, state.facilitator.verify(&parsed.raw)).await;
    drop(permit);
    match result {
        Ok(Ok(response)) => {
            if response_is_rpc_ambiguous(&response.0) {
                ApiError::unavailable(
                    "rpc_unavailable",
                    "NEAR verification is temporarily unavailable",
                )
                .into_response()
            } else {
                axum::Json(response.0).into_response()
            }
        }
        Ok(Err(_)) | Err(_) => ApiError::unavailable(
            "verification_unavailable",
            "NEAR verification is temporarily unavailable",
        )
        .into_response(),
    }
}

async fn settle(State(state): State<AppState>, request: Request) -> Response {
    let started = Instant::now();
    let deadline = Duration::from_secs(state.config.request_limits.settle_timeout_seconds);
    let response = match tokio::time::timeout(deadline, settle_inner(&state, request)).await {
        Ok(response) => response,
        Err(_) => ApiError::unavailable(
            "settlement_pending",
            "settlement is still pending; retry with the same payment identifier",
        )
        .into_response(),
    };
    state.metrics.record_request(
        "settle",
        if response.status().is_success() {
            "completed"
        } else {
            "rejected"
        },
        started.elapsed().as_secs_f64(),
    );
    response
}

// The ordered HTTP flow mirrors the security boundary from authentication
// through durable claim creation and terminal replay.
#[allow(clippy::too_many_lines)]
async fn settle_inner(state: &AppState, request: Request) -> Response {
    let authenticated = match authenticate(state, request.headers()).await {
        Ok(authenticated) => authenticated,
        Err(error) => return error.into_response(),
    };
    let client = authenticated.client;
    if !state
        .rates
        .check(
            &authenticated.key_prefix,
            Operation::Settle,
            client
                .settle_rate_per_minute
                .min(state.config.request_limits.settle_per_minute),
        )
        .await
    {
        return ApiError::rate_limited("rate_limit_exceeded", "settlement rate limit exceeded")
            .into_response();
    }
    let parsed = match read_and_parse(state, request).await {
        Ok(parsed) => parsed,
        Err(error) => return error.into_response(),
    };
    let decoded = match decode_signed_delegate(&parsed.meta.signed_delegate_action) {
        Ok(decoded) => decoded,
        Err(failure) => {
            return protocol_json(
                StatusCode::OK,
                &SettleResponse::failure(
                    failure.reason(),
                    None,
                    None,
                    String::new(),
                    state.config.network.clone(),
                ),
            );
        }
    };
    if !decoded.signed_delegate.verify() {
        return protocol_json(
            StatusCode::OK,
            &SettleResponse::failure(
                VerificationFailure::InvalidSignature.reason(),
                None,
                None,
                String::new(),
                state.config.network.clone(),
            ),
        );
    }
    let Ok(fingerprint) = request_fingerprint(&parsed.value, &decoded.payment_hash) else {
        return ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_json",
            "request JSON cannot be canonicalized",
        )
        .into_response();
    };
    match prior_settlement_response(
        state,
        client.id,
        parsed.meta.payment_identifier.as_deref(),
        &decoded.payment_hash,
        &fingerprint,
    )
    .await
    {
        Ok(Some(response)) => return response,
        Ok(None) => {}
        Err(_) => {
            return ApiError::unavailable(
                "database_unavailable",
                "settlement journal is temporarily unavailable",
            )
            .into_response();
        }
    }
    if let Some(response) = static_settle_failure(state, &parsed) {
        return protocol_json(StatusCode::OK, &response);
    }
    if !state.readiness.can_settle() {
        if let Some(response) = prior_settlement_race_response(
            state,
            client.id,
            parsed.meta.payment_identifier.as_deref(),
            &decoded.payment_hash,
            &fingerprint,
        )
        .await
        {
            return response;
        }
        return ApiError::unavailable(
            "settlement_unavailable",
            "settlement is temporarily unavailable",
        )
        .into_response();
    }
    match state
        .store
        .payee_allowed(
            client.id,
            &parsed.meta.network,
            &parsed.meta.asset,
            &parsed.meta.pay_to,
        )
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            if let Some(response) = prior_settlement_race_response(
                state,
                client.id,
                parsed.meta.payment_identifier.as_deref(),
                &decoded.payment_hash,
                &fingerprint,
            )
            .await
            {
                return response;
            }
            return protocol_json(
                StatusCode::OK,
                &SettleResponse::failure(
                    "payee_not_allowed",
                    None,
                    None,
                    String::new(),
                    state.config.network.clone(),
                ),
            );
        }
        Err(_) => {
            return ApiError::unavailable(
                "database_unavailable",
                "settlement policy is temporarily unavailable",
            )
            .into_response();
        }
    }

    // Route through the registered x402-rs scheme before exposing the
    // chain-specific VerifiedPayment needed by the durable journal.
    let routed = state.facilitator.verify(&parsed.raw).await;
    let Ok(routed) = routed else {
        if let Some(response) = prior_settlement_race_response(
            state,
            client.id,
            parsed.meta.payment_identifier.as_deref(),
            &decoded.payment_hash,
            &fingerprint,
        )
        .await
        {
            return response;
        }
        return ApiError::unavailable(
            "verification_unavailable",
            "NEAR verification is temporarily unavailable",
        )
        .into_response();
    };
    if !routed
        .0
        .get("isValid")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        if let Some(response) = prior_settlement_race_response(
            state,
            client.id,
            parsed.meta.payment_identifier.as_deref(),
            &decoded.payment_hash,
            &fingerprint,
        )
        .await
        {
            return response;
        }
        if response_is_rpc_ambiguous(&routed.0) {
            return ApiError::unavailable(
                "rpc_unavailable",
                "NEAR verification is temporarily unavailable",
            )
            .into_response();
        }
        return settle_from_verify_failure(&routed.0, &state.config.network).into_response();
    }
    let policy = VerificationPolicy {
        max_sponsored_gas: state.config.max_inner_gas,
    };
    let verified = match state.provider.verify(&parsed.raw, &policy).await {
        Ok(verified) => verified,
        Err(failure) if verification_is_rpc_ambiguous(failure) => {
            if let Some(response) = prior_settlement_race_response(
                state,
                client.id,
                parsed.meta.payment_identifier.as_deref(),
                &decoded.payment_hash,
                &fingerprint,
            )
            .await
            {
                return response;
            }
            return ApiError::unavailable(
                "rpc_unavailable",
                "NEAR verification is temporarily unavailable",
            )
            .into_response();
        }
        Err(failure) => {
            if let Some(response) = prior_settlement_race_response(
                state,
                client.id,
                parsed.meta.payment_identifier.as_deref(),
                &decoded.payment_hash,
                &fingerprint,
            )
            .await
            {
                return response;
            }
            return protocol_json(
                StatusCode::OK,
                &SettleResponse::failure(
                    failure.reason(),
                    None,
                    None,
                    String::new(),
                    state.config.network.clone(),
                ),
            );
        }
    };
    if verified.payment_hash() != &decoded.payment_hash {
        return ApiError::unavailable(
            "verification_inconsistent",
            "payment verification was internally inconsistent",
        )
        .into_response();
    }
    let new = NewSettlement {
        id: Uuid::new_v4(),
        api_client_id: client.id,
        payment_identifier: parsed.meta.payment_identifier.clone(),
        payment_hash: *verified.payment_hash(),
        request_fingerprint: fingerprint,
        x402_version: parsed.meta.x402_version,
        scheme: parsed.meta.scheme.clone(),
        network: parsed.meta.network.clone(),
        asset: parsed.meta.asset.clone(),
        pay_to: parsed.meta.pay_to.clone(),
        amount: parsed.meta.amount.clone(),
        payer: verified.payer.to_string(),
        delegate_public_key: verified.payer_public_key.to_string(),
        delegate_nonce: verified.delegate_nonce.to_string(),
        delegate_max_block_height: verified.max_block_height.to_string(),
        policy_snapshot: state.config.policy_snapshot(),
        reservation_yocto_near: state.config.sponsorship.reservation_yocto_near.clone(),
        global_daily_budget_yocto_near: state.config.sponsorship.global_daily_yocto_near.clone(),
        client_daily_budget_yocto_near: client.daily_budget_yocto_near.clone(),
    };
    let Ok(claim) = state.store.claim_settlement(&new).await else {
        return ApiError::unavailable(
            "database_unavailable",
            "settlement journal is temporarily unavailable",
        )
        .into_response();
    };
    let settlement_id = match claim {
        ClaimOutcome::New(record) => {
            let worker_state = state.clone();
            let raw_request = parsed.raw.clone();
            let worker_span = tracing::info_span!(
                "settlement_worker",
                network = %state.config.network,
                version = VERSION
            );
            tokio::spawn(
                async move {
                    run_new_settlement(worker_state, record.id, raw_request).await;
                }
                .instrument(worker_span),
            );
            record.id
        }
        ClaimOutcome::Existing(record) => {
            state.metrics.record_idempotency_replay();
            if record.state.is_terminal() {
                return stored_terminal_response(&record);
            }
            record.id
        }
        ClaimOutcome::IdentifierConflict => {
            return ApiError::new(
                StatusCode::CONFLICT,
                "payment_identifier_conflict",
                "payment identifier was already used for another request",
            )
            .into_response();
        }
        ClaimOutcome::DuplicateSettlement => {
            return protocol_json(
                StatusCode::OK,
                &SettleResponse::failure(
                    "duplicate_settlement",
                    None,
                    Some(verified.payer.to_string()),
                    String::new(),
                    state.config.network.clone(),
                ),
            );
        }
        ClaimOutcome::BudgetExceeded => {
            return ApiError::rate_limited(
                "sponsorship_budget_exhausted",
                "sponsorship budget is exhausted",
            )
            .into_response();
        }
    };

    let deadline = Duration::from_secs(state.config.request_limits.settle_timeout_seconds);
    match tokio::time::timeout(deadline, wait_for_terminal(&state.store, settlement_id)).await {
        Ok(Ok(record)) => stored_terminal_response(&record),
        Ok(Err(_)) => ApiError::unavailable(
            "database_unavailable",
            "settlement journal is temporarily unavailable",
        )
        .into_response(),
        Err(_) => ApiError::unavailable(
            "settlement_pending",
            "settlement is still pending; retry with the same payment identifier",
        )
        .into_response(),
    }
}

async fn prior_settlement_response(
    state: &AppState,
    api_client_id: Uuid,
    payment_identifier: Option<&str>,
    payment_hash: &[u8; 32],
    request_fingerprint: &[u8; 32],
) -> Result<Option<Response>, StoreError> {
    let Some(claim) = state
        .store
        .find_existing_settlement(
            api_client_id,
            payment_identifier,
            payment_hash,
            request_fingerprint,
        )
        .await?
    else {
        return Ok(None);
    };
    let response = match claim {
        ClaimOutcome::Existing(record) => {
            state.metrics.record_idempotency_replay();
            if record.state.is_terminal() {
                stored_terminal_response(&record)
            } else {
                let deadline =
                    Duration::from_secs(state.config.request_limits.settle_timeout_seconds);
                match tokio::time::timeout(deadline, wait_for_terminal(&state.store, record.id))
                    .await
                {
                    Ok(Ok(record)) => stored_terminal_response(&record),
                    Ok(Err(error)) => return Err(error),
                    Err(_) => ApiError::unavailable(
                        "settlement_pending",
                        "settlement is still pending; retry with the same payment identifier",
                    )
                    .into_response(),
                }
            }
        }
        ClaimOutcome::IdentifierConflict => ApiError::new(
            StatusCode::CONFLICT,
            "payment_identifier_conflict",
            "payment identifier was already used for another request",
        )
        .into_response(),
        ClaimOutcome::DuplicateSettlement => protocol_json(
            StatusCode::OK,
            &SettleResponse::failure(
                "duplicate_settlement",
                None,
                None,
                String::new(),
                state.config.network.clone(),
            ),
        ),
        ClaimOutcome::New(_) | ClaimOutcome::BudgetExceeded => {
            return Err(StoreError::Corrupt(
                "existing-settlement lookup returned a non-existing outcome".to_owned(),
            ));
        }
    };
    Ok(Some(response))
}

async fn prior_settlement_race_response(
    state: &AppState,
    api_client_id: Uuid,
    payment_identifier: Option<&str>,
    payment_hash: &[u8; 32],
    request_fingerprint: &[u8; 32],
) -> Option<Response> {
    match prior_settlement_response(
        state,
        api_client_id,
        payment_identifier,
        payment_hash,
        request_fingerprint,
    )
    .await
    {
        Ok(response) => response,
        Err(_) => Some(
            ApiError::unavailable(
                "database_unavailable",
                "settlement journal is temporarily unavailable",
            )
            .into_response(),
        ),
    }
}

async fn authenticate(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<AuthenticatedClient, ApiError> {
    match state.auth.authenticate(headers).await {
        Ok(authenticated) => {
            let store = state.store.clone();
            let prefix = authenticated.key_prefix.clone();
            tokio::spawn(async move {
                let _result = store.touch_api_key(&prefix).await;
            });
            Ok(authenticated)
        }
        Err(AuthError::Invalid | AuthError::Configuration) => Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid_api_key",
            "missing or invalid API key",
        )),
        Err(AuthError::Store(_) | AuthError::Entropy) => Err(ApiError::unavailable(
            "authentication_unavailable",
            "authentication is temporarily unavailable",
        )),
    }
}

async fn read_and_parse(state: &AppState, request: Request) -> Result<ParsedRequest, ApiError> {
    ensure_json(request.headers())?;
    let bytes = to_bytes(request.into_body(), state.config.request_limits.body_bytes)
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request_too_large",
                "request body exceeds 64 KiB",
            )
        })?;
    parse_request(&bytes, &state.config.payment_identifier).map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "malformed_request",
            "request body is not a canonical x402 request",
        )
    })
}

fn ensure_json(headers: &HeaderMap) -> Result<(), ApiError> {
    let Some(content_type) = headers.get(CONTENT_TYPE) else {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            "Content-Type must be application/json",
        ));
    };
    let Ok(content_type) = content_type.to_str() else {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            "Content-Type must be application/json",
        ));
    };
    if content_type
        .split(';')
        .next()
        .is_none_or(|value| !value.trim().eq_ignore_ascii_case("application/json"))
    {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            "Content-Type must be application/json",
        ));
    }
    Ok(())
}

fn static_verify_failure(state: &AppState, request: &ParsedRequest) -> Option<VerifyResponse> {
    static_failure_reason(state, request).map(|reason| VerifyResponse::invalid(reason, None, None))
}

fn static_settle_failure(state: &AppState, request: &ParsedRequest) -> Option<SettleResponse> {
    static_failure_reason(state, request).map(|reason| {
        SettleResponse::failure(
            reason,
            None,
            None,
            String::new(),
            state.config.network.clone(),
        )
    })
}

fn static_failure_reason(state: &AppState, request: &ParsedRequest) -> Option<&'static str> {
    if request.meta.x402_version != 2 {
        Some("invalid_x402_version")
    } else if request.meta.scheme != "exact" {
        Some("unsupported_scheme")
    } else if request.meta.network != state.config.network {
        Some("invalid_network")
    } else if request.meta.asset != state.config.asset {
        Some("invalid_asset")
    } else if !decimal_is_at_least(&request.meta.amount, &state.config.minimum_amount) {
        Some("amount_below_minimum")
    } else {
        None
    }
}

fn settle_from_verify_failure(value: &Value, network: &str) -> Response {
    let reason = value
        .get("invalidReason")
        .and_then(Value::as_str)
        .unwrap_or("invalid_payment");
    let payer = value
        .get("payer")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    protocol_json(
        StatusCode::OK,
        &SettleResponse::failure(reason, None, payer, String::new(), network.to_owned()),
    )
}

fn response_is_rpc_ambiguous(value: &Value) -> bool {
    value
        .get("invalidReason")
        .and_then(Value::as_str)
        .is_some_and(|reason| {
            matches!(
                reason,
                "invalid_exact_near_current_block_height_unavailable"
                    | "invalid_exact_near_access_key_lookup_failed"
                    | "invalid_exact_near_account_lookup_failed"
                    | "invalid_exact_near_token_account_lookup_failed"
                    | "invalid_exact_near_balance_check_failed"
                    | "invalid_exact_near_storage_check_failed"
            )
        })
}

const fn verification_is_rpc_ambiguous(failure: VerificationFailure) -> bool {
    matches!(
        failure,
        VerificationFailure::CurrentBlockHeightUnavailable
            | VerificationFailure::AccessKeyLookupFailed
            | VerificationFailure::AccountLookupFailed
            | VerificationFailure::TokenAccountLookupFailed
            | VerificationFailure::BalanceCheckFailed
            | VerificationFailure::StorageCheckFailed
    )
}

fn protocol_json<T: Serialize>(status: StatusCode, body: &T) -> Response {
    match serde_json::to_vec(body) {
        Ok(bytes) => raw_json(status, bytes),
        Err(_) => ApiError::unavailable(
            "response_serialization_failed",
            "response serialization failed",
        )
        .into_response(),
    }
}

fn raw_json(status: StatusCode, bytes: Vec<u8>) -> Response {
    let mut response = Response::new(Body::from(bytes));
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
}

fn stored_terminal_response(record: &SettlementRecord) -> Response {
    let status = record
        .terminal_http_status
        .and_then(|status| StatusCode::from_u16(status).ok())
        .unwrap_or(StatusCode::SERVICE_UNAVAILABLE);
    let bytes = record.terminal_response_bytes.clone().unwrap_or_else(|| {
        service_error_bytes(
            "journal_incomplete",
            "terminal settlement response is unavailable",
        )
    });
    let mut response = raw_json(status, bytes);
    if status == StatusCode::SERVICE_UNAVAILABLE {
        response
            .headers_mut()
            .insert(RETRY_AFTER, HeaderValue::from_static(RETRY_SECONDS));
    }
    response
}

async fn wait_for_terminal(store: &PgStore, id: Uuid) -> Result<SettlementRecord, StoreError> {
    let mut interval = tokio::time::interval(Duration::from_millis(100));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        let record = store
            .settlement(id)
            .await?
            .ok_or_else(|| StoreError::Corrupt("settlement disappeared".to_owned()))?;
        if record.state.is_terminal() {
            return Ok(record);
        }
    }
}

// Settlement deliberately remains one sequence so the revalidation, journal,
// and broadcast ordering can be reviewed without cross-function gaps.
#[allow(clippy::too_many_lines)]
async fn run_new_settlement(
    state: AppState,
    settlement_id: Uuid,
    request: x402_types::proto::VerifyRequest,
) {
    let _relayer_guard = state.relayer_lock.lock().await;
    if !state.readiness.can_settle() {
        terminal_service_failure(
            &state,
            settlement_id,
            "leadership_unavailable",
            "settlement leadership was lost before transaction preparation",
        )
        .await;
        return;
    }
    let policy = VerificationPolicy {
        max_sponsored_gas: state.config.max_inner_gas,
    };
    let payment = match state.provider.verify(&request, &policy).await {
        Ok(payment) => payment,
        Err(failure) if verification_is_rpc_ambiguous(failure) => {
            terminal_service_failure(
                &state,
                settlement_id,
                "rpc_unavailable",
                "NEAR verification was unavailable before transaction preparation",
            )
            .await;
            return;
        }
        Err(failure) => {
            terminal_protocol_failure(&state, settlement_id, failure.reason(), None, None).await;
            return;
        }
    };
    let Ok(relayer_status) = fresh_relayer_status(&state).await else {
        terminal_service_failure(
            &state,
            settlement_id,
            "relayer_unavailable",
            "relayer policy, balance, or chain state is unavailable",
        )
        .await;
        return;
    };
    let relayer_head = RelayerHead {
        block_height: relayer_status.block_height,
        block_hash: relayer_status.block_hash,
        access_key_nonce: relayer_status.access_key_nonce,
    };
    let Ok(prepared) = state
        .provider
        .prepare_outer_transaction(&payment, relayer_head)
    else {
        terminal_service_failure(
            &state,
            settlement_id,
            "transaction_preparation_failed",
            "outer transaction could not be prepared",
        )
        .await;
        return;
    };
    let journal = PreparedJournalEntry {
        settlement_id,
        relayer_account_id: prepared.signer_id.to_string(),
        relayer_public_key: prepared.signer_public_key.to_string(),
        relayer_nonce: prepared.relayer_nonce.to_string(),
        transaction_bytes: prepared.signed_transaction_bytes().to_vec(),
        transaction_hash: prepared.transaction_hash.to_string(),
    };
    if state.store.mark_prepared(&journal).await.is_err() {
        state.readiness.set_reconciliation(false);
        tracing::error!(event = "settlement_prepare_journal_failed");
        return;
    }

    let Ok(current_relayer) = fresh_relayer_status(&state).await else {
        state.readiness.set_reconciliation(false);
        tracing::warn!(event = "settlement_paused_after_relayer_recheck");
        return;
    };
    if current_relayer.access_key_nonce != relayer_status.access_key_nonce {
        let public_key = state.provider.relayer_public_key().to_string();
        let _quarantine = state
            .store
            .quarantine_relayer(
                &state.config.network,
                &state.config.relayer_account_id,
                &public_key,
                "relayer nonce changed between preparation and broadcast",
                &current_relayer.access_key_nonce.to_string(),
            )
            .await;
        state.readiness.set_relayer(false);
        state.readiness.set_reconciliation(false);
        tracing::error!(event = "settlement_relayer_nonce_changed_before_broadcast");
        return;
    }

    // A prepared transaction is durable from this point.  Any leadership loss
    // leaves it for reconciliation; it must never be replaced with new bytes.
    if !state.readiness.can_settle() {
        state.readiness.set_reconciliation(false);
        tracing::warn!(event = "settlement_paused_after_prepare");
        return;
    }
    if state.store.mark_submitted(settlement_id).await.is_err() {
        state.readiness.set_reconciliation(false);
        tracing::error!(event = "settlement_submit_journal_failed");
        return;
    }
    // Recheck immediately before the external side effect, after the durable
    // state transition.  This is deliberately adjacent to broadcast.
    if !state.readiness.can_settle() {
        state.readiness.set_reconciliation(false);
        tracing::warn!(event = "settlement_paused_before_broadcast");
        return;
    }
    let lookup = state
        .provider
        .broadcast_exact(prepared.signed_transaction_bytes())
        .await;
    match lookup {
        Ok(TransactionLookup::Final(outcome)) => {
            finalize_outcome(
                &state,
                settlement_id,
                &payment,
                prepared.transaction_hash,
                &outcome,
            )
            .await;
        }
        Err(NearRpcError::TransactionRejected) => {
            terminal_transaction_rejected(
                &state,
                settlement_id,
                Some(payment.payer.to_string()),
                prepared.transaction_hash,
            )
            .await;
        }
        Ok(TransactionLookup::Pending(_) | TransactionLookup::Unknown) | Err(_) => {
            // Indeterminate: exact bytes/hash stay submitted for reconciliation.
            state.readiness.set_reconciliation(false);
            tracing::warn!(event = "settlement_broadcast_indeterminate");
        }
    }
}

async fn fresh_relayer_status(state: &AppState) -> Result<RelayerStatus, StoreError> {
    let status = state.provider.relayer_status().await.map_err(|_| {
        state.readiness.set_relayer(false);
        StoreError::Corrupt("relayer chain state is unavailable".to_owned())
    })?;
    let public_key = state.provider.relayer_public_key().to_string();
    let policy_active = state
        .store
        .relayer_is_active(
            &state.config.network,
            &state.config.relayer_account_id,
            &public_key,
        )
        .await?;
    let funded = decimal_is_at_least(
        &status.account.amount.as_yoctonear().to_string(),
        &state.config.sponsorship.balance_hard_stop_yocto_near,
    );
    if !policy_active || !funded {
        state.readiness.set_relayer(false);
        return Err(StoreError::Corrupt(
            "relayer policy or balance hard stop is not satisfied".to_owned(),
        ));
    }
    state.readiness.set_relayer(true);
    Ok(status)
}

async fn finalize_outcome(
    state: &AppState,
    settlement_id: Uuid,
    payment: &VerifiedPayment,
    transaction_hash: CryptoHash,
    outcome: &FinalExecutionOutcomeView,
) {
    if let Err(error) = validate_final_outcome_identity(
        outcome,
        transaction_hash,
        &state.provider.relayer_account_id(),
        &payment.payer,
    ) {
        state.readiness.set_reconciliation(false);
        tracing::warn!(
            event = "settlement_outcome_identity_indeterminate",
            reason = %error
        );
        return;
    }
    let (gas_burnt, tokens_burnt) = execution_cost(outcome);
    let transaction = transaction_hash.to_string();
    let (terminal_state, response, error_code) =
        match interpret_final_outcome(outcome, &payment.payer, &payment.requirements.asset) {
            Ok(_) => (
                SettlementState::Succeeded,
                SettleResponse::success(
                    payment.payer.to_string(),
                    transaction,
                    state.config.network.clone(),
                ),
                None,
            ),
            Err(error) if error.is_definitive_failure() => (
                SettlementState::Failed,
                SettleResponse::failure(
                    "transaction_failed",
                    Some(error.to_string()),
                    Some(payment.payer.to_string()),
                    transaction,
                    state.config.network.clone(),
                ),
                Some("transaction_failed".to_owned()),
            ),
            Err(error) => {
                state.readiness.set_reconciliation(false);
                tracing::warn!(
                    event = "settlement_receipt_indeterminate",
                    reason = %error
                );
                return;
            }
        };
    let (metric_result, metric_reason) = match terminal_state {
        SettlementState::Succeeded => ("succeeded", "success"),
        SettlementState::Failed => ("failed", "transaction_failed"),
        SettlementState::Reserved | SettlementState::Prepared | SettlementState::Submitted => {
            ("failed", "invalid_terminal_state")
        }
    };
    let Ok(bytes) = serde_json::to_vec(&response) else {
        tracing::error!(event = "terminal_response_serialization_failed");
        return;
    };
    let entry = TerminalJournalEntry {
        settlement_id,
        state: terminal_state,
        http_status: StatusCode::OK.as_u16(),
        response_bytes: bytes,
        error_code,
        error_detail: None,
        gas_burnt: Some(gas_burnt.to_string()),
        tokens_burnt: Some(tokens_burnt.to_string()),
        actual_yocto_near: tokens_burnt.to_string(),
    };
    if state.store.mark_terminal(&entry).await.is_err() {
        state.readiness.set_reconciliation(false);
        tracing::error!(event = "terminal_journal_failed");
    } else {
        state
            .metrics
            .record_settlement_cost(gas_burnt, yocto_near_metric(tokens_burnt));
        state
            .metrics
            .record_settlement_result(metric_result, metric_reason);
        tracing::info!(
            event = "settlement_terminal",
            result = metric_result,
            reason = metric_reason
        );
    }
}

fn execution_cost(outcome: &FinalExecutionOutcomeView) -> (u64, u128) {
    let mut gas = outcome.transaction_outcome.outcome.gas_burnt.as_gas();
    let mut tokens = outcome
        .transaction_outcome
        .outcome
        .tokens_burnt
        .as_yoctonear();
    for receipt in &outcome.receipts_outcome {
        gas = gas.saturating_add(receipt.outcome.gas_burnt.as_gas());
        tokens = tokens.saturating_add(receipt.outcome.tokens_burnt.as_yoctonear());
    }
    (gas, tokens)
}

fn yocto_near_metric(value: u128) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(f64::MAX)
}

async fn terminal_protocol_failure(
    state: &AppState,
    settlement_id: Uuid,
    reason: &'static str,
    payer: Option<String>,
    transaction: Option<String>,
) {
    let response = SettleResponse::failure(
        reason,
        None,
        payer,
        transaction.unwrap_or_default(),
        state.config.network.clone(),
    );
    let Ok(bytes) = serde_json::to_vec(&response) else {
        return;
    };
    let result = state
        .store
        .mark_terminal(&TerminalJournalEntry {
            settlement_id,
            state: SettlementState::Failed,
            http_status: StatusCode::OK.as_u16(),
            response_bytes: bytes,
            error_code: Some(reason.to_owned()),
            error_detail: None,
            gas_burnt: Some("0".to_owned()),
            tokens_burnt: Some("0".to_owned()),
            actual_yocto_near: "0".to_owned(),
        })
        .await;
    if result.is_ok() {
        state.metrics.record_settlement_result("failed", reason);
        tracing::info!(event = "settlement_terminal", result = "failed", reason);
    } else {
        state.readiness.set_reconciliation(false);
        tracing::error!(event = "terminal_journal_failed");
    }
}

async fn terminal_service_failure(
    state: &AppState,
    settlement_id: Uuid,
    code: &'static str,
    message: &'static str,
) {
    let result = state
        .store
        .mark_terminal(&TerminalJournalEntry {
            settlement_id,
            state: SettlementState::Failed,
            http_status: StatusCode::SERVICE_UNAVAILABLE.as_u16(),
            response_bytes: service_error_bytes(code, message),
            error_code: Some(code.to_owned()),
            error_detail: None,
            gas_burnt: Some("0".to_owned()),
            tokens_burnt: Some("0".to_owned()),
            actual_yocto_near: "0".to_owned(),
        })
        .await;
    if result.is_ok() {
        state.metrics.record_settlement_result("failed", code);
        tracing::info!(
            event = "settlement_terminal",
            result = "failed",
            reason = code
        );
    } else {
        state.readiness.set_reconciliation(false);
        tracing::error!(event = "terminal_journal_failed");
    }
}

async fn terminal_transaction_rejected(
    state: &AppState,
    settlement_id: Uuid,
    payer: Option<String>,
    transaction_hash: CryptoHash,
) {
    let response = SettleResponse::failure(
        "transaction_rejected",
        None,
        payer,
        transaction_hash.to_string(),
        state.config.network.clone(),
    );
    let Ok(response_bytes) = serde_json::to_vec(&response) else {
        state.readiness.set_reconciliation(false);
        return;
    };
    let result = state
        .store
        .mark_terminal(&TerminalJournalEntry {
            settlement_id,
            state: SettlementState::Failed,
            http_status: StatusCode::OK.as_u16(),
            response_bytes,
            error_code: Some("transaction_rejected".to_owned()),
            error_detail: None,
            gas_burnt: Some("0".to_owned()),
            tokens_burnt: Some("0".to_owned()),
            actual_yocto_near: "0".to_owned(),
        })
        .await;
    if result.is_ok() {
        state
            .metrics
            .record_settlement_result("failed", "transaction_rejected");
        tracing::info!(
            event = "settlement_terminal",
            result = "failed",
            reason = "transaction_rejected"
        );
    } else {
        state.readiness.set_reconciliation(false);
        tracing::error!(event = "terminal_journal_failed");
    }
}

fn service_error_bytes(code: &str, message: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "error": {
            "code": code,
            "message": message,
        }
    }))
    .unwrap_or_else(|_| {
        b"{\"error\":{\"code\":\"internal_error\",\"message\":\"internal error\"}}".to_vec()
    })
}

/// Reconcile nonterminal rows after leadership acquisition.  This function
/// never signs replacement bytes: prepared/submitted rows are queried and, if
/// safe, rebroadcast using only the exact journaled transaction.
pub async fn reconcile(state: &AppState) -> Result<(), StoreError> {
    if !state.readiness.snapshot().leadership {
        return Err(StoreError::Corrupt(
            "reconciliation requires leadership".to_owned(),
        ));
    }
    state.readiness.set_reconciliation(false);
    let _relayer_guard = state.relayer_lock.lock().await;
    let records = state.store.nonterminal_settlements().await?;
    state
        .metrics
        .record_pending_settlements(u64::try_from(records.len()).unwrap_or(u64::MAX));
    for record in records {
        state.store.note_reconciliation(record.id).await?;
        match record.state {
            SettlementState::Reserved => {
                // The service is not ready while startup reconciliation runs,
                // therefore every observed reserved row predates this leader.
                // With no prepared bytes there can have been no broadcast.
                terminal_service_failure(
                    state,
                    record.id,
                    "recovered_before_prepare",
                    "an interrupted settlement was released before transaction preparation",
                )
                .await;
            }
            SettlementState::Prepared | SettlementState::Submitted => {
                reconcile_prepared(state, &record).await?;
                let remains_nonterminal = state
                    .store
                    .settlement(record.id)
                    .await?
                    .is_some_and(|current| !current.state.is_terminal());
                if remains_nonterminal {
                    break;
                }
            }
            SettlementState::Succeeded | SettlementState::Failed => {}
        }
    }
    let remaining = state.store.nonterminal_settlements().await?;
    state
        .metrics
        .record_pending_settlements(u64::try_from(remaining.len()).unwrap_or(u64::MAX));
    state.readiness.set_reconciliation(remaining.is_empty());
    Ok(())
}

// Recovery keeps every exact-byte/hash and dual-RPC decision adjacent.
#[allow(clippy::too_many_lines)]
async fn reconcile_prepared(state: &AppState, record: &SettlementRecord) -> Result<(), StoreError> {
    let expected_account = state.provider.relayer_account_id().to_string();
    let expected_public_key = state.provider.relayer_public_key().to_string();
    if record.relayer_account_id.as_deref() != Some(expected_account.as_str())
        || record.relayer_public_key.as_deref() != Some(expected_public_key.as_str())
    {
        state.readiness.set_relayer(false);
        return Err(StoreError::Corrupt(
            "journaled relayer identity does not match configured relayer".to_owned(),
        ));
    }
    let hash = record
        .outer_transaction_hash
        .as_deref()
        .ok_or_else(|| StoreError::Corrupt("prepared row has no transaction hash".to_owned()))?
        .parse::<CryptoHash>()
        .map_err(|_| StoreError::Corrupt("prepared row has invalid transaction hash".to_owned()))?;
    // Validate the exact persisted bytes before trusting *any* RPC result for
    // this journal row, including an already-final transaction.
    let bytes = record
        .outer_transaction_bytes
        .as_deref()
        .ok_or_else(|| StoreError::Corrupt("prepared row has no transaction bytes".to_owned()))?;
    let signer = record
        .relayer_account_id
        .as_deref()
        .ok_or_else(|| StoreError::Corrupt("prepared row has no signer".to_owned()))?
        .parse::<AccountId>()
        .map_err(|_| StoreError::Corrupt("prepared row has invalid signer".to_owned()))?;
    validate_stored_transaction(record, bytes, hash, &signer).inspect_err(|_| {
        state.readiness.set_relayer(false);
    })?;
    let primary = state.provider.query_transaction(hash, signer.clone()).await;
    let backup = state
        .provider
        .query_transaction_backup(hash, signer.clone())
        .await;
    let primary_final = final_outcome(&primary);
    let backup_final = final_outcome(&backup);
    if final_outcomes_conflict(primary_final, backup_final) {
        state.readiness.set_reconciliation(false);
        return Err(StoreError::Corrupt(
            "primary and backup RPCs returned conflicting final outcomes".to_owned(),
        ));
    }
    let outcome = primary_final.or(backup_final);
    if primary_final.is_none() && backup_final.is_some() {
        state.metrics.record_rpc_failover("reconcile_transaction");
    }
    if let Some(outcome) = outcome {
        let payer = record
            .payer
            .parse::<AccountId>()
            .map_err(|_| StoreError::Corrupt("journal payer is invalid".to_owned()))?;
        let asset = record
            .asset
            .parse::<AccountId>()
            .map_err(|_| StoreError::Corrupt("journal asset is invalid".to_owned()))?;
        finalize_reconciled(state, record, outcome, &payer, &asset, hash).await?;
        return Ok(());
    }
    if [primary.as_ref(), backup.as_ref()]
        .into_iter()
        .any(|lookup| matches!(lookup, Ok(TransactionLookup::Pending(_))))
    {
        return Ok(());
    }
    let primary_unknown = lookup_is_unknown(&primary);
    let backup_unknown = lookup_is_unknown(&backup);
    if !primary_unknown || !backup_unknown {
        state.readiness.set_reconciliation(false);
        return Err(StoreError::Corrupt(
            "RPC ambiguity prevented settlement reconciliation".to_owned(),
        ));
    }

    let primary_status = fresh_relayer_status(state).await?;
    let backup_head = state.provider.backup_relayer_head().await.map_err(|_| {
        StoreError::Corrupt("backup relayer state unavailable during reconciliation".to_owned())
    })?;
    let prepared_nonce = record
        .relayer_nonce
        .as_deref()
        .and_then(|nonce| nonce.parse::<u64>().ok())
        .ok_or_else(|| StoreError::Corrupt("prepared row has invalid nonce".to_owned()))?;
    if primary_status.access_key_nonce >= prepared_nonce
        || backup_head.access_key_nonce >= prepared_nonce
    {
        let public_key = record
            .relayer_public_key
            .as_deref()
            .ok_or_else(|| StoreError::Corrupt("prepared row has no public key".to_owned()))?;
        state
            .store
            .quarantine_relayer(
                &state.config.network,
                &state.config.relayer_account_id,
                public_key,
                "nonce advanced while exact transaction remained unknown",
                &primary_status
                    .access_key_nonce
                    .max(backup_head.access_key_nonce)
                    .to_string(),
            )
            .await?;
        state.readiness.set_relayer(false);
        return Err(StoreError::Corrupt(
            "relayer key quarantined after unknown nonce advance".to_owned(),
        ));
    }
    let delegate_max_height = record
        .delegate_max_block_height
        .parse::<u64>()
        .map_err(|_| StoreError::Corrupt("journal delegate expiry is invalid".to_owned()))?;
    if primary_status.block_height.max(backup_head.block_height) >= delegate_max_height {
        terminal_protocol_failure(
            state,
            record.id,
            VerificationFailure::DelegateActionExpired.reason(),
            Some(record.payer.clone()),
            (record.state == SettlementState::Submitted).then(|| hash.to_string()),
        )
        .await;
        return Ok(());
    }
    if record.state == SettlementState::Prepared {
        state.store.mark_submitted(record.id).await?;
    }
    // Leadership is rechecked immediately before the rebroadcast side effect.
    if !can_reconciliation_broadcast(state) {
        return Err(StoreError::Corrupt(
            "leadership lost before reconciliation broadcast".to_owned(),
        ));
    }
    let current_primary = fresh_relayer_status(state).await?;
    let current_backup = state.provider.backup_relayer_head().await.map_err(|_| {
        StoreError::Corrupt("backup relayer state unavailable before rebroadcast".to_owned())
    })?;
    if current_primary.access_key_nonce != primary_status.access_key_nonce
        || current_backup.access_key_nonce != backup_head.access_key_nonce
    {
        state.readiness.set_relayer(false);
        return Err(StoreError::Corrupt(
            "relayer nonce changed before exact-byte rebroadcast".to_owned(),
        ));
    }
    match state.provider.broadcast_exact(bytes).await {
        Ok(TransactionLookup::Final(outcome)) => {
            let payer = record
                .payer
                .parse::<AccountId>()
                .map_err(|_| StoreError::Corrupt("journal payer is invalid".to_owned()))?;
            let asset = record
                .asset
                .parse::<AccountId>()
                .map_err(|_| StoreError::Corrupt("journal asset is invalid".to_owned()))?;
            finalize_reconciled(state, record, &outcome, &payer, &asset, hash).await?;
        }
        Err(NearRpcError::TransactionRejected) => {
            terminal_transaction_rejected(state, record.id, Some(record.payer.clone()), hash).await;
        }
        Ok(TransactionLookup::Pending(_) | TransactionLookup::Unknown) | Err(_) => {}
    }
    Ok(())
}

fn validate_stored_transaction(
    record: &SettlementRecord,
    bytes: &[u8],
    expected_hash: CryptoHash,
    expected_signer: &AccountId,
) -> Result<(), StoreError> {
    let signed = decode_signed_transaction(bytes)
        .map_err(|_| StoreError::Corrupt("prepared transaction bytes are invalid".to_owned()))?;
    if signed_transaction_hash(bytes).ok() != Some(expected_hash)
        || signed.get_hash() != expected_hash
    {
        return Err(StoreError::Corrupt(
            "prepared transaction bytes do not match journaled hash".to_owned(),
        ));
    }
    if !signed
        .signature
        .verify(signed.get_hash().as_ref(), signed.transaction.public_key())
    {
        return Err(StoreError::Corrupt(
            "prepared outer transaction signature is invalid".to_owned(),
        ));
    }
    let Transaction::V0(transaction) = &signed.transaction else {
        return Err(StoreError::Corrupt(
            "prepared outer transaction is not V0".to_owned(),
        ));
    };
    let expected_public_key = record
        .relayer_public_key
        .as_deref()
        .ok_or_else(|| StoreError::Corrupt("prepared row has no public key".to_owned()))?;
    let expected_nonce = record
        .relayer_nonce
        .as_deref()
        .and_then(|nonce| nonce.parse::<u64>().ok())
        .ok_or_else(|| StoreError::Corrupt("prepared row has invalid nonce".to_owned()))?;
    let payer = record
        .payer
        .parse::<AccountId>()
        .map_err(|_| StoreError::Corrupt("journal payer is invalid".to_owned()))?;
    if transaction.signer_id != *expected_signer
        || transaction.public_key.to_string() != expected_public_key
        || transaction.nonce != expected_nonce
        || transaction.receiver_id != payer
        || transaction.actions.len() != 1
    {
        return Err(StoreError::Corrupt(
            "prepared outer transaction identity does not match the journal".to_owned(),
        ));
    }
    let Some(Action::Delegate(delegate)) = transaction.actions.first() else {
        return Err(StoreError::Corrupt(
            "prepared outer transaction does not contain one delegate action".to_owned(),
        ));
    };
    if !delegate.verify() {
        return Err(StoreError::Corrupt(
            "prepared delegate signature is invalid".to_owned(),
        ));
    }
    let delegate_hash = signed_delegate_hash(delegate)
        .map_err(|_| StoreError::Corrupt("prepared delegate cannot be hashed".to_owned()))?;
    let expected_delegate_nonce = record
        .delegate_nonce
        .parse::<u64>()
        .map_err(|_| StoreError::Corrupt("journal delegate nonce is invalid".to_owned()))?;
    let expected_max_height = record
        .delegate_max_block_height
        .parse::<u64>()
        .map_err(|_| StoreError::Corrupt("journal delegate expiry is invalid".to_owned()))?;
    if delegate_hash != record.payment_hash
        || delegate.delegate_action.sender_id != payer
        || delegate.delegate_action.public_key.to_string() != record.delegate_public_key
        || delegate.delegate_action.nonce != expected_delegate_nonce
        || delegate.delegate_action.max_block_height != expected_max_height
    {
        return Err(StoreError::Corrupt(
            "prepared delegate does not match the settlement journal".to_owned(),
        ));
    }
    Ok(())
}

fn lookup_is_unknown(lookup: &Result<TransactionLookup, NearRpcError>) -> bool {
    matches!(
        lookup,
        Ok(TransactionLookup::Unknown) | Err(NearRpcError::TransactionUnknown)
    )
}

fn final_outcome(
    lookup: &Result<TransactionLookup, NearRpcError>,
) -> Option<&FinalExecutionOutcomeView> {
    match lookup {
        Ok(TransactionLookup::Final(outcome)) => Some(outcome.as_ref()),
        Ok(TransactionLookup::Unknown | TransactionLookup::Pending(_)) | Err(_) => None,
    }
}

fn final_outcomes_conflict<T: Eq>(primary: Option<&T>, backup: Option<&T>) -> bool {
    matches!((primary, backup), (Some(primary), Some(backup)) if primary != backup)
}

fn can_reconciliation_broadcast(state: &AppState) -> bool {
    let snapshot = state.readiness.snapshot();
    snapshot.leadership && snapshot.rpc && snapshot.relayer
}

async fn finalize_reconciled(
    state: &AppState,
    record: &SettlementRecord,
    outcome: &FinalExecutionOutcomeView,
    payer: &AccountId,
    asset: &AccountId,
    transaction_hash: CryptoHash,
) -> Result<(), StoreError> {
    if let Err(error) = validate_final_outcome_identity(
        outcome,
        transaction_hash,
        &state.provider.relayer_account_id(),
        payer,
    ) {
        state.readiness.set_reconciliation(false);
        tracing::warn!(
            event = "reconciliation_outcome_identity_indeterminate",
            reason = %error
        );
        return Ok(());
    }
    let (gas_burnt, tokens_burnt) = execution_cost(outcome);
    let transaction = transaction_hash.to_string();
    let (settlement_state, response, error_code) =
        match interpret_final_outcome(outcome, payer, asset) {
            Ok(_) => (
                SettlementState::Succeeded,
                SettleResponse::success(
                    payer.to_string(),
                    transaction,
                    state.config.network.clone(),
                ),
                None,
            ),
            Err(error) if error.is_definitive_failure() => (
                SettlementState::Failed,
                SettleResponse::failure(
                    "transaction_failed",
                    Some(error.to_string()),
                    Some(payer.to_string()),
                    transaction,
                    state.config.network.clone(),
                ),
                Some("transaction_failed".to_owned()),
            ),
            Err(error) => {
                state.readiness.set_reconciliation(false);
                tracing::warn!(
                    event = "reconciliation_receipt_indeterminate",
                    reason = %error
                );
                return Ok(());
            }
        };
    let (metric_result, metric_reason) = match settlement_state {
        SettlementState::Succeeded => ("succeeded", "success"),
        SettlementState::Failed => ("failed", "transaction_failed"),
        SettlementState::Reserved | SettlementState::Prepared | SettlementState::Submitted => {
            ("failed", "invalid_terminal_state")
        }
    };
    let bytes =
        serde_json::to_vec(&response).map_err(|error| StoreError::Corrupt(error.to_string()))?;
    state
        .store
        .mark_terminal(&TerminalJournalEntry {
            settlement_id: record.id,
            state: settlement_state,
            http_status: StatusCode::OK.as_u16(),
            response_bytes: bytes,
            error_code,
            error_detail: None,
            gas_burnt: Some(gas_burnt.to_string()),
            tokens_burnt: Some(tokens_burnt.to_string()),
            actual_yocto_near: tokens_burnt.to_string(),
        })
        .await?;
    state
        .metrics
        .record_settlement_cost(gas_burnt, yocto_near_metric(tokens_burnt));
    state
        .metrics
        .record_settlement_result(metric_result, metric_reason);
    tracing::info!(
        event = "settlement_terminal",
        result = metric_result,
        reason = metric_reason
    );
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Operation {
    Verify,
    Settle,
}

#[derive(Default)]
struct RateLimiter {
    windows: Mutex<HashMap<(String, Operation), RateWindow>>,
}

struct RateWindow {
    started: Instant,
    count: u32,
}

impl RateLimiter {
    async fn check(&self, key_prefix: &str, operation: Operation, limit: u32) -> bool {
        let mut windows = self.windows.lock().await;
        let now = Instant::now();
        let window = windows
            .entry((key_prefix.to_owned(), operation))
            .or_insert(RateWindow {
                started: now,
                count: 0,
            });
        if now.duration_since(window.started) >= Duration::from_secs(60) {
            window.started = now;
            window.count = 0;
        }
        if window.count >= limit {
            return false;
        }
        window.count = window.count.saturating_add(1);
        true
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use tower::ServiceExt as _;
    use tower_http::trace::DefaultMakeSpan;
    use tracing_subscriber::fmt::{MakeWriter, format::FmtSpan};

    use super::*;

    #[derive(Clone)]
    struct CaptureWriter(Arc<StdMutex<Vec<u8>>>);

    struct CaptureGuard(Arc<StdMutex<Vec<u8>>>);

    impl std::io::Write for CaptureGuard {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .map_err(|_| std::io::Error::other("capture lock poisoned"))?
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> MakeWriter<'writer> for CaptureWriter {
        type Writer = CaptureGuard;

        fn make_writer(&'writer self) -> Self::Writer {
            CaptureGuard(Arc::clone(&self.0))
        }
    }

    #[test]
    fn content_type_is_strict_but_allows_charset() {
        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        );
        assert!(ensure_json(&headers).is_ok());
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/plain"));
        assert!(ensure_json(&headers).is_err());
    }

    #[tokio::test]
    async fn rate_limiter_enforces_each_operation_separately() {
        let limiter = RateLimiter::default();
        assert!(limiter.check("x402_test_a", Operation::Verify, 1).await);
        assert!(!limiter.check("x402_test_a", Operation::Verify, 1).await);
        assert!(limiter.check("x402_test_b", Operation::Verify, 1).await);
        assert!(limiter.check("x402_test_a", Operation::Settle, 1).await);
    }

    #[test]
    fn service_errors_have_stable_nested_shape() {
        let value: Value =
            serde_json::from_slice(&service_error_bytes("pending", "retry")).unwrap_or(Value::Null);
        assert_eq!(value["error"]["code"], "pending");
        assert_eq!(value["error"]["message"], "retry");
    }

    #[test]
    fn signed_delegate_decoder_is_linked_into_service_boundary() {
        assert!(x402_chain_near::decode_signed_delegate("not-base64").is_err());
    }

    #[test]
    fn conflicting_final_results_fail_closed() {
        assert!(!final_outcomes_conflict(Some(&1_u8), Some(&1_u8)));
        assert!(final_outcomes_conflict(Some(&1_u8), Some(&2_u8)));
        assert!(!final_outcomes_conflict(Some(&1_u8), None));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn authentication_headers_are_redacted_before_http_tracing() {
        let bytes = Arc::new(StdMutex::new(Vec::new()));
        let writer = CaptureWriter(Arc::clone(&bytes));
        let subscriber = tracing_subscriber::fmt()
            .with_writer(writer)
            .with_ansi(false)
            .without_time()
            .with_span_events(FmtSpan::NEW)
            .finish();
        let _subscriber_guard = tracing::subscriber::set_default(subscriber);
        let application = Router::new().route("/healthz", get(health)).layer(
            ServiceBuilder::new()
                .layer(SetSensitiveRequestHeadersLayer::new([
                    AUTHORIZATION,
                    HeaderName::from_static("x-api-key"),
                ]))
                .layer(
                    TraceLayer::new_for_http().make_span_with(
                        DefaultMakeSpan::new()
                            .level(tracing::Level::INFO)
                            .include_headers(true),
                    ),
                ),
        );
        let secret = format!("x402_test_{}.{}", "a".repeat(24), "b".repeat(64));
        let request = Request::builder()
            .uri("/healthz")
            .header("x-api-key", &secret)
            .header(AUTHORIZATION, format!("Bearer {secret}"))
            .body(Body::empty())
            .unwrap_or_else(|_| std::process::abort());
        let response = application
            .oneshot(request)
            .await
            .unwrap_or_else(|error| match error {});
        assert_eq!(response.status(), StatusCode::OK);
        let output = bytes.lock().map_or_else(
            |_| std::process::abort(),
            |bytes| String::from_utf8_lossy(&bytes).into_owned(),
        );
        assert!(!output.contains(&secret));
        assert!(
            output.matches("Sensitive").count() >= 2,
            "captured trace did not include an explicit redaction marker: {output}"
        );
    }
}

#[cfg(test)]
#[path = "service_http_tests.rs"]
mod http_tests;

#[cfg(test)]
#[path = "service_recovery_tests.rs"]
mod recovery_tests;
