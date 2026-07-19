use std::error::Error;
use std::time::Duration;

use url::Url;
use uuid::Uuid;
use zeroize::Zeroizing;

use super::{LeadershipHandle, ReadinessState};

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

fn loopback_database_url() -> TestResult<Option<String>> {
    let Ok(raw) = std::env::var("X402_FACILITATOR_TEST_DATABASE_URL") else {
        return Ok(None);
    };
    let url = Url::parse(&raw)?;
    if !matches!(url.scheme(), "postgres" | "postgresql") {
        return Ok(None);
    }
    let is_loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"));
    Ok(is_loopback.then_some(raw))
}

async fn wait_for_leadership(readiness: &ReadinessState, timeout: Duration) -> bool {
    tokio::time::timeout(timeout, async {
        while !readiness.snapshot().leadership {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .is_ok()
}

#[tokio::test]
async fn competing_instances_have_exactly_one_leader_and_fail_over() -> TestResult {
    let Some(database_url) = loopback_database_url()? else {
        eprintln!(
            "skipping PostgreSQL leadership test: \
             X402_FACILITATOR_TEST_DATABASE_URL is unset or not loopback"
        );
        return Ok(());
    };
    let network = format!("near:leadership-test-{}", Uuid::new_v4().simple());
    let primary_readiness = ReadinessState::default();
    let standby_readiness = ReadinessState::default();

    let primary = LeadershipHandle::spawn(
        Zeroizing::new(database_url.clone()),
        &network,
        primary_readiness.clone(),
    );
    assert!(
        wait_for_leadership(&primary_readiness, Duration::from_secs(3)).await,
        "the first instance did not acquire its session advisory lock"
    );

    let standby = LeadershipHandle::spawn(
        Zeroizing::new(database_url),
        &network,
        standby_readiness.clone(),
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(primary_readiness.snapshot().leadership);
    assert!(!standby_readiness.snapshot().leadership);

    primary.shutdown().await;
    assert!(
        wait_for_leadership(&standby_readiness, Duration::from_secs(5)).await,
        "the standby did not acquire leadership after the first session closed"
    );
    assert!(!primary_readiness.snapshot().leadership);

    standby.shutdown().await;
    assert!(!standby_readiness.snapshot().leadership);
    Ok(())
}
