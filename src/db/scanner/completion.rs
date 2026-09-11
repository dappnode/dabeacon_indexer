use crate::db::Pool;
use crate::error::Result;

/// Called only after every stage of a finalized scan has succeeded.
pub async fn mark_complete(pool: &Pool, validators: &[i64], epoch: i64) -> Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE attestation_duties SET inclusion_known=TRUE WHERE validator_index=ANY($1) AND epoch=$2")
        .bind(validators).bind(epoch).execute(&mut *tx).await?;
    sqlx::query(
        "INSERT INTO completed_scans (validator_index, epoch)
         SELECT UNNEST($1::BIGINT[]), $2
         ON CONFLICT DO NOTHING",
    )
    .bind(validators)
    .bind(epoch)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM live_gaps WHERE validator_index=ANY($1) AND epoch=$2")
        .bind(validators)
        .bind(epoch)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}
