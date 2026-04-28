pub mod api;
pub mod scanner;

use sqlx::postgres::PgPoolOptions;

use crate::error::Result;

/// Database connection pool. Aliased through the `db` module so callers
/// outside `db::` don't need to depend on `sqlx` directly.
pub type Pool = sqlx::PgPool;

pub async fn connect(database_url: &str) -> Result<Pool> {
    tracing::debug!("Connecting to database");
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(database_url)
        .await?;
    tracing::debug!("Database connection established");

    tracing::debug!("Running database migrations");
    sqlx::migrate!("./migrations").run(&pool).await?;
    tracing::info!("Database connected and migrations applied");

    Ok(pool)
}
