//! Validator-table writes + scanner-internal reads.

use sqlx::Row;

use crate::db::Pool;
use crate::error::Result;

pub struct ValidatorRow {
    pub validator_index: i64,
    pub activation_epoch: i64,
    pub exit_epoch: Option<i64>,
    pub last_scanned_epoch: Option<i64>,
}

/// Insert or update a validator. Does NOT overwrite `last_scanned_epoch` if the
/// validator already exists (preserves scan progress across restarts).
pub async fn upsert_validator(
    pool: &Pool,
    validator_index: i64,
    pubkey: &[u8],
    activation_epoch: i64,
    exit_epoch: Option<i64>,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO validators (validator_index, pubkey, activation_epoch, exit_epoch, updated_at)
        VALUES ($1, $2, $3, $4, NOW())
        ON CONFLICT (validator_index) DO UPDATE SET
            pubkey = EXCLUDED.pubkey,
            activation_epoch = EXCLUDED.activation_epoch,
            exit_epoch = EXCLUDED.exit_epoch,
            updated_at = NOW()
        "#,
    )
    .bind(validator_index)
    .bind(pubkey)
    .bind(activation_epoch)
    .bind(exit_epoch)
    .execute(pool)
    .await?;
    Ok(())
}

/// Load all tracked validators from the DB.
pub async fn get_all_validators(pool: &Pool) -> Result<Vec<ValidatorRow>> {
    let rows = sqlx::query(
        "SELECT validator_index, activation_epoch, exit_epoch, last_scanned_epoch FROM validators",
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .iter()
        .map(|r| ValidatorRow {
            validator_index: r.get("validator_index"),
            activation_epoch: r.get("activation_epoch"),
            exit_epoch: r.get("exit_epoch"),
            last_scanned_epoch: r.get("last_scanned_epoch"),
        })
        .collect())
}

/// Batch update scan watermark for multiple validators. Uses `GREATEST` so a
/// later watermark can't be rewound by a concurrent pass scanning older epochs.
pub async fn update_validators_scanned_epoch(
    pool: &Pool,
    validator_indices: &[i64],
    epoch: i64,
) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE validators
        SET last_scanned_epoch = GREATEST(COALESCE(last_scanned_epoch, -1), $2)
        WHERE validator_index = ANY($1)
        "#,
    )
    .bind(validator_indices)
    .bind(epoch)
    .execute(pool)
    .await?;
    Ok(())
}
