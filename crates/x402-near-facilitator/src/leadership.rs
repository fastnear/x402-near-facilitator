use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};
use sqlx::{Connection, PgConnection};
use tokio::sync::watch;
use zeroize::Zeroizing;

#[derive(Clone, Default)]
#[allow(missing_debug_implementations)]
pub struct ReadinessState {
    inner: Arc<ReadinessInner>,
}

#[derive(Default)]
struct ReadinessInner {
    leadership: AtomicBool,
    reconciliation: AtomicBool,
    rpc: AtomicBool,
    relayer: AtomicBool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
// Named readiness gates are intentionally independent and externally visible.
#[allow(clippy::struct_excessive_bools)]
pub struct ReadinessSnapshot {
    pub leadership: bool,
    pub reconciliation: bool,
    pub rpc: bool,
    pub relayer: bool,
}

#[allow(missing_debug_implementations)]
pub struct LeadershipHandle {
    shutdown: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl ReadinessState {
    pub fn set_leadership(&self, ready: bool) {
        self.inner.leadership.store(ready, Ordering::Release);
        if !ready {
            self.set_reconciliation(false);
        }
    }

    pub fn set_reconciliation(&self, ready: bool) {
        self.inner.reconciliation.store(ready, Ordering::Release);
    }

    pub fn set_rpc(&self, ready: bool) {
        self.inner.rpc.store(ready, Ordering::Release);
    }

    pub fn set_relayer(&self, ready: bool) {
        self.inner.relayer.store(ready, Ordering::Release);
    }

    pub fn snapshot(&self) -> ReadinessSnapshot {
        ReadinessSnapshot {
            leadership: self.inner.leadership.load(Ordering::Acquire),
            reconciliation: self.inner.reconciliation.load(Ordering::Acquire),
            rpc: self.inner.rpc.load(Ordering::Acquire),
            relayer: self.inner.relayer.load(Ordering::Acquire),
        }
    }

    pub fn can_settle(&self) -> bool {
        let snapshot = self.snapshot();
        snapshot.leadership && snapshot.reconciliation && snapshot.rpc && snapshot.relayer
    }
}

impl LeadershipHandle {
    pub fn spawn(
        direct_database_url: Zeroizing<String>,
        network: &str,
        readiness: ReadinessState,
    ) -> Self {
        let (shutdown, receiver) = watch::channel(false);
        let lock_key = advisory_lock_key(network);
        let task = tokio::spawn(run_leadership(
            direct_database_url,
            lock_key,
            readiness,
            receiver,
        ));
        Self { shutdown, task }
    }

    pub async fn shutdown(self) {
        let _send_result = self.shutdown.send(true);
        let _join_result = self.task.await;
    }
}

async fn run_leadership(
    database_url: Zeroizing<String>,
    lock_key: i64,
    readiness: ReadinessState,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut retry = Duration::from_millis(250);
    while !*shutdown.borrow() {
        readiness.set_leadership(false);
        let connection = PgConnection::connect(database_url.as_str()).await;
        let Ok(mut connection) = connection else {
            tracing::warn!(event = "leadership_connection_failed");
            wait_or_shutdown(retry, &mut shutdown).await;
            retry = (retry * 2).min(Duration::from_secs(5));
            continue;
        };
        let acquired = sqlx::query_scalar::<_, bool>("SELECT pg_try_advisory_lock($1)")
            .bind(lock_key)
            .fetch_one(&mut connection)
            .await;
        match acquired {
            Ok(true) => {
                retry = Duration::from_millis(250);
                readiness.set_leadership(true);
                tracing::info!(event = "leadership_acquired");
                if hold_leadership(&mut connection, &readiness, &mut shutdown).await {
                    break;
                }
                tracing::warn!(event = "leadership_lost");
            }
            Ok(false) => {
                wait_or_shutdown(Duration::from_secs(2), &mut shutdown).await;
            }
            Err(_) => {
                tracing::warn!(event = "leadership_lock_failed");
                wait_or_shutdown(retry, &mut shutdown).await;
                retry = (retry * 2).min(Duration::from_secs(5));
            }
        }
    }
    readiness.set_leadership(false);
}

async fn hold_leadership(
    connection: &mut PgConnection,
    readiness: &ReadinessState,
    shutdown: &mut watch::Receiver<bool>,
) -> bool {
    let mut heartbeat = tokio::time::interval(Duration::from_secs(2));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                if sqlx::query("SELECT 1").execute(&mut *connection).await.is_err() {
                    readiness.set_leadership(false);
                    return false;
                }
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    readiness.set_leadership(false);
                    return true;
                }
            }
        }
    }
}

async fn wait_or_shutdown(duration: Duration, shutdown: &mut watch::Receiver<bool>) {
    tokio::select! {
        () = tokio::time::sleep(duration) => {}
        _ = shutdown.changed() => {}
    }
}

fn advisory_lock_key(network: &str) -> i64 {
    let digest: [u8; 32] =
        Sha256::digest(format!("x402-near-facilitator/leader/v1/{network}")).into();
    let mut key = [0_u8; 8];
    key.copy_from_slice(&digest[..8]);
    i64::from_be_bytes(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advisory_keys_are_stable_and_network_specific() {
        assert_eq!(
            advisory_lock_key("near:mainnet"),
            advisory_lock_key("near:mainnet")
        );
        assert_ne!(
            advisory_lock_key("near:mainnet"),
            advisory_lock_key("near:testnet")
        );
    }

    #[test]
    fn losing_leadership_also_invalidates_reconciliation() {
        let readiness = ReadinessState::default();
        readiness.set_leadership(true);
        readiness.set_reconciliation(true);
        readiness.set_rpc(true);
        readiness.set_relayer(true);
        assert!(readiness.can_settle());
        readiness.set_leadership(false);
        assert!(!readiness.snapshot().reconciliation);
        assert!(!readiness.can_settle());
    }
}

#[cfg(test)]
#[path = "leadership_postgres_tests.rs"]
mod postgres_tests;
