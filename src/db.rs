use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OptionalExtension, params};
use std::io;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexModelSync {
    pub stored_signature: Option<String>,
    pub current_signature: Option<String>,
    pub index_cleared: bool,
}

const INDEX_MODEL_SIGNATURE_KEY: &str = "index_model_signature";

pub fn init(db_path: &Path) -> Result<()> {
    let connection = open_connection(db_path)?;

    connection.execute_batch(
        r#"
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;

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

        CREATE TABLE IF NOT EXISTS app_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        "#,
    )?;

    Ok(())
}

pub fn clear_images(db_path: &Path) -> Result<()> {
    let connection = open_connection(db_path)?;
    connection.execute("DELETE FROM images", [])?;
    Ok(())
}

pub fn count_images(db_path: &Path) -> Result<usize> {
    let connection = open_connection(db_path)?;
    let count: i64 = connection.query_row("SELECT COUNT(*) FROM images", [], |row| row.get(0))?;
    usize::try_from(count).context("image count does not fit into usize")
}

pub fn get_index_model_signature(db_path: &Path) -> Result<Option<String>> {
    let connection = open_connection(db_path)?;
    let signature = connection
        .query_row(
            "SELECT value FROM app_meta WHERE key = ?1",
            [INDEX_MODEL_SIGNATURE_KEY],
            |row| row.get(0),
        )
        .optional()?;
    Ok(signature)
}

pub fn set_index_model_signature(db_path: &Path, signature: Option<&str>) -> Result<()> {
    let connection = open_connection(db_path)?;
    match signature {
        Some(signature) => {
            connection.execute(
                r#"
                INSERT INTO app_meta (key, value)
                VALUES (?1, ?2)
                ON CONFLICT(key) DO UPDATE SET value = excluded.value
                "#,
                params![INDEX_MODEL_SIGNATURE_KEY, signature],
            )?;
        }
        None => {
            connection.execute(
                "DELETE FROM app_meta WHERE key = ?1",
                [INDEX_MODEL_SIGNATURE_KEY],
            )?;
        }
    }
    Ok(())
}

pub fn sync_index_model_signature(
    db_path: &Path,
    current_signature: Option<&str>,
) -> Result<IndexModelSync> {
    let stored_signature = get_index_model_signature(db_path)?;
    let image_count = count_images(db_path)?;
    let current_signature_owned = current_signature.map(ToOwned::to_owned);

    let needs_reset = match (stored_signature.as_deref(), current_signature) {
        (Some(stored), Some(current)) => stored != current,
        (None, Some(_)) => image_count > 0,
        _ => false,
    };

    if needs_reset {
        clear_images(db_path)?;
    }

    if let Some(current_signature) = current_signature {
        if stored_signature.as_deref() != Some(current_signature) {
            set_index_model_signature(db_path, Some(current_signature))?;
        }
    }

    Ok(IndexModelSync {
        stored_signature,
        current_signature: current_signature_owned,
        index_cleared: needs_reset && image_count > 0,
    })
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
    if let Some(parent) = db_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create database parent directory {}",
                parent.display()
            )
        })?;
    }

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
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        NewImageRecord, clear_images, count_images, decode_vector, encode_vector,
        get_index_model_signature, init, set_index_model_signature, sync_index_model_signature,
        upsert_image,
    };

    #[test]
    fn vector_roundtrip_is_lossless() {
        let values = vec![0.5_f32, -1.25, 3.0];
        let decoded = decode_vector(&encode_vector(&values)).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn count_images_tracks_stored_rows() {
        let db_path = unique_test_db_path();
        init(&db_path).unwrap();
        assert_eq!(count_images(&db_path).unwrap(), 0);

        upsert_image(
            &db_path,
            &NewImageRecord {
                path: "a.png".to_owned(),
                file_name: "a.png".to_owned(),
                mtime_ms: 1,
                size_bytes: 10,
                dims: 2,
                vector: vec![0.1, 0.2],
            },
        )
        .unwrap();
        upsert_image(
            &db_path,
            &NewImageRecord {
                path: "b.png".to_owned(),
                file_name: "b.png".to_owned(),
                mtime_ms: 2,
                size_bytes: 20,
                dims: 2,
                vector: vec![0.3, 0.4],
            },
        )
        .unwrap();

        assert_eq!(count_images(&db_path).unwrap(), 2);

        clear_images(&db_path).unwrap();
        assert_eq!(count_images(&db_path).unwrap(), 0);

        let _ = fs::remove_file(&db_path);
        let wal_path = db_path.with_extension("sqlite3-wal");
        let shm_path = db_path.with_extension("sqlite3-shm");
        let _ = fs::remove_file(wal_path);
        let _ = fs::remove_file(shm_path);
    }

    #[test]
    fn init_creates_database_file_and_missing_parent_directories() {
        let db_path = unique_test_dir()
            .join("config")
            .join("puppy_find.db");
        assert!(!db_path.exists());
        assert!(
            !db_path
                .parent()
                .expect("db path should have a parent")
                .exists()
        );

        init(&db_path).unwrap();

        assert!(db_path.exists());
        assert!(
            db_path
                .parent()
                .expect("db path should have a parent")
                .is_dir()
        );

        let _ = fs::remove_file(&db_path);
        let wal_path = db_path.with_extension("db-wal");
        let shm_path = db_path.with_extension("db-shm");
        let _ = fs::remove_file(wal_path);
        let _ = fs::remove_file(shm_path);
        let _ = fs::remove_dir_all(
            db_path
                .parent()
                .and_then(|parent| parent.parent())
                .expect("config directory should have a parent"),
        );
    }

    #[test]
    fn sync_index_model_signature_resets_old_vectors_when_signature_changes() {
        let db_path = unique_test_db_path();
        init(&db_path).unwrap();

        upsert_image(
            &db_path,
            &NewImageRecord {
                path: "a.png".to_owned(),
                file_name: "a.png".to_owned(),
                mtime_ms: 1,
                size_bytes: 10,
                dims: 2,
                vector: vec![0.1, 0.2],
            },
        )
        .unwrap();

        let first_sync = sync_index_model_signature(&db_path, Some("sig-a")).unwrap();
        assert!(first_sync.index_cleared);
        assert_eq!(count_images(&db_path).unwrap(), 0);
        assert_eq!(
            get_index_model_signature(&db_path).unwrap().as_deref(),
            Some("sig-a")
        );

        upsert_image(
            &db_path,
            &NewImageRecord {
                path: "b.png".to_owned(),
                file_name: "b.png".to_owned(),
                mtime_ms: 2,
                size_bytes: 20,
                dims: 2,
                vector: vec![0.3, 0.4],
            },
        )
        .unwrap();

        let second_sync = sync_index_model_signature(&db_path, Some("sig-a")).unwrap();
        assert!(!second_sync.index_cleared);
        assert_eq!(count_images(&db_path).unwrap(), 1);

        let third_sync = sync_index_model_signature(&db_path, Some("sig-b")).unwrap();
        assert!(third_sync.index_cleared);
        assert_eq!(count_images(&db_path).unwrap(), 0);
        assert_eq!(
            get_index_model_signature(&db_path).unwrap().as_deref(),
            Some("sig-b")
        );

        let _ = fs::remove_file(&db_path);
        let wal_path = db_path.with_extension("sqlite3-wal");
        let shm_path = db_path.with_extension("sqlite3-shm");
        let _ = fs::remove_file(wal_path);
        let _ = fs::remove_file(shm_path);
    }

    #[test]
    fn set_index_model_signature_can_clear_metadata() {
        let db_path = unique_test_db_path();
        init(&db_path).unwrap();

        set_index_model_signature(&db_path, Some("sig-a")).unwrap();
        assert_eq!(
            get_index_model_signature(&db_path).unwrap().as_deref(),
            Some("sig-a")
        );

        set_index_model_signature(&db_path, None).unwrap();
        assert_eq!(get_index_model_signature(&db_path).unwrap(), None);

        let _ = fs::remove_file(&db_path);
        let wal_path = db_path.with_extension("sqlite3-wal");
        let shm_path = db_path.with_extension("sqlite3-shm");
        let _ = fs::remove_file(wal_path);
        let _ = fs::remove_file(shm_path);
    }

    fn unique_test_db_path() -> std::path::PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("puppy_find_db_test_{timestamp}.sqlite3"))
    }

    fn unique_test_dir() -> std::path::PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("puppy_find_db_dir_test_{timestamp}"))
    }
}
