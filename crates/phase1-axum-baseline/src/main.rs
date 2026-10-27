use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::env;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Serialize, Deserialize)]
pub struct Record {
    pub id: i64,
    pub payload: String,
    pub updated_at: i64,
}

#[derive(Debug, Deserialize)]
pub struct CreateRecordRequest {
    pub payload: String,
}

#[derive(Debug, Serialize)]
pub struct CreateRecordResponse {
    pub id: i64,
}

#[derive(Clone)]
pub struct AppState {
    pub pool: Arc<Pool<SqliteConnectionManager>>,
}

enum AppError {
    NotFound,
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            AppError::NotFound => (StatusCode::NOT_FOUND, "Record Not Found").into_response(),
            AppError::Internal(err) => {
                (StatusCode::INTERNAL_SERVER_ERROR, format!("Error: {err}")).into_response()
            }
        }
    }
}

async fn get_record(
    Path(id): Path<i64>,
    State(state): State<AppState>,
) -> Result<Json<Record>, AppError> {
    let pool = state.pool.clone();
    tokio::task::spawn_blocking(move || {
        let conn = pool
            .get()
            .map_err(|e| AppError::Internal(e.to_string()))?;
        let mut stmt = conn
            .prepare("SELECT id, payload, updated_at FROM records WHERE id = ?1")
            .map_err(|e| AppError::Internal(e.to_string()))?;
        let record = stmt
            .query_row(params![id], |row| {
                Ok(Record {
                    id: row.get(0)?,
                    payload: row.get(1)?,
                    updated_at: row.get(2)?,
                })
            })
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => AppError::NotFound,
                _ => AppError::Internal(e.to_string()),
            })?;
        Ok(Json(record))
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?
}

async fn create_record(
    State(state): State<AppState>,
    Json(req): Json<CreateRecordRequest>,
) -> Result<(StatusCode, Json<CreateRecordResponse>), AppError> {
    let pool = state.pool.clone();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    tokio::task::spawn_blocking(move || {
        let conn = pool
            .get()
            .map_err(|e| AppError::Internal(e.to_string()))?;
        conn.execute(
            "INSERT INTO records (payload, updated_at) VALUES (?1, ?2)",
            params![req.payload, now],
        )
        .map_err(|e| AppError::Internal(e.to_string()))?;

        let id = conn.last_insert_rowid();
        Ok((StatusCode::CREATED, Json(CreateRecordResponse { id })))
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?
}

fn init_db(pool: &Pool<SqliteConnectionManager>) -> Result<(), rusqlite::Error> {
    let conn = pool.get().expect("Failed to get db connection for migration");
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA busy_timeout = 5000;
         CREATE TABLE IF NOT EXISTS records (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             payload TEXT NOT NULL,
             updated_at INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_records_updated_at ON records(updated_at);",
    )?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let db_path = env::var("DATABASE_URL").unwrap_or_else(|_| "phase1.db".to_string());
    let port = env::var("PORT").unwrap_or_else(|_| "3000".to_string());
    let pool_size: u32 = env::var("POOL_SIZE")
        .unwrap_or_else(|_| "64".to_string())
        .parse()
        .unwrap_or(64);

    let manager = SqliteConnectionManager::file(&db_path);
    let pool = Pool::builder()
        .max_size(pool_size)
        .build(manager)
        .expect("Failed to create connection pool");

    init_db(&pool).expect("Database initialization failed");

    let state = AppState {
        pool: Arc::new(pool),
    };

    let app = Router::new()
        .route("/record/:id", get(get_record))
        .route("/record", post(create_record))
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], port.parse::<u16>()?));
    println!("Phase 1 Axum server listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
