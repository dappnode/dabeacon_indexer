//! Instance-table writes: per-process registration + heartbeat. Used for
//! observability only (multiple instances coexist; see `scan_epoch`'s doc for
//! the coordination invariants).

use uuid::Uuid;

use crate::db::Pool;
use crate::error::Result;

pub async fn register_instance(pool: &Pool, instance_id: Uuid) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO instances (instance_id, started_at, heartbeat)
        VALUES ($1, NOW(), NOW())
        ON CONFLICT (instance_id) DO UPDATE SET heartbeat = NOW()
        "#,
    )
    .bind(instance_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn update_heartbeat(pool: &Pool, instance_id: Uuid) -> Result<()> {
    sqlx::query("UPDATE instances SET heartbeat = NOW() WHERE instance_id = $1")
        .bind(instance_id)
        .execute(pool)
        .await?;
    Ok(())
}
