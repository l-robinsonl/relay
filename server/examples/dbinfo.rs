//! Read-only diagnostic: reports what a Postgres database contains.
//!
//! Run before pointing Relay at a database, so that an existing
//! application's schema is never migrated over by accident.
//!
//! ```sh
//! cargo run -p relay-server --example dbinfo -- "$DATABASE_URL"
//! ```

use sqlx::{Row, postgres::PgPoolOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .ok_or("pass a connection URL, or set DATABASE_URL")?;

    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(30))
        .connect(&url)
        .await?;

    let version: String = sqlx::query("SELECT version()")
        .fetch_one(&pool)
        .await?
        .get(0);
    println!(
        "server:   {}",
        version.split(" on ").next().unwrap_or(&version)
    );

    let database: String = sqlx::query("SELECT current_database()")
        .fetch_one(&pool)
        .await?
        .get(0);
    let user: String = sqlx::query("SELECT current_user")
        .fetch_one(&pool)
        .await?
        .get(0);
    println!("database: {database}");
    println!("user:     {user}");

    let rows = sqlx::query(
        "SELECT schemaname, tablename
         FROM pg_tables
         WHERE schemaname NOT IN ('pg_catalog', 'information_schema')
         ORDER BY schemaname, tablename",
    )
    .fetch_all(&pool)
    .await?;

    if rows.is_empty() {
        println!("tables:   none — this database is empty");
    } else {
        println!("tables:   {}", rows.len());
        for row in &rows {
            let schema: String = row.get("schemaname");
            let table: String = row.get("tablename");
            println!("  - {schema}.{table}");
        }
    }

    Ok(())
}
