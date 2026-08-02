use pc_db::{Db, Migrator};
use std::time::Instant;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_migrate_test".into()
    });
    let db = Db::connect(&url, 8, 1).await?;
    let start = Instant::now();
    Migrator::run(&db).await?;
    let status = Migrator::status(&db).await?;
    println!("MIGRATED in {:?}", start.elapsed());
    println!(
        "available={} applied={} pending={}",
        status.available,
        status.applied,
        status.pending.len()
    );
    Ok(())
}
