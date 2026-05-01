/// SQLite clip library.
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::path::PathBuf;
use anyhow::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clip {
    pub id: i64,
    pub filename: String,
    pub filepath: String,
    pub duration_s: f64,
    pub width: i64,
    pub height: i64,
    pub fps: f64,
    pub filesize_b: i64,
    pub format: String,
    pub created_at: String,
    pub tags: String,
    pub thumbnail: String,
}

impl Clip {
    pub fn duration_str(&self) -> String {
        let total = self.duration_s as u64;
        let h = total / 3600;
        let m = (total % 3600) / 60;
        let s = total % 60;
        if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
    }
}

#[derive(Default)]
pub struct LibraryState(pub Mutex<()>);

fn db_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".cliplite")
        .join("library.db")
}

pub fn init_db() -> Result<()> {
    let path = db_path();
    std::fs::create_dir_all(path.parent().unwrap())?;
    let conn = Connection::open(&path)?;
    conn.execute_batch("
        CREATE TABLE IF NOT EXISTS clips (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            filename    TEXT NOT NULL,
            filepath    TEXT NOT NULL UNIQUE,
            duration_s  REAL DEFAULT 0,
            width       INTEGER DEFAULT 0,
            height      INTEGER DEFAULT 0,
            fps         REAL DEFAULT 0,
            filesize_b  INTEGER DEFAULT 0,
            format      TEXT DEFAULT '',
            created_at  TEXT DEFAULT (datetime('now')),
            tags        TEXT DEFAULT '',
            thumbnail   TEXT DEFAULT ''
        );
        CREATE INDEX IF NOT EXISTS idx_clips_created ON clips(created_at DESC);
    ")?;
    Ok(())
}

pub fn get_all_clips(search: &str) -> Result<Vec<Clip>> {
    let conn = Connection::open(db_path())?;
    let mut stmt = if search.is_empty() {
        conn.prepare("SELECT * FROM clips ORDER BY created_at DESC")?
    } else {
        conn.prepare("SELECT * FROM clips WHERE filename LIKE ?1 OR tags LIKE ?1 ORDER BY created_at DESC")?
    };

    let pattern = format!("%{}%", search);
    let rows = if search.is_empty() {
        stmt.query_map([], row_to_clip)?
    } else {
        stmt.query_map(params![pattern], row_to_clip)?
    };

    Ok(rows.filter_map(|r| r.ok()).collect())
}

pub fn add_clip_to_db(filepath: &str, meta: &ClipMeta) -> Result<Clip> {
    let conn = Connection::open(db_path())?;
    let filename = std::path::Path::new(filepath)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    conn.execute(
        "INSERT OR IGNORE INTO clips (filename, filepath, duration_s, width, height, fps, filesize_b, format)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![filename, filepath, meta.duration_s, meta.width, meta.height,
                meta.fps, meta.filesize_b, meta.format],
    )?;

    let id: i64 = conn.query_row(
        "SELECT id FROM clips WHERE filepath = ?1", params![filepath], |r| r.get(0))?;

    Ok(conn.query_row("SELECT * FROM clips WHERE id = ?1", params![id], row_to_clip)?)
}

pub fn update_thumbnail(clip_id: i64, thumb_path: &str) -> Result<()> {
    let conn = Connection::open(db_path())?;
    conn.execute("UPDATE clips SET thumbnail = ?1 WHERE id = ?2", params![thumb_path, clip_id])?;
    Ok(())
}

pub fn delete_clip_from_db(clip_id: i64) -> Result<(String, String)> {
    let conn = Connection::open(db_path())?;
    let (filepath, thumbnail): (String, String) = conn.query_row(
        "SELECT filepath, thumbnail FROM clips WHERE id = ?1", params![clip_id],
        |r| Ok((r.get(0)?, r.get(1)?))
    )?;
    conn.execute("DELETE FROM clips WHERE id = ?1", params![clip_id])?;
    Ok((filepath, thumbnail))
}

pub fn update_tags_in_db(clip_id: i64, tags: &str) -> Result<()> {
    let conn = Connection::open(db_path())?;
    conn.execute("UPDATE clips SET tags = ?1 WHERE id = ?2", params![tags, clip_id])?;
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct ClipMeta {
    pub duration_s: f64,
    pub width: i64,
    pub height: i64,
    pub fps: f64,
    pub filesize_b: i64,
    pub format: String,
}

fn row_to_clip(row: &rusqlite::Row) -> rusqlite::Result<Clip> {
    Ok(Clip {
        id: row.get(0)?,
        filename: row.get(1)?,
        filepath: row.get(2)?,
        duration_s: row.get::<_, f64>(3).unwrap_or(0.0),
        width: row.get::<_, i64>(4).unwrap_or(0),
        height: row.get::<_, i64>(5).unwrap_or(0),
        fps: row.get::<_, f64>(6).unwrap_or(0.0),
        filesize_b: row.get::<_, i64>(7).unwrap_or(0),
        format: row.get::<_, String>(8).unwrap_or_default(),
        created_at: row.get::<_, String>(9).unwrap_or_default(),
        tags: row.get::<_, String>(10).unwrap_or_default(),
        thumbnail: row.get::<_, String>(11).unwrap_or_default(),
    })
}
