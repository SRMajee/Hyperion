use rusqlite::{params, Connection, OpenFlags};
use std::time::{SystemTime, UNIX_EPOCH};

#[allow(dead_code)]
pub struct Record {
    pub id: i64,
    pub payload: String,
    pub updated_at: i64,
}

pub fn init_db(path: &str) -> std::io::Result<()> {
    let conn = Connection::open(path).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA temp_store = MEMORY;
         PRAGMA mmap_size = 268435456;
         PRAGMA cache_size = -64000;
         
         CREATE TABLE IF NOT EXISTS records (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             payload TEXT NOT NULL,
             updated_at INTEGER NOT NULL
         );
         
         CREATE INDEX IF NOT EXISTS idx_records_id ON records(id);",
    )
    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

    // Seed 1000 initial records if empty
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM records", [], |r| r.get(0))
        .unwrap_or(0);

    if count < 1000 {
        let mut stmt = conn
            .prepare("INSERT INTO records (payload, updated_at) VALUES (?1, ?2)")
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        conn.execute_batch("BEGIN TRANSACTION;").ok();
        for i in 1..=1000 {
            let p = format!("Initial seed record {}", i);
            stmt.execute(params![p, now]).ok();
        }
        conn.execute_batch("COMMIT;").ok();
    }

    Ok(())
}

pub fn open_readonly(path: &str) -> Result<Connection, rusqlite::Error> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;

    conn.execute_batch(
        "PRAGMA query_only = 1;
         PRAGMA synchronous = OFF;
         PRAGMA temp_store = MEMORY;
         PRAGMA mmap_size = 268435456;",
    )?;

    Ok(conn)
}

pub fn open_writer(path: &str) -> Result<Connection, rusqlite::Error> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;

    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA temp_store = MEMORY;
         PRAGMA mmap_size = 268435456;",
    )?;

    Ok(conn)
}
