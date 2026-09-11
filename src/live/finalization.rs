use std::collections::{HashMap, HashSet};

use crate::beacon_client::BeaconClient;
use crate::beacon_client::types::{BlockRoot, FinalizedCheckpointEvent};
use crate::db::Pool as PgPool;
use crate::db::scanner::live;
use crate::error::{Error, Result};

/// Finality confirms previously collected evidence; it never requires an old
/// state rescan. The independent archive worker repairs incomplete epochs.
pub(super) async fn finalize_collected_evidence(
    client: &BeaconClient,
    pool: &PgPool,
    scan_validators: &HashSet<u64>,
    finalized: &FinalizedCheckpointEvent,
) -> Result<()> {
    let timer = crate::metrics::LIVE_FINALIZED_RESCAN_DURATION
        .with_label_values(&["promote_collected"])
        .start_timer();
    let Some(target) = crate::chain::finalized_scan_target(finalized.epoch) else {
        return Ok(());
    };
    let header = client.get_header(finalized.block.as_str()).await?;
    if header.execution_optimistic == Some(true)
        || !header.data.canonical
        || header.data.root != finalized.block
    {
        return Err(Error::InconsistentBeaconData(
            "Finalized root is not verified canonical".into(),
        ));
    }
    let blocks = live::recent_blocks(pool, 0).await?;
    let confirmed = confirmed_ancestry(&blocks, &finalized.block);
    if confirmed.is_empty() {
        tracing::debug!(
            target,
            "Finalized checkpoint is not connected to collected evidence yet"
        );
        return Ok(());
    }
    let floor = confirmed.iter().map(|(slot, _)| *slot).min().unwrap();
    let roots: Vec<String> = confirmed
        .into_iter()
        .map(|(_, root)| root.to_string())
        .collect();
    let indices: Vec<i64> = scan_validators.iter().map(|v| *v as i64).collect();
    let completed = promote_collected(pool, &indices, target, floor, &roots).await?;
    tracing::debug!(
        target,
        completed,
        "Verified live evidence against finalized ancestry"
    );
    timer.observe_duration();
    Ok(())
}

/// Only the uninterrupted parent chain ending at the checkpoint is confirmed.
/// Disconnected ranges, even at lower slot numbers, cannot earn finality.
fn confirmed_ancestry(
    blocks: &[(u64, BlockRoot, BlockRoot)],
    root: &BlockRoot,
) -> Vec<(u64, BlockRoot)> {
    let by_root: HashMap<_, _> = blocks
        .iter()
        .map(|(slot, root, parent)| (root, (*slot, parent)))
        .collect();
    let mut confirmed = Vec::new();
    let mut cursor = root;
    let mut child_slot = None;
    while let Some(&(slot, parent)) = by_root.get(cursor) {
        if child_slot.is_some_and(|child| slot >= child) {
            break;
        }
        confirmed.push((slot, cursor.clone()));
        child_slot = Some(slot);
        cursor = parent;
    }
    confirmed
}

async fn promote_collected(
    pool: &PgPool,
    validators: &[i64],
    target: u64,
    floor: u64,
    roots: &[String],
) -> Result<u64> {
    let spe = crate::chain::slots_per_epoch() as i64;
    let mut tx = pool.begin().await?;

    // Replay staged responses only on the confirmed branch. A successful fetch
    // survives missing duty rows or a crash between staging and its first join.
    sqlx::query(
        "UPDATE attestation_duties a SET
        source_reward=(r->>'source')::BIGINT,target_reward=(r->>'target')::BIGINT,
        head_reward=(r->>'head')::BIGINT,inactivity_penalty=(r->>'inactivity')::BIGINT,
        source_correct=CASE WHEN (r->>'source')::BIGINT > 0 THEN TRUE ELSE a.source_correct END,
        target_correct=CASE WHEN (r->>'target')::BIGINT > 0 THEN TRUE ELSE a.target_correct END,
        head_correct=CASE WHEN (r->>'head')::BIGINT > 0 THEN TRUE ELSE a.head_correct END
        FROM live_jobs j CROSS JOIN LATERAL JSONB_ARRAY_ELEMENTS(j.payload->'total_rewards') r
        WHERE j.component='attestation_rewards' AND j.status='complete'
          AND j.dependency=ANY($1) AND j.validator_index=ANY($2) AND j.epoch<=$3
          AND a.validator_index=j.validator_index AND a.epoch=j.epoch
          AND (r->>'validator_index')::BIGINT=j.validator_index
          AND NOT EXISTS (SELECT 1 FROM completed_scans c
              WHERE c.validator_index=a.validator_index AND c.epoch=a.epoch)",
    )
    .bind(roots)
    .bind(validators)
    .bind(target as i64)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE block_proposals p SET reward_total=(j.payload->>'total')::BIGINT,
        reward_attestations=(j.payload->>'attestations')::BIGINT,
        reward_sync=(j.payload->>'sync_aggregate')::BIGINT,
        reward_slashings=(j.payload->>'proposer_slashings')::BIGINT+(j.payload->>'attester_slashings')::BIGINT
        FROM live_jobs j WHERE j.component='proposal_rewards' AND j.status='complete'
          AND j.dependency=ANY($1) AND j.validator_index=ANY($2) AND j.epoch<=$3
          AND p.slot=j.slot AND p.proposer_index=j.validator_index
          AND (j.payload->>'proposer_index')::BIGINT=p.proposer_index
          AND NOT EXISTS (SELECT 1 FROM completed_scans c
              WHERE c.validator_index=p.proposer_index AND c.epoch=j.epoch)")
        .bind(roots).bind(validators).bind(target as i64).execute(&mut *tx).await?;
    sqlx::query(
        "UPDATE sync_duties s SET reward=(r->>'reward')::BIGINT
        FROM live_jobs j CROSS JOIN LATERAL JSONB_ARRAY_ELEMENTS(j.payload) r
        WHERE j.component='sync_rewards' AND j.status='complete'
          AND j.dependency=ANY($1) AND j.validator_index=ANY($2) AND j.epoch<=$3
          AND s.slot=j.slot AND s.validator_index=j.validator_index
          AND (r->>'validator_index')::BIGINT=j.validator_index
          AND NOT EXISTS (SELECT 1 FROM completed_scans c
              WHERE c.validator_index=s.validator_index AND c.epoch=j.epoch)",
    )
    .bind(roots)
    .bind(validators)
    .bind(target as i64)
    .execute(&mut *tx)
    .await?;

    // Inclusion can be known even when a reward endpoint is unavailable.
    sqlx::query(
        "UPDATE attestation_duties a SET inclusion_known=TRUE
        WHERE a.validator_index=ANY($1) AND a.epoch<=$2 AND a.epoch*$3 >= $4
          AND (SELECT COUNT(*) FROM live_coverage c WHERE c.validator_index=a.validator_index
               AND c.slot >= a.epoch*$3 AND c.slot < (a.epoch+2)*$3) = 2*$3
          AND (NOT a.included OR EXISTS (SELECT 1 FROM live_blocks b
               WHERE b.slot=a.inclusion_slot AND b.root=ANY($5)))",
    )
    .bind(validators)
    .bind(target as i64)
    .bind(spe)
    .bind(floor as i64)
    .bind(roots)
    .execute(&mut *tx)
    .await?;

    // Chain finality and data completeness are separate. Freeze every outcome
    // whose slot/window is proven on finalized ancestry, even if a reward is
    // still pending and the epoch cannot yet earn completed_scans.
    sqlx::query(
        "UPDATE attestation_duties a SET finalized=TRUE
         WHERE a.validator_index=ANY($1) AND a.epoch<=$2 AND a.epoch*$3 >= $4
           AND a.inclusion_known
           AND (SELECT COUNT(*) FROM live_coverage c WHERE c.validator_index=a.validator_index
                AND c.slot >= a.epoch*$3 AND c.slot < (a.epoch+2)*$3) = 2*$3
           AND (NOT a.included OR EXISTS (SELECT 1 FROM live_blocks b
                WHERE b.slot=a.inclusion_slot AND b.root=ANY($5)))",
    )
    .bind(validators)
    .bind(target as i64)
    .bind(spe)
    .bind(floor as i64)
    .bind(roots)
    .execute(&mut *tx)
    .await?;
    for table in ["block_proposals", "sync_duties"] {
        let owner = if table == "block_proposals" {
            "proposer_index"
        } else {
            "validator_index"
        };
        let present = if table == "block_proposals" {
            "proposed"
        } else {
            "NOT missed_block"
        };
        let sql = format!(
            "UPDATE {table} d SET finalized=TRUE
             WHERE d.{owner}=ANY($1) AND d.slot/$2 <= $3 AND d.slot >= $4
               AND EXISTS (SELECT 1 FROM live_coverage c
                    WHERE c.validator_index=d.{owner} AND c.slot=d.slot)
               AND (({present} AND EXISTS (SELECT 1 FROM live_blocks b
                         WHERE b.slot=d.slot AND b.root=ANY($5)))
                    OR (NOT ({present}) AND NOT EXISTS
                         (SELECT 1 FROM live_blocks b WHERE b.slot=d.slot)))"
        );
        sqlx::query(&sql)
            .bind(validators)
            .bind(spe)
            .bind(target as i64)
            .bind(floor as i64)
            .bind(roots)
            .execute(&mut *tx)
            .await?;
    }

    let ready: Vec<(i64,i64)> = sqlx::query_as("SELECT a.validator_index,a.epoch
        FROM attestation_duties a WHERE a.validator_index=ANY($1) AND a.epoch<=$2
          AND a.epoch*$3 >= $4 AND a.inclusion_known
          AND a.source_reward IS NOT NULL AND a.target_reward IS NOT NULL AND a.head_reward IS NOT NULL
          AND (SELECT COUNT(*) FROM live_coverage c WHERE c.validator_index=a.validator_index
               AND c.slot >= a.epoch*$3 AND c.slot < (a.epoch+2)*$3) = 2*$3
          AND EXISTS (SELECT 1 FROM live_jobs j WHERE j.validator_index=a.validator_index
              AND j.epoch=a.epoch AND j.component='attestation_rewards' AND j.status='complete'
              AND j.dependency=ANY($5))
          AND NOT EXISTS (SELECT 1 FROM live_jobs j WHERE j.validator_index=a.validator_index
              AND j.epoch=a.epoch AND (j.status<>'complete' OR j.dependency IS NULL OR NOT(j.dependency=ANY($5))))
          AND NOT EXISTS (SELECT 1 FROM block_proposals p WHERE p.proposer_index=a.validator_index
              AND p.slot >= a.epoch*$3 AND p.slot < (a.epoch+1)*$3 AND p.proposed
              AND (p.reward_total IS NULL OR p.reward_attestations IS NULL OR p.reward_sync IS NULL
                   OR p.reward_slashings IS NULL OR NOT EXISTS
                   (SELECT 1 FROM live_jobs j WHERE j.validator_index=a.validator_index AND j.slot=p.slot
                    AND j.component='proposal_rewards' AND j.status='complete' AND j.dependency=ANY($5))))
          AND NOT EXISTS (SELECT 1 FROM sync_duties s WHERE s.validator_index=a.validator_index
              AND s.slot >= a.epoch*$3 AND s.slot < (a.epoch+1)*$3 AND NOT s.missed_block
              AND (s.reward IS NULL OR NOT EXISTS
                   (SELECT 1 FROM live_jobs j WHERE j.validator_index=a.validator_index AND j.slot=s.slot
                    AND j.component='sync_rewards' AND j.status='complete' AND j.dependency=ANY($5))))
          AND NOT EXISTS (SELECT 1 FROM completed_scans c WHERE c.validator_index=a.validator_index AND c.epoch=a.epoch)")
        .bind(validators).bind(target as i64).bind(spe).bind(floor as i64).bind(roots)
        .fetch_all(&mut *tx).await?;
    for &(validator, epoch) in &ready {
        sqlx::query(
            "UPDATE attestation_duties SET finalized=TRUE WHERE validator_index=$1 AND epoch=$2",
        )
        .bind(validator)
        .bind(epoch)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE block_proposals SET finalized=TRUE WHERE proposer_index=$1 AND slot >= $2 AND slot < $3")
            .bind(validator).bind(epoch*spe).bind((epoch+1)*spe).execute(&mut *tx).await?;
        sqlx::query("UPDATE sync_duties SET finalized=TRUE WHERE validator_index=$1 AND slot >= $2 AND slot < $3")
            .bind(validator).bind(epoch*spe).bind((epoch+1)*spe).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO completed_scans(validator_index,epoch) VALUES($1,$2) ON CONFLICT DO NOTHING")
            .bind(validator).bind(epoch).execute(&mut *tx).await?;
        sqlx::query("UPDATE validators SET last_scanned_epoch=GREATEST(last_scanned_epoch,$2) WHERE validator_index=$1")
            .bind(validator).bind(epoch).execute(&mut *tx).await?;
    }
    // Every finalized epoch since this validator began live tracking must be
    // either complete or explicitly repairable by the archive worker.
    sqlx::query(
        "INSERT INTO live_gaps(validator_index,epoch,reason)
         SELECT v.validator_index,e,'finalized live collection incomplete'
         FROM live_tracking_start t
         JOIN validators v USING(validator_index)
         CROSS JOIN LATERAL GENERATE_SERIES(t.started_epoch,$2::BIGINT) e
         WHERE v.validator_index=ANY($1) AND v.activation_epoch<=e
           AND (v.exit_epoch IS NULL OR v.exit_epoch>e)
           AND NOT EXISTS (SELECT 1 FROM completed_scans c
               WHERE c.validator_index=v.validator_index AND c.epoch=e)
         ON CONFLICT(validator_index,epoch) DO UPDATE SET
           reason=EXCLUDED.reason,updated_at=NOW()",
    )
    .bind(validators)
    .bind(target as i64)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(ready.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn root(byte: u8) -> BlockRoot {
        BlockRoot::parse(&format!("0x{}", format!("{byte:02x}").repeat(32))).unwrap()
    }
    #[test]
    fn finality_requires_connected_ancestry_not_just_older_slots() {
        let blocks = vec![
            (1, root(1), root(0)),
            (3, root(3), root(2)),
            (4, root(4), root(3)),
        ];
        let confirmed = confirmed_ancestry(&blocks, &root(4));
        assert_eq!(confirmed, vec![(4, root(4)), (3, root(3))]);
        assert!(confirmed_ancestry(&blocks, &root(5)).is_empty());
    }

    #[tokio::test]
    #[ignore = "requires a disposable RECOVERY_TEST_DATABASE_URL"]
    async fn staged_rewards_finalize_without_history_only_after_complete_coverage() {
        use crate::db::scanner::{attestations, proposals, sync, validators};
        let pool = crate::db::isolated_test_pool().await;
        let validator = 99190;
        validators::upsert_validator(&pool, validator, &[19], 0, None)
            .await
            .unwrap();
        let epoch = 20u64;
        let start = crate::chain::epoch_start_slot(epoch);
        let end = crate::chain::epoch_start_slot(epoch + 2);
        for slot in start..=end {
            live::record_block(
                &pool,
                slot,
                &root((slot - start + 1) as u8),
                &root((slot - start) as u8),
            )
            .await
            .unwrap();
            if slot < end - 1 {
                live::record_coverage(&pool, &[validator], slot)
                    .await
                    .unwrap();
            }
        }
        live::enqueue_jobs(
            &pool,
            epoch,
            None,
            "attestation_rewards",
            &[validator],
            None,
        )
        .await
        .unwrap();
        let job = live::pending_jobs(&pool, epoch, 10)
            .await
            .unwrap()
            .remove(0);
        live::complete_job(&pool, &job, &serde_json::json!({"ideal_rewards":[],"total_rewards":[{
            "validator_index":validator.to_string(),"source":"10","target":"20","head":"0","inactivity":"0"
        }]}), &root(64)).await.unwrap();
        // The response is durable before its duty row exists.
        attestations::upsert_attestation_duty(
            &pool,
            validator,
            epoch as i64,
            start as i64,
            0,
            0,
            true,
            Some((start + 1) as i64),
            Some(1),
            Some(1),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            false,
        )
        .await
        .unwrap();
        proposals::upsert_block_proposal(
            &pool,
            (start + 1) as i64,
            validator,
            true,
            None,
            None,
            None,
            None,
            false,
        )
        .await
        .unwrap();
        sync::upsert_sync_duty(
            &pool,
            validator,
            (start + 2) as i64,
            true,
            None,
            false,
            false,
        )
        .await
        .unwrap();
        for (component, slot, payload) in [
            (
                "proposal_rewards",
                start + 1,
                serde_json::json!({"proposer_index":validator.to_string(),"total":"9",
                "attestations":"4","sync_aggregate":"3","proposer_slashings":"1","attester_slashings":"1"}),
            ),
            (
                "sync_rewards",
                start + 2,
                serde_json::json!([{"validator_index":validator.to_string(),"reward":"6"}]),
            ),
        ] {
            live::enqueue_jobs(
                &pool,
                epoch,
                Some(slot),
                component,
                &[validator],
                Some(&root((slot - start + 1) as u8)),
            )
            .await
            .unwrap();
            let job = live::pending_jobs(&pool, epoch, 10)
                .await
                .unwrap()
                .remove(0);
            live::complete_job(&pool, &job, &payload, &root((slot - start + 1) as u8))
                .await
                .unwrap();
        }
        let roots: Vec<String> = (1..=65).map(|i| root(i).to_string()).collect();
        assert_eq!(
            promote_collected(&pool, &[validator], epoch, start, &roots)
                .await
                .unwrap(),
            0
        );
        live::record_coverage(&pool, &[validator], end - 1)
            .await
            .unwrap();
        assert_eq!(
            promote_collected(&pool, &[validator], epoch, start, &roots)
                .await
                .unwrap(),
            1
        );
        let row:(i64,bool,bool) = sqlx::query_as("SELECT source_reward,finalized,inclusion_known FROM attestation_duties WHERE validator_index=$1")
            .bind(validator).fetch_one(&pool).await.unwrap();
        assert_eq!(row, (10, true, true));
        let rewards: (i64, i64) = sqlx::query_as(
            "SELECT (SELECT reward_total FROM block_proposals),(SELECT reward FROM sync_duties)",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(rewards, (9, 6));
        assert_eq!(
            promote_collected(&pool, &[validator], epoch, start, &roots)
                .await
                .unwrap(),
            0
        );
        live::invalidate_from(&pool, start).await.unwrap();
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM attestation_duties WHERE finalized")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 1);
        pool.close().await;
    }

    #[tokio::test]
    #[ignore = "requires a disposable RECOVERY_TEST_DATABASE_URL"]
    async fn finalized_incomplete_evidence_stays_explicitly_repairable() {
        use crate::db::scanner::{attestations, validators};
        let pool = crate::db::isolated_test_pool().await;
        let validator = 99191;
        let epoch = 20u64;
        let start = crate::chain::epoch_start_slot(epoch);
        let end = crate::chain::epoch_start_slot(epoch + 2);
        validators::upsert_validator(&pool, validator, &[19], 0, None)
            .await
            .unwrap();
        live::register_tracking_start(&pool, &[validator], epoch)
            .await
            .unwrap();
        attestations::upsert_attestation_duty(
            &pool,
            validator,
            epoch as i64,
            start as i64,
            0,
            0,
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            false,
        )
        .await
        .unwrap();
        for slot in start..end {
            live::record_coverage(&pool, &[validator], slot)
                .await
                .unwrap();
        }

        assert_eq!(
            promote_collected(&pool, &[validator], epoch, start, &[])
                .await
                .unwrap(),
            0
        );
        let status: (bool, bool) = sqlx::query_as(
            "SELECT finalized,inclusion_known FROM attestation_duties
             WHERE validator_index=$1 AND epoch=$2",
        )
        .bind(validator)
        .bind(epoch as i64)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(status, (true, true));
        let gap: String = sqlx::query_scalar(
            "SELECT reason FROM live_gaps WHERE validator_index=$1 AND epoch=$2",
        )
        .bind(validator)
        .bind(epoch as i64)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(gap, "finalized live collection incomplete");
        let completed: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM completed_scans WHERE validator_index=$1 AND epoch=$2",
        )
        .bind(validator)
        .bind(epoch as i64)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(completed, 0);
        pool.close().await;
    }
}
