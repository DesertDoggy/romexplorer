//! Server/database status for a dashboard: connection, version, row counts, job counts,
//! provider availability.

use serde::Serialize;
use sqlx::Row;

use super::{db, jobs, providers, App, Result};

#[derive(Debug, Clone, Serialize)]
pub struct DatabaseInfo {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub database: String,
    pub server_version: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub app_root: String,
    pub database: DatabaseInfo,
    pub tables: Vec<db::TableCount>,
    pub jobs: Vec<jobs::StatusCount>,
    pub providers: Vec<providers::ProviderInfo>,
    pub directories: u64,
    pub local_files: u64,
}

pub async fn status(app: &App) -> Result<Status> {
    let version: String = sqlx::query("SELECT VERSION() AS v").fetch_one(&app.pool).await?.try_get("v")?;
    let directories: i64 = sqlx::query("SELECT COUNT(*) AS n FROM local_directories WHERE deleted = FALSE").fetch_one(&app.pool).await?.try_get("n")?;
    let local_files: i64 = sqlx::query("SELECT COUNT(*) AS n FROM local_files WHERE deleted = FALSE").fetch_one(&app.pool).await?.try_get("n")?;
    Ok(Status {
        app_root: app.paths.root.display().to_string(),
        database: DatabaseInfo { host: app.config.host.clone(), port: app.config.port, user: app.config.user.clone(), database: app.config.database.clone(), server_version: version },
        tables: db::table_counts(app).await?,
        jobs: jobs::counts(app).await?,
        providers: providers::list(app).await?,
        directories: directories as u64,
        local_files: local_files as u64,
    })
}
