//! Durable recent evidence and bounded, independently retryable reward work.

use crate::beacon_client::types::BlockRoot;
use crate::db::Pool;
use crate::error::Result;

#[derive(Debug)]
pub struct LiveJob {
    pub epoch: u64,
    pub slot: Option<u64>,
    pub component: String,
    pub validators: Vec<i64>,
    pub root: Option<BlockRoot>,
}

pub async fn register_tracking_start(pool: &Pool, validators: &[i64], epoch: u64) -> Result<()> {
    sqlx::query(
        "INSERT INTO live_tracking_start(validator_index,started_epoch)
         SELECT UNNEST($1::BIGINT[]),$2
         ON CONFLICT(validator_index) DO UPDATE SET
           started_epoch=LEAST(live_tracking_start.started_epoch,EXCLUDED.started_epoch)",
    )
    .bind(validators)
    .bind(epoch as i64)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn record_block(
    pool: &Pool,
    slot: u64,
    root: &BlockRoot,
    parent: &BlockRoot,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO live_blocks(slot, root, parent_root) VALUES ($1,$2,$3)
        ON CONFLICT(slot) DO UPDATE SET root=EXCLUDED.root, parent_root=EXCLUDED.parent_root",
    )
    .bind(slot as i64)
    .bind(root.as_str())
    .bind(parent.as_str())
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn recent_blocks(pool: &Pool, min_slot: u64) -> Result<Vec<(u64, BlockRoot, BlockRoot)>> {
    let rows: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT slot, root, parent_root FROM live_blocks WHERE slot >= $1 ORDER BY slot",
    )
    .bind(min_slot as i64)
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|(slot, root, parent)| {
            Ok((
                slot as u64,
                BlockRoot::parse(&root)?,
                BlockRoot::parse(&parent)?,
            ))
        })
        .collect()
}

pub async fn record_coverage(pool: &Pool, validators: &[i64], slot: u64) -> Result<()> {
    sqlx::query(
        "INSERT INTO live_coverage(validator_index,slot)
        SELECT UNNEST($1::BIGINT[]),$2 ON CONFLICT DO NOTHING",
    )
    .bind(validators)
    .bind(slot as i64)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn processed_tip(pool: &Pool, validators: &[i64]) -> Result<Option<u64>> {
    let tip: Option<i64> = sqlx::query_scalar(
        "SELECT CASE WHEN COUNT(*) = $2 THEN MIN(tip) END
        FROM (SELECT validator_index, MAX(slot) AS tip FROM live_coverage
              WHERE validator_index=ANY($1) GROUP BY validator_index) t",
    )
    .bind(validators)
    .bind(validators.len() as i64)
    .fetch_one(pool)
    .await?;
    Ok(tip.map(|s| s as u64))
}

pub async fn missing_coverage_slots(
    pool: &Pool,
    validators: &[i64],
    start: u64,
    end: u64,
    limit: i64,
) -> Result<Vec<u64>> {
    let slots: Vec<i64> = sqlx::query_scalar("SELECT s FROM GENERATE_SERIES($2::BIGINT,$3::BIGINT) s
        WHERE (SELECT COUNT(*) FROM live_coverage c WHERE c.slot=s AND c.validator_index=ANY($1)) < $4
        ORDER BY s DESC LIMIT $5")
        .bind(validators).bind(start as i64).bind(end as i64).bind(validators.len() as i64)
        .bind(limit).fetch_all(pool).await?;
    Ok(slots.into_iter().map(|s| s as u64).collect())
}

/// Called by the serialized worker before writing a replacement branch.
pub async fn invalidate_from(pool: &Pool, replay_from: u64) -> Result<()> {
    let epoch = crate::chain::slot_to_epoch(replay_from) as i64;
    let mut tx = pool.begin().await?;
    for query in [
        "DELETE FROM live_blocks WHERE slot >= $1",
        "DELETE FROM live_coverage WHERE slot >= $1",
        "DELETE FROM block_proposals WHERE slot >= $1 AND NOT finalized",
        "DELETE FROM sync_duties WHERE slot >= $1 AND NOT finalized",
    ] {
        sqlx::query(query)
            .bind(replay_from as i64)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("DELETE FROM attestation_duties WHERE epoch >= $1 AND NOT finalized")
        .bind(epoch)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM live_jobs j WHERE epoch >= $1 AND NOT EXISTS
        (SELECT 1 FROM completed_scans c WHERE c.epoch=j.epoch AND c.validator_index=j.validator_index)")
        .bind(epoch).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn enqueue_jobs(
    pool: &Pool,
    epoch: u64,
    slot: Option<u64>,
    component: &str,
    validators: &[i64],
    root: Option<&BlockRoot>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO live_jobs(validator_index,epoch,slot,component,root)
        SELECT UNNEST($1::BIGINT[]),$2,$3,$4,$5 ON CONFLICT DO NOTHING",
    )
    .bind(validators)
    .bind(epoch as i64)
    .bind(slot.map_or(-1, |s| s as i64))
    .bind(component)
    .bind(root.map(BlockRoot::as_str))
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn pending_jobs(pool: &Pool, min_epoch: u64, limit: i64) -> Result<Vec<LiveJob>> {
    type Row = (i64, i64, String, Vec<i64>, Option<String>);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT epoch,slot,component,
        ARRAY_AGG(validator_index ORDER BY validator_index),root FROM live_jobs
        WHERE status='pending' AND next_attempt<=NOW() AND epoch >= $1
        GROUP BY epoch,slot,component,root ORDER BY epoch DESC, slot DESC LIMIT $2",
    )
    .bind(min_epoch as i64)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|(epoch, slot, component, validators, root)| {
            Ok(LiveJob {
                epoch: epoch as u64,
                slot: (slot >= 0).then_some(slot as u64),
                component,
                validators,
                root: root.map(|r| BlockRoot::parse(&r)).transpose()?,
            })
        })
        .collect()
}

/// Payload and success are committed together, including rewards whose duty row
/// does not exist yet. Result-table updates may be replayed idempotently later.
pub async fn complete_job(
    pool: &Pool,
    job: &LiveJob,
    payload: &serde_json::Value,
    dependency: &BlockRoot,
) -> Result<()> {
    sqlx::query(
        "UPDATE live_jobs SET status='complete',payload=$5,dependency=$6,last_error=NULL
        WHERE epoch=$1 AND slot=$2 AND component=$3 AND validator_index=ANY($4)",
    )
    .bind(job.epoch as i64)
    .bind(job.slot.map_or(-1, |s| s as i64))
    .bind(&job.component)
    .bind(&job.validators)
    .bind(payload)
    .bind(dependency.as_str())
    .execute(pool)
    .await?;
    crate::metrics::LIVE_LAST_REWARD_SUCCESS
        .with_label_values(&[&job.component])
        .set(job.epoch as i64);
    Ok(())
}

pub async fn fail_job(pool: &Pool, job: &LiveJob, error: &str) -> Result<()> {
    sqlx::query("UPDATE live_jobs SET attempts=attempts+1,last_error=$5,
        next_attempt=NOW()+MAKE_INTERVAL(secs => 12 * POWER(2,LEAST(attempts,4))::INT)
        WHERE epoch=$1 AND slot=$2 AND component=$3 AND validator_index=ANY($4) AND status='pending'")
        .bind(job.epoch as i64).bind(job.slot.map_or(-1, |s|s as i64))
        .bind(&job.component).bind(&job.validators).bind(error).execute(pool).await?;
    Ok(())
}

pub async fn expire_jobs(pool: &Pool, min_epoch: u64) -> Result<()> {
    sqlx::query(
        "INSERT INTO live_gaps(validator_index,epoch,reason)
         SELECT validator_index,epoch,'reward retry window expired' FROM live_jobs
         WHERE epoch < $1 AND status='pending'
         ON CONFLICT(validator_index,epoch) DO UPDATE SET
           reason=EXCLUDED.reason,updated_at=NOW()",
    )
    .bind(min_epoch as i64)
    .execute(pool)
    .await?;
    let retired = sqlx::query(
        "UPDATE live_jobs SET status='needs_backfill' WHERE epoch < $1 AND status='pending'",
    )
    .bind(min_epoch as i64)
    .execute(pool)
    .await?
    .rows_affected();
    crate::metrics::LIVE_EPOCHS_INCOMPLETE.inc_by(retired);
    Ok(())
}

/// Keep recent and unfinalized ancestry. Old gaps remain in live_jobs for
/// diagnostics/backfill; complete archive repairs no longer need staged data.
pub async fn prune_completed_evidence(pool: &Pool, retain_epoch: u64) -> Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM live_jobs j WHERE epoch < $1 AND EXISTS
        (SELECT 1 FROM completed_scans c WHERE c.epoch=j.epoch AND c.validator_index=j.validator_index)")
        .bind(retain_epoch as i64).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM live_coverage WHERE slot < $1")
        .bind(crate::chain::epoch_start_slot(retain_epoch) as i64)
        .execute(&mut *tx)
        .await?;
    // Retain one anchor before the floor, including a skipped boundary.
    sqlx::query(
        "DELETE FROM live_blocks WHERE slot <
        (SELECT MAX(slot) FROM live_blocks WHERE slot < $1)",
    )
    .bind(crate::chain::epoch_start_slot(retain_epoch) as i64)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn report_progress(pool: &Pool) -> Result<()> {
    let rows: Vec<(String, i64, Option<i64>)> = sqlx::query_as(
        "SELECT status,COUNT(*),MIN(epoch) FROM live_jobs WHERE status<>'complete' GROUP BY status",
    )
    .fetch_all(pool)
    .await?;
    for status in ["pending", "needs_backfill"] {
        let row = rows.iter().find(|(s, _, _)| s == status);
        crate::metrics::LIVE_PENDING_JOBS
            .with_label_values(&[status])
            .set(row.map_or(0, |(_, n, _)| *n));
        crate::metrics::LIVE_OLDEST_GAP
            .with_label_values(&[status])
            .set(row.and_then(|(_, _, epoch)| *epoch).unwrap_or(-1));
    }
    let gap: (i64, Option<i64>) = sqlx::query_as("SELECT COUNT(*),MIN(epoch) FROM live_gaps")
        .fetch_one(pool)
        .await?;
    crate::metrics::LIVE_GAPS.set(gap.0);
    crate::metrics::LIVE_OLDEST_GAP
        .with_label_values(&["needs_backfill"])
        .set(
            match (rows.iter().find(|(s, _, _)| s == "needs_backfill"), gap.1) {
                (Some((_, _, Some(job_epoch))), Some(gap_epoch)) => (*job_epoch).min(gap_epoch),
                (Some((_, _, Some(job_epoch))), None) => *job_epoch,
                (_, Some(gap_epoch)) => gap_epoch,
                _ => -1,
            },
        );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires a disposable RECOVERY_TEST_DATABASE_URL"]
    async fn retry_progress_survives_failure_and_keeps_new_work_available() {
        let pool = crate::db::isolated_test_pool().await;
        super::super::validators::upsert_validator(&pool, 1, &[1], 0, None)
            .await
            .unwrap();
        enqueue_jobs(&pool, 20, None, "attestation_rewards", &[1], None)
            .await
            .unwrap();
        let job = pending_jobs(&pool, 0, 12).await.unwrap().remove(0);
        fail_job(&pool, &job, "temporary 404").await.unwrap();
        assert!(pending_jobs(&pool, 0, 12).await.unwrap().is_empty());
        enqueue_jobs(&pool, 21, None, "attestation_rewards", &[1], None)
            .await
            .unwrap();
        let newer = pending_jobs(&pool, 0, 12).await.unwrap().remove(0);
        assert_eq!(newer.epoch, 21);
        // Time-based retries work without a new head or an in-memory cursor.
        sqlx::query("UPDATE live_jobs SET next_attempt=NOW() WHERE epoch=20")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(pending_jobs(&pool, 0, 12).await.unwrap().len(), 2);
        let root = BlockRoot::parse(&format!("0x{}", "01".repeat(32))).unwrap();
        complete_job(&pool, &job, &serde_json::json!({"total_rewards":[]}), &root)
            .await
            .unwrap();
        enqueue_jobs(&pool, 20, None, "attestation_rewards", &[1], None)
            .await
            .unwrap();
        assert_eq!(pending_jobs(&pool, 0, 12).await.unwrap().len(), 1);
        expire_jobs(&pool, 22).await.unwrap();
        let rows: Vec<(i64, String, bool)> =
            sqlx::query_as("SELECT epoch,status,payload IS NOT NULL FROM live_jobs ORDER BY epoch")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            rows,
            vec![
                (20, "complete".into(), true),
                (21, "needs_backfill".into(), false)
            ]
        );
        record_coverage(&pool, &[1], 100).await.unwrap();
        record_coverage(&pool, &[1], 102).await.unwrap();
        record_block(
            &pool,
            102,
            &root,
            &BlockRoot::parse(&format!("0x{}", "02".repeat(32))).unwrap(),
        )
        .await
        .unwrap();
        let presence = crate::db::api::live::fetch_block_presence(&pool, &[1], 100, 103)
            .await
            .unwrap();
        assert_eq!(presence.get(&100), Some(&false));
        assert_eq!(presence.get(&101), None);
        assert_eq!(presence.get(&102), Some(&true));
        assert_eq!(
            missing_coverage_slots(&pool, &[1], 100, 102, 12)
                .await
                .unwrap(),
            vec![101]
        );
        assert_eq!(processed_tip(&pool, &[1]).await.unwrap(), Some(102));
        invalidate_from(&pool, 96).await.unwrap();
        assert_eq!(processed_tip(&pool, &[1]).await.unwrap(), None);
        pool.close().await;
    }
}
