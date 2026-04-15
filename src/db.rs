use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::io;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct AppSettings {
    pub model_dir: String,
    pub image_dir: String,
}

#[derive(Debug, Clone)]
pub struct IndexedImageSnapshot {
    pub path: String,
    pub mtime_ms: i64,
    pub size_bytes: i64,
}

#[derive(Debug, Clone)]
pub struct NewImageRecord {
    pub path: String,
    pub file_name: String,
    pub mtime_ms: i64,
    pub size_bytes: i64,
    pub dims: usize,
    pub vector: Vec<f32>,
}

#[derive(Debug, Clone)]
pub struct SearchImageRecord {
    pub id: i64,
    pub file_name: String,
    pub path: String,
    pub dims: usize,
    pub vector: Vec<f32>,
}

pub fn init(db_path: &Path) -> Result<()> {
    let connection = open_connection(db_path)?;

    connection.execute_batch(
        r#"
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;

        CREATE TABLE IF NOT EXISTS settings (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            model_dir TEXT NOT NULL,
            image_dir TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS images (
            id INTEGER PRIMARY KEY,
            path TEXT NOT NULL UNIQUE,
            file_name TEXT NOT NULL,
            mtime_ms INTEGER NOT NULL,
            size_bytes INTEGER NOT NULL,
            dims INTEGER NOT NULL,
            vector_blob BLOB NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        "#,
    )?;

    Ok(())
}

pub fn load_settings(db_path: &Path) -> Result<Option<AppSettings>> {
    let connection = open_connection(db_path)?;
    let settings = connection
        .query_row(
            "SELECT model_dir, image_dir FROM settings WHERE id = 1",
            [],
            |row| {
                Ok(AppSettings {
                    model_dir: row.get(0)?,
                    image_dir: row.get(1)?,
                })
            },
        )
        .optional()?;

    Ok(settings)
}

pub fn save_settings(db_path: &Path, settings: &AppSettings) -> Result<()> {
    let mut connection = open_connection(db_path)?;
    let transaction = connection.transaction()?;
    transaction.execute(
        r#"
        INSERT INTO settings (id, model_dir, image_dir, updated_at)
        VALUES (1, ?1, ?2, ?3)
        ON CONFLICT(id) DO UPDATE SET
            model_dir = excluded.model_dir,
            image_dir = excluded.image_dir,
            updated_at = excluded.updated_at
        "#,
        params![settings.model_dir, settings.image_dir, now_rfc3339()?],
    )?;
    transaction.commit()?;

    Ok(())
}

pub fn clear_images(db_path: &Path) -> Result<()> {
    let connection = open_connection(db_path)?;
    connection.execute("DELETE FROM images", [])?;
    Ok(())
}

pub fn list_indexed_images(db_path: &Path) -> Result<HashMap<String, IndexedImageSnapshot>> {
    let connection = open_connection(db_path)?;
    let mut statement =
        connection.prepare("SELECT path, mtime_ms, size_bytes FROM images ORDER BY id")?;
    let rows = statement.query_map([], |row| {
        Ok(IndexedImageSnapshot {
            path: row.get(0)?,
            mtime_ms: row.get(1)?,
            size_bytes: row.get(2)?,
        })
    })?;

    let mut images = HashMap::new();
    for row in rows {
        let image = row?;
        images.insert(image.path.clone(), image);
    }
    Ok(images)
}

pub fn upsert_image(db_path: &Path, record: &NewImageRecord) -> Result<()> {
    let mut connection = open_connection(db_path)?;
    let transaction = connection.transaction()?;
    let now = now_rfc3339()?;

    transaction.execute(
        r#"
        INSERT INTO images (
            path, file_name, mtime_ms, size_bytes, dims, vector_blob, created_at, updated_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
        ON CONFLICT(path) DO UPDATE SET
            file_name = excluded.file_name,
            mtime_ms = excluded.mtime_ms,
            size_bytes = excluded.size_bytes,
            dims = excluded.dims,
            vector_blob = excluded.vector_blob,
            updated_at = excluded.updated_at
        "#,
        params![
            record.path,
            record.file_name,
            record.mtime_ms,
            record.size_bytes,
            i64::try_from(record.dims).context("embedding dimension does not fit into i64")?,
            encode_vector(&record.vector),
            now,
        ],
    )?;

    transaction.commit()?;
    Ok(())
}

pub fn delete_images_by_paths(db_path: &Path, paths: &[String]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }

    let mut connection = open_connection(db_path)?;
    let transaction = connection.transaction()?;
    {
        let mut statement = transaction.prepare("DELETE FROM images WHERE path = ?1")?;
        for path in paths {
            statement.execute([path])?;
        }
    }
    transaction.commit()?;

    Ok(())
}

pub fn list_search_images(db_path: &Path) -> Result<Vec<SearchImageRecord>> {
    let connection = open_connection(db_path)?;
    let mut statement = connection.prepare(
        "SELECT id, file_name, path, dims, vector_blob FROM images ORDER BY updated_at DESC, id DESC",
    )?;
    let rows = statement.query_map([], |row| {
        let dims: i64 = row.get(3)?;
        let blob: Vec<u8> = row.get(4)?;

        Ok(SearchImageRecord {
            id: row.get(0)?,
            file_name: row.get(1)?,
            path: row.get(2)?,
            dims: usize::try_from(dims).map_err(|_| {
                rusqlite::Error::FromSqlConversionFailure(
                    3,
                    rusqlite::types::Type::Integer,
                    Box::new(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "stored dims is negative",
                    )),
                )
            })?,
            vector: decode_vector(&blob).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    4,
                    rusqlite::types::Type::Blob,
                    Box::new(io::Error::new(
                        io::ErrorKind::InvalidData,
                        error.to_string(),
                    )),
                )
            })?,
        })
    })?;

    let mut images = Vec::new();
    for row in rows {
        images.push(row?);
    }
    Ok(images)
}

pub fn get_image_path(db_path: &Path, id: i64) -> Result<Option<String>> {
    let connection = open_connection(db_path)?;
    let path = connection
        .query_row("SELECT path FROM images WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .optional()?;
    Ok(path)
}

fn open_connection(db_path: &Path) -> Result<Connection> {
    let connection = Connection::open(db_path)
        .with_context(|| format!("failed to open database at {}", db_path.display()))?;
    Ok(connection)
}

fn encode_vector(values: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(values.len() * 4);
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn decode_vector(blob: &[u8]) -> Result<Vec<f32>> {
    if blob.len() % 4 != 0 {
        return Err(anyhow!("vector blob length must be divisible by 4"));
    }

    let mut values = Vec::with_capacity(blob.len() / 4);
    for chunk in blob.chunks_exact(4) {
        values.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Ok(values)
}

fn now_rfc3339() -> Result<String> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .context("failed to format timestamp")
}

#[cfg(test)]
mod tests {
    use super::{decode_vector, encode_vector};

    #[test]
    fn vector_roundtrip_is_lossless() {
        let values = vec![0.5_f32, -1.25, 3.0];
        let decoded = decode_vector(&encode_vector(&values)).unwrap();
        assert_eq!(decoded, values);
    }
}
