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

#[cfg(test)]
pub async fn isolated_test_pool() -> Pool {
    let url =
        std::env::var("RECOVERY_TEST_DATABASE_URL").expect("disposable test database required");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("recovery_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let options: sqlx::postgres::PgConnectOptions = url.parse().unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect_with(options.options([("search_path", schema.as_str())]))
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    admin.close().await;
    pool
}
