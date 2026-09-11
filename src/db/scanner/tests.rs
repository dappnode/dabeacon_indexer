//! Run against a disposable PostgreSQL database with RECOVERY_TEST_DATABASE_URL.
use super::{attestations, completion, live, proposals, sync, validators};

#[tokio::test]
#[ignore = "requires a disposable RECOVERY_TEST_DATABASE_URL"]
async fn incomplete_scans_remain_repairable_and_reorgs_preserve_finalized_rows() {
    let pool = crate::db::isolated_test_pool().await;
    validators::upsert_validator(&pool, 42, &[42], 0, None)
        .await
        .unwrap();

    // A live inclusion is promoted despite missing rewards. Finality alone
    // must not make the epoch disappear from the repair queue.
    write_attestation(&pool, 2, 66, None, false).await;
    proposals::upsert_block_proposal(&pool, 67, 42, true, None, None, None, None, false)
        .await
        .unwrap();
    sync::upsert_sync_duty(&pool, 42, 68, true, None, false, false)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE attestation_duties SET finalized=TRUE WHERE validator_index=42 AND epoch=2",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(
        attestations::validators_with_completed_scan(&pool, &[42], 2)
            .await
            .unwrap()
            .is_empty()
    );

    // A partial finalized scan writes attestations, then fails before the
    // other stages. It still must not count as coverage.
    write_attestation(&pool, 2, 66, Some(10), true).await;
    assert_eq!(
        attestations::count_covered_validator_epochs(&pool, &[(42, 2, 2)])
            .await
            .unwrap(),
        0
    );
    write_attestation(&pool, 2, 66, Some(20), true).await;
    proposals::upsert_block_proposal(
        &pool,
        67,
        42,
        true,
        Some(30),
        Some(30),
        Some(0),
        Some(0),
        true,
    )
    .await
    .unwrap();
    sync::upsert_sync_duty(&pool, 42, 68, true, Some(40), false, true)
        .await
        .unwrap();
    completion::mark_complete(&pool, &[42], 2).await.unwrap();
    assert_eq!(
        attestations::count_covered_validator_epochs(&pool, &[(42, 2, 2)])
            .await
            .unwrap(),
        1
    );

    // Completed rows resist both live writes and repeated finalized writes.
    write_attestation(&pool, 2, 66, Some(99), true).await;
    sync::upsert_sync_duty(&pool, 42, 68, false, None, false, false)
        .await
        .unwrap();
    let reward: i64 = sqlx::query_scalar(
        "SELECT source_reward FROM attestation_duties WHERE validator_index=42 AND epoch=2",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(reward, 20);
    let reward: i64 =
        sqlx::query_scalar("SELECT reward FROM sync_duties WHERE validator_index=42 AND slot=68")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(reward, 40);

    // Checkpoint 5 makes scan epoch 3 safe, not epoch 4 or 5.
    write_attestation(&pool, 3, 99, Some(1), false).await;
    write_attestation(&pool, 4, 130, None, false).await;
    sqlx::query(
        "UPDATE attestation_duties SET finalized=TRUE WHERE validator_index=42 AND epoch <= $1",
    )
    .bind(crate::chain::finalized_scan_target(5).unwrap() as i64)
    .execute(&pool)
    .await
    .unwrap();
    let flags: Vec<(i64, bool)> = sqlx::query_as(
        "SELECT epoch, finalized FROM attestation_duties WHERE validator_index=42 ORDER BY epoch",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(flags, vec![(2, true), (3, true), (4, false)]);

    // Replay starts at the previous epoch boundary. Both early inclusions and
    // late inclusions are deleted together; finalized history survives.
    live::invalidate_from(&pool, 96).await.unwrap();
    let epochs: Vec<i64> = sqlx::query_scalar(
        "SELECT epoch FROM attestation_duties WHERE validator_index=42 ORDER BY epoch",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(epochs, vec![2, 3]);
    pool.close().await;
}

async fn write_attestation(
    pool: &crate::db::Pool,
    epoch: i64,
    slot: i64,
    reward: Option<i64>,
    finalized: bool,
) {
    attestations::upsert_attestation_duty(
        pool,
        42,
        epoch,
        slot - 1,
        0,
        0,
        true,
        Some(slot),
        Some(1),
        Some(1),
        None,
        None,
        None,
        reward,
        reward,
        reward,
        Some(0),
        finalized,
    )
    .await
    .unwrap();
}
