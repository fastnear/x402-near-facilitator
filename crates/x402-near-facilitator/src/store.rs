use std::time::Duration;
use std::{borrow::Cow, str::FromStr};

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::Value;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgRow};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

fn embedded_migrator() -> sqlx::migrate::Migrator {
    let migration = sqlx::migrate::Migration::new(
        1,
        Cow::Borrowed("initial"),
        sqlx::migrate::MigrationType::Simple,
        Cow::Borrowed(include_str!("../../../migrations/0001_initial.sql")),
        false,
    );
    sqlx::migrate::Migrator {
        migrations: Cow::Owned(vec![migration]),
        ..sqlx::migrate::Migrator::DEFAULT
    }
}

#[derive(Clone)]
#[allow(missing_debug_implementations)]
pub struct PgStore {
    pool: PgPool,
}

#[derive(Clone, Debug)]
pub struct ApiClient {
    pub id: Uuid,
    pub name: String,
    pub environment: String,
    pub daily_budget_yocto_near: String,
    pub verify_rate_per_minute: u32,
    pub settle_rate_per_minute: u32,
}

#[allow(missing_debug_implementations)]
pub struct ApiKeyCandidate {
    pub client: ApiClient,
    pub digest: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettlementState {
    Reserved,
    Prepared,
    Submitted,
    Succeeded,
    Failed,
}

impl SettlementState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reserved => "reserved",
            Self::Prepared => "prepared",
            Self::Submitted => "submitted",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }

    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed)
    }
}

impl FromStr for SettlementState {
    type Err = StoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "reserved" => Ok(Self::Reserved),
            "prepared" => Ok(Self::Prepared),
            "submitted" => Ok(Self::Submitted),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            _ => Err(StoreError::Corrupt(format!(
                "unknown settlement state {value}"
            ))),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SettlementRecord {
    pub id: Uuid,
    pub api_client_id: Uuid,
    pub payment_identifier: Option<String>,
    pub payment_hash: [u8; 32],
    pub request_fingerprint: [u8; 32],
    pub state: SettlementState,
    pub network: String,
    pub asset: String,
    pub pay_to: String,
    pub amount: String,
    pub payer: String,
    pub delegate_public_key: String,
    pub delegate_nonce: String,
    pub delegate_max_block_height: String,
    pub reservation_date: NaiveDate,
    pub reserved_yocto_near: String,
    pub relayer_account_id: Option<String>,
    pub relayer_public_key: Option<String>,
    pub relayer_nonce: Option<String>,
    pub outer_transaction_bytes: Option<Vec<u8>>,
    pub outer_transaction_hash: Option<String>,
    pub terminal_http_status: Option<u16>,
    pub terminal_response_bytes: Option<Vec<u8>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct JournalSummary {
    pub reserved: u64,
    pub prepared: u64,
    pub submitted: u64,
    pub oldest_created_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
pub struct SponsorshipUsage {
    pub reserved_yocto_near: String,
    pub spent_yocto_near: String,
}

#[derive(Clone, Debug)]
pub struct NewSettlement {
    pub id: Uuid,
    pub api_client_id: Uuid,
    pub payment_identifier: Option<String>,
    pub payment_hash: [u8; 32],
    pub request_fingerprint: [u8; 32],
    pub x402_version: u8,
    pub scheme: String,
    pub network: String,
    pub asset: String,
    pub pay_to: String,
    pub amount: String,
    pub payer: String,
    pub delegate_public_key: String,
    pub delegate_nonce: String,
    pub delegate_max_block_height: String,
    pub policy_snapshot: Value,
    pub reservation_yocto_near: String,
    pub global_daily_budget_yocto_near: String,
    pub client_daily_budget_yocto_near: String,
}

#[derive(Clone, Debug)]
pub struct PreparedJournalEntry {
    pub settlement_id: Uuid,
    pub relayer_account_id: String,
    pub relayer_public_key: String,
    pub relayer_nonce: String,
    pub transaction_bytes: Vec<u8>,
    pub transaction_hash: String,
}

#[derive(Clone, Debug)]
pub struct TerminalJournalEntry {
    pub settlement_id: Uuid,
    pub state: SettlementState,
    pub http_status: u16,
    pub response_bytes: Vec<u8>,
    pub error_code: Option<String>,
    pub error_detail: Option<String>,
    pub gas_burnt: Option<String>,
    pub tokens_burnt: Option<String>,
    pub actual_yocto_near: String,
}

#[derive(Clone, Debug)]
pub enum ClaimOutcome {
    New(SettlementRecord),
    Existing(SettlementRecord),
    IdentifierConflict,
    DuplicateSettlement,
    BudgetExceeded,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database operation failed")]
    Database(#[source] sqlx::Error),
    #[error("database migration failed")]
    Migration(#[source] sqlx::migrate::MigrateError),
    #[error("database state is inconsistent: {0}")]
    Corrupt(String),
    #[error("invalid database configuration: {0}")]
    Configuration(String),
    #[error("invalid state transition from {from} to {to}")]
    Transition { from: String, to: String },
}

impl From<sqlx::Error> for StoreError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

impl PgStore {
    #[cfg(test)]
    pub(crate) fn from_explicit_test_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn connect(database_url: &str, max_connections: u32) -> Result<Self, StoreError> {
        let options = PgConnectOptions::from_str(database_url).map_err(|_| {
            StoreError::Configuration("database URL could not be parsed".to_owned())
        })?;
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .min_connections(1)
            .acquire_timeout(Duration::from_secs(5))
            .idle_timeout(Duration::from_secs(300))
            .after_connect(|connection, _metadata| {
                Box::pin(async move {
                    sqlx::query("SET TIME ZONE 'UTC'")
                        .execute(&mut *connection)
                        .await?;
                    sqlx::query("SET statement_timeout = '15s'")
                        .execute(&mut *connection)
                        .await?;
                    sqlx::query("SET lock_timeout = '5s'")
                        .execute(&mut *connection)
                        .await?;
                    Ok(())
                })
            })
            .connect_with(options)
            .await?;
        Ok(Self { pool })
    }

    pub async fn migrate(&self) -> Result<(), StoreError> {
        embedded_migrator()
            .run(&self.pool)
            .await
            .map_err(StoreError::Migration)
    }

    pub async fn ping(&self) -> Result<(), StoreError> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    pub async fn schema_compatible(&self) -> Result<bool, StoreError> {
        for migration in embedded_migrator().iter() {
            let checksum: Option<Vec<u8>> = sqlx::query_scalar(
                "SELECT checksum FROM _sqlx_migrations \
                 WHERE version = $1 AND success = true",
            )
            .bind(migration.version)
            .fetch_optional(&self.pool)
            .await?;
            if checksum.as_deref() != Some(migration.checksum.as_ref()) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub async fn active_clients_have_payee_policy(
        &self,
        network: &str,
        asset: &str,
    ) -> Result<bool, StoreError> {
        sqlx::query_scalar(
            "SELECT \
                EXISTS ( \
                    SELECT 1 FROM api_clients c \
                    WHERE c.status = 'active' \
                ) \
                AND NOT EXISTS ( \
                    SELECT 1 FROM api_clients c \
                    WHERE c.status = 'active' \
                      AND NOT EXISTS ( \
                        SELECT 1 FROM api_client_payees p \
                        WHERE p.client_id = c.id \
                          AND p.network = $1 \
                          AND p.asset = $2 \
                      ) \
                )",
        )
        .bind(network)
        .bind(asset)
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::Database)
    }

    pub async fn operationally_ready(
        &self,
        network: &str,
        asset: &str,
    ) -> Result<bool, StoreError> {
        self.ping().await?;
        Ok(self.schema_compatible().await?
            && self
                .active_clients_have_payee_policy(network, asset)
                .await?)
    }

    pub async fn lookup_api_key(
        &self,
        key_prefix: &str,
    ) -> Result<Option<ApiKeyCandidate>, StoreError> {
        let row = sqlx::query(
            "SELECT c.id, c.name, c.environment, \
                    c.daily_budget_yocto_near::text AS daily_budget, \
                    c.verify_rate_per_minute, c.settle_rate_per_minute, k.key_digest \
             FROM api_keys k \
             JOIN api_clients c ON c.id = k.client_id \
             WHERE k.key_prefix = $1 \
               AND k.status = 'active' \
               AND c.status = 'active'",
        )
        .bind(key_prefix)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(api_key_from_row).transpose()
    }

    pub async fn touch_api_key(&self, key_prefix: &str) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE api_keys SET last_used_at = now() \
             WHERE key_prefix = $1 AND status = 'active'",
        )
        .bind(key_prefix)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn payee_allowed(
        &self,
        client_id: Uuid,
        network: &str,
        asset: &str,
        pay_to: &str,
    ) -> Result<bool, StoreError> {
        let allowed: bool = sqlx::query_scalar(
            "SELECT EXISTS( \
                SELECT 1 FROM api_client_payees \
                WHERE client_id = $1 AND network = $2 AND asset = $3 AND pay_to = $4 \
             )",
        )
        .bind(client_id)
        .bind(network)
        .bind(asset)
        .bind(pay_to)
        .fetch_one(&self.pool)
        .await?;
        Ok(allowed)
    }

    #[allow(clippy::too_many_lines)]
    pub async fn claim_settlement(&self, new: &NewSettlement) -> Result<ClaimOutcome, StoreError> {
        if let Some(existing) = self
            .find_existing_settlement(
                new.api_client_id,
                new.payment_identifier.as_deref(),
                &new.payment_hash,
                &new.request_fingerprint,
            )
            .await?
        {
            return Ok(existing);
        }

        let usage_date = Utc::now().date_naive();
        let mut transaction = self.pool.begin().await?;
        if !reserve_global_budget(
            &mut transaction,
            usage_date,
            &new.reservation_yocto_near,
            &new.global_daily_budget_yocto_near,
        )
        .await?
        {
            transaction.rollback().await?;
            return Ok(ClaimOutcome::BudgetExceeded);
        }
        if !reserve_client_budget(
            &mut transaction,
            usage_date,
            new.api_client_id,
            &new.reservation_yocto_near,
            &new.client_daily_budget_yocto_near,
        )
        .await?
        {
            transaction.rollback().await?;
            return Ok(ClaimOutcome::BudgetExceeded);
        }

        let inserted = sqlx::query(
            "INSERT INTO settlements ( \
                id, api_client_id, payment_identifier, payment_hash, request_fingerprint, \
                state, x402_version, scheme, network, asset, pay_to, amount, payer, \
                delegate_public_key, delegate_nonce, delegate_max_block_height, \
                policy_snapshot, reservation_date, reserved_yocto_near \
             ) VALUES ( \
                $1, $2, $3, $4, $5, 'reserved', $6, $7, $8, $9, $10, $11::numeric, \
                $12, $13, $14::numeric, $15::numeric, $16, $17, $18::numeric \
             ) \
             ON CONFLICT DO NOTHING",
        )
        .bind(new.id)
        .bind(new.api_client_id)
        .bind(&new.payment_identifier)
        .bind(new.payment_hash.as_slice())
        .bind(new.request_fingerprint.as_slice())
        .bind(i16::from(new.x402_version))
        .bind(&new.scheme)
        .bind(&new.network)
        .bind(&new.asset)
        .bind(&new.pay_to)
        .bind(&new.amount)
        .bind(&new.payer)
        .bind(&new.delegate_public_key)
        .bind(&new.delegate_nonce)
        .bind(&new.delegate_max_block_height)
        .bind(&new.policy_snapshot)
        .bind(usage_date)
        .bind(&new.reservation_yocto_near)
        .execute(&mut *transaction)
        .await?;

        if inserted.rows_affected() == 0 {
            transaction.rollback().await?;
            return self
                .find_existing_settlement(
                    new.api_client_id,
                    new.payment_identifier.as_deref(),
                    &new.payment_hash,
                    &new.request_fingerprint,
                )
                .await?
                .ok_or_else(|| {
                    StoreError::Corrupt(
                        "settlement insert conflicted but conflicting row was not visible"
                            .to_owned(),
                    )
                });
        }

        insert_event(
            &mut transaction,
            new.id,
            None,
            SettlementState::Reserved,
            Some("claimed"),
            &serde_json::json!({}),
        )
        .await?;
        transaction.commit().await?;
        let record = self
            .settlement(new.id)
            .await?
            .ok_or_else(|| StoreError::Corrupt("inserted settlement disappeared".to_owned()))?;
        Ok(ClaimOutcome::New(record))
    }

    pub async fn settlement(&self, id: Uuid) -> Result<Option<SettlementRecord>, StoreError> {
        let row = sqlx::query(SETTLEMENT_SELECT)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|row| settlement_from_row(&row)).transpose()
    }

    pub async fn nonterminal_settlements(&self) -> Result<Vec<SettlementRecord>, StoreError> {
        let sql = format!(
            "{SETTLEMENT_SELECT_BASE} \
             WHERE state IN ('reserved', 'prepared', 'submitted') ORDER BY created_at"
        );
        let rows = sqlx::query(&sql).fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|row| settlement_from_row(&row))
            .collect()
    }

    pub async fn journal_summary(&self) -> Result<JournalSummary, StoreError> {
        let row = sqlx::query(
            "SELECT \
                count(*) FILTER (WHERE state = 'reserved') AS reserved, \
                count(*) FILTER (WHERE state = 'prepared') AS prepared, \
                count(*) FILTER (WHERE state = 'submitted') AS submitted, \
                min(created_at) AS oldest_created_at \
             FROM settlements \
             WHERE state IN ('reserved', 'prepared', 'submitted')",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(JournalSummary {
            reserved: nonnegative_count(&row, "reserved")?,
            prepared: nonnegative_count(&row, "prepared")?,
            submitted: nonnegative_count(&row, "submitted")?,
            oldest_created_at: row.try_get("oldest_created_at")?,
        })
    }

    pub async fn global_sponsorship_usage_today(&self) -> Result<SponsorshipUsage, StoreError> {
        let row = sqlx::query(
            "SELECT \
                COALESCE(( \
                    SELECT reserved_yocto_near::text \
                    FROM daily_global_sponsorship WHERE usage_date = CURRENT_DATE \
                ), '0') AS reserved, \
                COALESCE(( \
                    SELECT spent_yocto_near::text \
                    FROM daily_global_sponsorship WHERE usage_date = CURRENT_DATE \
                ), '0') AS spent",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(SponsorshipUsage {
            reserved_yocto_near: row.try_get("reserved")?,
            spent_yocto_near: row.try_get("spent")?,
        })
    }

    pub async fn mark_prepared(&self, entry: &PreparedJournalEntry) -> Result<(), StoreError> {
        let mut transaction = self.pool.begin().await?;
        let result = sqlx::query(
            "UPDATE settlements SET \
                state = 'prepared', relayer_account_id = $2, relayer_public_key = $3, \
                relayer_nonce = $4::numeric, outer_transaction_bytes = $5, \
                outer_transaction_hash = $6, prepared_at = now(), updated_at = now() \
             WHERE id = $1 AND state = 'reserved'",
        )
        .bind(entry.settlement_id)
        .bind(&entry.relayer_account_id)
        .bind(&entry.relayer_public_key)
        .bind(&entry.relayer_nonce)
        .bind(&entry.transaction_bytes)
        .bind(&entry.transaction_hash)
        .execute(&mut *transaction)
        .await?;
        if result.rows_affected() != 1 {
            let from = state_for_update(&mut transaction, entry.settlement_id).await?;
            return Err(StoreError::Transition {
                from,
                to: SettlementState::Prepared.as_str().to_owned(),
            });
        }
        insert_event(
            &mut transaction,
            entry.settlement_id,
            Some(SettlementState::Reserved),
            SettlementState::Prepared,
            Some("outer_transaction_persisted"),
            &serde_json::json!({"transaction": entry.transaction_hash}),
        )
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn mark_submitted(&self, id: Uuid) -> Result<(), StoreError> {
        let mut transaction = self.pool.begin().await?;
        let result = sqlx::query(
            "UPDATE settlements SET state = 'submitted', submitted_at = now(), updated_at = now() \
             WHERE id = $1 AND state = 'prepared'",
        )
        .bind(id)
        .execute(&mut *transaction)
        .await?;
        if result.rows_affected() != 1 {
            let from = state_for_update(&mut transaction, id).await?;
            return Err(StoreError::Transition {
                from,
                to: SettlementState::Submitted.as_str().to_owned(),
            });
        }
        insert_event(
            &mut transaction,
            id,
            Some(SettlementState::Prepared),
            SettlementState::Submitted,
            Some("broadcast_started"),
            &serde_json::json!({}),
        )
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn mark_terminal(&self, entry: &TerminalJournalEntry) -> Result<(), StoreError> {
        if !entry.state.is_terminal() {
            return Err(StoreError::Transition {
                from: "unknown".to_owned(),
                to: entry.state.as_str().to_owned(),
            });
        }
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT state, reservation_date, api_client_id, \
                    reserved_yocto_near::text AS reserved \
             FROM settlements WHERE id = $1 FOR UPDATE",
        )
        .bind(entry.settlement_id)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or_else(|| StoreError::Corrupt("terminal settlement not found".to_owned()))?;
        let from: String = row.try_get("state")?;
        let from_state = SettlementState::from_str(&from)?;
        if from_state.is_terminal() {
            // Terminal transitions are idempotent only when the exact body and
            // status already match.
            let existing = sqlx::query(
                "SELECT terminal_http_status, terminal_response_bytes \
                 FROM settlements WHERE id = $1",
            )
            .bind(entry.settlement_id)
            .fetch_one(&mut *transaction)
            .await?;
            let status: Option<i16> = existing.try_get("terminal_http_status")?;
            let bytes: Option<Vec<u8>> = existing.try_get("terminal_response_bytes")?;
            transaction.rollback().await?;
            if status == i16::try_from(entry.http_status).ok()
                && bytes.as_deref() == Some(entry.response_bytes.as_slice())
            {
                return Ok(());
            }
            return Err(StoreError::Transition {
                from,
                to: entry.state.as_str().to_owned(),
            });
        }
        let transition_allowed = matches!(
            (from_state, entry.state),
            (SettlementState::Reserved, SettlementState::Failed)
                | (
                    SettlementState::Prepared | SettlementState::Submitted,
                    SettlementState::Succeeded | SettlementState::Failed,
                )
        );
        if !transition_allowed {
            transaction.rollback().await?;
            return Err(StoreError::Transition {
                from,
                to: entry.state.as_str().to_owned(),
            });
        }
        let usage_date: NaiveDate = row.try_get("reservation_date")?;
        let client_id: Uuid = row.try_get("api_client_id")?;
        let reserved: String = row.try_get("reserved")?;

        release_budget(
            &mut transaction,
            usage_date,
            client_id,
            &reserved,
            &entry.actual_yocto_near,
        )
        .await?;
        let status = i16::try_from(entry.http_status).map_err(|_| {
            StoreError::Corrupt("terminal HTTP status does not fit SMALLINT".to_owned())
        })?;
        sqlx::query(
            "UPDATE settlements SET \
                state = $2, terminal_http_status = $3, terminal_response_bytes = $4, \
                error_code = $5, error_detail = $6, gas_burnt = $7::numeric, \
                tokens_burnt = $8::numeric, finalized_at = now(), updated_at = now() \
             WHERE id = $1",
        )
        .bind(entry.settlement_id)
        .bind(entry.state.as_str())
        .bind(status)
        .bind(&entry.response_bytes)
        .bind(&entry.error_code)
        .bind(&entry.error_detail)
        .bind(entry.gas_burnt.as_deref().unwrap_or("0"))
        .bind(entry.tokens_burnt.as_deref().unwrap_or("0"))
        .execute(&mut *transaction)
        .await?;
        insert_event(
            &mut transaction,
            entry.settlement_id,
            Some(from_state),
            entry.state,
            entry.error_code.as_deref(),
            &serde_json::json!({}),
        )
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn note_reconciliation(&self, id: Uuid) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE settlements SET last_reconciled_at = now(), \
                    reconciliation_attempts = reconciliation_attempts + 1, updated_at = now() \
             WHERE id = $1",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn upsert_relayer(
        &self,
        network: &str,
        account_id: &str,
        public_key: &str,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO relayers (network, account_id, public_key) VALUES ($1, $2, $3) \
             ON CONFLICT (network, account_id, public_key) DO NOTHING",
        )
        .bind(network)
        .bind(account_id)
        .bind(public_key)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn quarantine_relayer(
        &self,
        network: &str,
        account_id: &str,
        public_key: &str,
        reason: &str,
        observed_nonce: &str,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO relayers ( \
                network, account_id, public_key, status, quarantine_reason, last_observed_nonce \
             ) VALUES ($1, $2, $3, 'quarantined', $4, $5::numeric) \
             ON CONFLICT (network, account_id, public_key) DO UPDATE SET \
                status = 'quarantined', quarantine_reason = EXCLUDED.quarantine_reason, \
                last_observed_nonce = EXCLUDED.last_observed_nonce, updated_at = now()",
        )
        .bind(network)
        .bind(account_id)
        .bind(public_key)
        .bind(reason)
        .bind(observed_nonce)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn relayer_is_active(
        &self,
        network: &str,
        account_id: &str,
        public_key: &str,
    ) -> Result<bool, StoreError> {
        let status: Option<String> = sqlx::query_scalar(
            "SELECT status FROM relayers \
             WHERE network = $1 AND account_id = $2 AND public_key = $3",
        )
        .bind(network)
        .bind(account_id)
        .bind(public_key)
        .fetch_optional(&self.pool)
        .await?;
        Ok(matches!(status.as_deref(), Some("active")))
    }

    pub async fn create_client(
        &self,
        client: &ApiClient,
        key_id: Uuid,
        key_prefix: &str,
        key_digest: &[u8; 32],
    ) -> Result<(), StoreError> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO api_clients ( \
                id, name, environment, daily_budget_yocto_near, \
                verify_rate_per_minute, settle_rate_per_minute \
             ) VALUES ($1, $2, $3, $4::numeric, $5, $6)",
        )
        .bind(client.id)
        .bind(&client.name)
        .bind(&client.environment)
        .bind(&client.daily_budget_yocto_near)
        .bind(
            i32::try_from(client.verify_rate_per_minute)
                .map_err(|_| StoreError::Configuration("verify rate is too large".to_owned()))?,
        )
        .bind(
            i32::try_from(client.settle_rate_per_minute)
                .map_err(|_| StoreError::Configuration("settle rate is too large".to_owned()))?,
        )
        .execute(&mut *transaction)
        .await?;
        insert_api_key(&mut transaction, key_id, client.id, key_prefix, key_digest).await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn client_environment(&self, client_id: Uuid) -> Result<String, StoreError> {
        sqlx::query_scalar(
            "SELECT environment FROM api_clients WHERE id = $1 AND status = 'active'",
        )
        .bind(client_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| StoreError::Corrupt("active client not found".to_owned()))
    }

    pub async fn rotate_client_key(
        &self,
        client_id: Uuid,
        key_id: Uuid,
        key_prefix: &str,
        key_digest: &[u8; 32],
    ) -> Result<(), StoreError> {
        let mut transaction = self.pool.begin().await?;
        let active: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM api_clients WHERE id = $1 AND status = 'active')",
        )
        .bind(client_id)
        .fetch_one(&mut *transaction)
        .await?;
        if !active {
            return Err(StoreError::Corrupt("active client not found".to_owned()));
        }
        sqlx::query(
            "UPDATE api_keys SET status = 'revoked', revoked_at = now() \
             WHERE client_id = $1 AND status = 'active'",
        )
        .bind(client_id)
        .execute(&mut *transaction)
        .await?;
        insert_api_key(&mut transaction, key_id, client_id, key_prefix, key_digest).await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn revoke_client(&self, client_id: Uuid) -> Result<bool, StoreError> {
        let mut transaction = self.pool.begin().await?;
        let changed = sqlx::query(
            "UPDATE api_clients SET status = 'revoked', revoked_at = now(), updated_at = now() \
             WHERE id = $1 AND status = 'active'",
        )
        .bind(client_id)
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        sqlx::query(
            "UPDATE api_keys SET status = 'revoked', revoked_at = now() \
             WHERE client_id = $1 AND status = 'active'",
        )
        .bind(client_id)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(changed == 1)
    }

    pub async fn allow_payee(
        &self,
        client_id: Uuid,
        network: &str,
        asset: &str,
        pay_to: &str,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO api_client_payees (client_id, network, asset, pay_to) \
             VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
        )
        .bind(client_id)
        .bind(network)
        .bind(asset)
        .bind(pay_to)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_client_budget(
        &self,
        client_id: Uuid,
        daily_yocto_near: &str,
    ) -> Result<bool, StoreError> {
        let changed = sqlx::query(
            "UPDATE api_clients SET daily_budget_yocto_near = $2::numeric, updated_at = now() \
             WHERE id = $1 AND status = 'active'",
        )
        .bind(client_id)
        .bind(daily_yocto_near)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(changed == 1)
    }

    pub async fn find_existing_settlement(
        &self,
        api_client_id: Uuid,
        payment_identifier: Option<&str>,
        payment_hash: &[u8; 32],
        request_fingerprint: &[u8; 32],
    ) -> Result<Option<ClaimOutcome>, StoreError> {
        if let Some(identifier) = payment_identifier {
            let sql = format!(
                "{SETTLEMENT_SELECT_BASE} \
                 WHERE (api_client_id = $1 AND payment_identifier = $2) \
                    OR payment_hash = $3"
            );
            let rows = sqlx::query(&sql)
                .bind(api_client_id)
                .bind(identifier)
                .bind(payment_hash.as_slice())
                .fetch_all(&self.pool)
                .await?;
            let mut payment_hash_exists = false;
            for row in rows {
                let existing = settlement_from_row(&row)?;
                if existing.api_client_id == api_client_id
                    && existing.payment_identifier.as_deref() == Some(identifier)
                {
                    return if existing.request_fingerprint == *request_fingerprint {
                        Ok(Some(ClaimOutcome::Existing(existing)))
                    } else {
                        Ok(Some(ClaimOutcome::IdentifierConflict))
                    };
                }
                payment_hash_exists |= existing.payment_hash == *payment_hash;
            }
            if payment_hash_exists {
                return Ok(Some(ClaimOutcome::DuplicateSettlement));
            }
            return Ok(None);
        }
        let sql = format!("{SETTLEMENT_SELECT_BASE} WHERE payment_hash = $1");
        if sqlx::query(&sql)
            .bind(payment_hash.as_slice())
            .fetch_optional(&self.pool)
            .await?
            .is_some()
        {
            return Ok(Some(ClaimOutcome::DuplicateSettlement));
        }
        Ok(None)
    }
}

const SETTLEMENT_SELECT_BASE: &str = "SELECT \
    id, api_client_id, payment_identifier, payment_hash, request_fingerprint, state, \
    network, asset, pay_to, amount::text AS amount, payer, delegate_public_key, \
    delegate_nonce::text AS delegate_nonce, \
    delegate_max_block_height::text AS delegate_max_block_height, reservation_date, \
    reserved_yocto_near::text AS reserved_yocto_near, relayer_account_id, \
    relayer_public_key, relayer_nonce::text AS relayer_nonce, outer_transaction_bytes, \
    outer_transaction_hash, terminal_http_status, terminal_response_bytes, created_at \
    FROM settlements";

const SETTLEMENT_SELECT: &str = "SELECT \
    id, api_client_id, payment_identifier, payment_hash, request_fingerprint, state, \
    network, asset, pay_to, amount::text AS amount, payer, delegate_public_key, \
    delegate_nonce::text AS delegate_nonce, \
    delegate_max_block_height::text AS delegate_max_block_height, reservation_date, \
    reserved_yocto_near::text AS reserved_yocto_near, relayer_account_id, \
    relayer_public_key, relayer_nonce::text AS relayer_nonce, outer_transaction_bytes, \
    outer_transaction_hash, terminal_http_status, terminal_response_bytes, created_at \
    FROM settlements WHERE id = $1";

fn api_key_from_row(row: &PgRow) -> Result<ApiKeyCandidate, StoreError> {
    let verify_rate: i32 = row.try_get("verify_rate_per_minute")?;
    let settle_rate: i32 = row.try_get("settle_rate_per_minute")?;
    Ok(ApiKeyCandidate {
        client: ApiClient {
            id: row.try_get("id")?,
            name: row.try_get("name")?,
            environment: row.try_get("environment")?,
            daily_budget_yocto_near: row.try_get("daily_budget")?,
            verify_rate_per_minute: u32::try_from(verify_rate)
                .map_err(|_| StoreError::Corrupt("negative verify rate".to_owned()))?,
            settle_rate_per_minute: u32::try_from(settle_rate)
                .map_err(|_| StoreError::Corrupt("negative settle rate".to_owned()))?,
        },
        digest: row.try_get("key_digest")?,
    })
}

fn settlement_from_row(row: &PgRow) -> Result<SettlementRecord, StoreError> {
    let payment_hash = fixed_hash(row.try_get("payment_hash")?, "payment_hash")?;
    let request_fingerprint =
        fixed_hash(row.try_get("request_fingerprint")?, "request_fingerprint")?;
    let terminal_http_status: Option<i16> = row.try_get("terminal_http_status")?;
    Ok(SettlementRecord {
        id: row.try_get("id")?,
        api_client_id: row.try_get("api_client_id")?,
        payment_identifier: row.try_get("payment_identifier")?,
        payment_hash,
        request_fingerprint,
        state: SettlementState::from_str(row.try_get("state")?)?,
        network: row.try_get("network")?,
        asset: row.try_get("asset")?,
        pay_to: row.try_get("pay_to")?,
        amount: row.try_get("amount")?,
        payer: row.try_get("payer")?,
        delegate_public_key: row.try_get("delegate_public_key")?,
        delegate_nonce: row.try_get("delegate_nonce")?,
        delegate_max_block_height: row.try_get("delegate_max_block_height")?,
        reservation_date: row.try_get("reservation_date")?,
        reserved_yocto_near: row.try_get("reserved_yocto_near")?,
        relayer_account_id: row.try_get("relayer_account_id")?,
        relayer_public_key: row.try_get("relayer_public_key")?,
        relayer_nonce: row.try_get("relayer_nonce")?,
        outer_transaction_bytes: row.try_get("outer_transaction_bytes")?,
        outer_transaction_hash: row.try_get("outer_transaction_hash")?,
        terminal_http_status: terminal_http_status
            .map(u16::try_from)
            .transpose()
            .map_err(|_| StoreError::Corrupt("negative terminal status".to_owned()))?,
        terminal_response_bytes: row.try_get("terminal_response_bytes")?,
        created_at: row.try_get("created_at")?,
    })
}

fn fixed_hash(bytes: Vec<u8>, field: &str) -> Result<[u8; 32], StoreError> {
    bytes
        .try_into()
        .map_err(|_| StoreError::Corrupt(format!("{field} does not contain exactly 32 bytes")))
}

fn nonnegative_count(row: &PgRow, field: &str) -> Result<u64, StoreError> {
    let count: i64 = row.try_get(field)?;
    u64::try_from(count).map_err(|_| StoreError::Corrupt(format!("{field} count is negative")))
}

async fn reserve_global_budget(
    transaction: &mut Transaction<'_, Postgres>,
    usage_date: NaiveDate,
    reservation: &str,
    limit: &str,
) -> Result<bool, StoreError> {
    let row = sqlx::query(
        "INSERT INTO daily_global_sponsorship (usage_date, reserved_yocto_near) \
         SELECT $1, $2::numeric WHERE $2::numeric <= $3::numeric \
         ON CONFLICT (usage_date) DO UPDATE SET \
            reserved_yocto_near = daily_global_sponsorship.reserved_yocto_near + $2::numeric, \
            updated_at = now() \
         WHERE daily_global_sponsorship.reserved_yocto_near \
             + daily_global_sponsorship.spent_yocto_near + $2::numeric <= $3::numeric \
         RETURNING 1",
    )
    .bind(usage_date)
    .bind(reservation)
    .bind(limit)
    .fetch_optional(&mut **transaction)
    .await?;
    Ok(row.is_some())
}

async fn reserve_client_budget(
    transaction: &mut Transaction<'_, Postgres>,
    usage_date: NaiveDate,
    client_id: Uuid,
    reservation: &str,
    limit: &str,
) -> Result<bool, StoreError> {
    let row = sqlx::query(
        "INSERT INTO daily_client_sponsorship ( \
            usage_date, client_id, reserved_yocto_near \
         ) SELECT $1, $2, $3::numeric WHERE $3::numeric <= $4::numeric \
         ON CONFLICT (usage_date, client_id) DO UPDATE SET \
            reserved_yocto_near = daily_client_sponsorship.reserved_yocto_near + $3::numeric, \
            updated_at = now() \
         WHERE daily_client_sponsorship.reserved_yocto_near \
             + daily_client_sponsorship.spent_yocto_near + $3::numeric <= $4::numeric \
         RETURNING 1",
    )
    .bind(usage_date)
    .bind(client_id)
    .bind(reservation)
    .bind(limit)
    .fetch_optional(&mut **transaction)
    .await?;
    Ok(row.is_some())
}

async fn release_budget(
    transaction: &mut Transaction<'_, Postgres>,
    usage_date: NaiveDate,
    client_id: Uuid,
    reservation: &str,
    actual: &str,
) -> Result<(), StoreError> {
    let global = sqlx::query(
        "UPDATE daily_global_sponsorship SET \
            reserved_yocto_near = reserved_yocto_near - $2::numeric, \
            spent_yocto_near = spent_yocto_near + $3::numeric, updated_at = now() \
         WHERE usage_date = $1 AND reserved_yocto_near >= $2::numeric",
    )
    .bind(usage_date)
    .bind(reservation)
    .bind(actual)
    .execute(&mut **transaction)
    .await?;
    let client = sqlx::query(
        "UPDATE daily_client_sponsorship SET \
            reserved_yocto_near = reserved_yocto_near - $3::numeric, \
            spent_yocto_near = spent_yocto_near + $4::numeric, updated_at = now() \
         WHERE usage_date = $1 AND client_id = $2 \
           AND reserved_yocto_near >= $3::numeric",
    )
    .bind(usage_date)
    .bind(client_id)
    .bind(reservation)
    .bind(actual)
    .execute(&mut **transaction)
    .await?;
    if global.rows_affected() != 1 || client.rows_affected() != 1 {
        return Err(StoreError::Corrupt(
            "sponsorship reservation ledger row is missing".to_owned(),
        ));
    }
    Ok(())
}

async fn insert_event(
    transaction: &mut Transaction<'_, Postgres>,
    settlement_id: Uuid,
    from: Option<SettlementState>,
    to: SettlementState,
    code: Option<&str>,
    metadata: &Value,
) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO settlement_events (settlement_id, from_state, to_state, code, metadata) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(settlement_id)
    .bind(from.map(SettlementState::as_str))
    .bind(to.as_str())
    .bind(code)
    .bind(metadata)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn state_for_update(
    transaction: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<String, StoreError> {
    sqlx::query_scalar("SELECT state FROM settlements WHERE id = $1 FOR UPDATE")
        .bind(id)
        .fetch_optional(&mut **transaction)
        .await?
        .ok_or_else(|| StoreError::Corrupt("settlement not found".to_owned()))
}

async fn insert_api_key(
    transaction: &mut Transaction<'_, Postgres>,
    key_id: Uuid,
    client_id: Uuid,
    key_prefix: &str,
    key_digest: &[u8; 32],
) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO api_keys (id, client_id, key_prefix, key_digest) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(key_id)
    .bind(client_id)
    .bind(key_prefix)
    .bind(key_digest.as_slice())
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

#[cfg(test)]
#[path = "store_postgres_tests.rs"]
mod postgres_tests;
