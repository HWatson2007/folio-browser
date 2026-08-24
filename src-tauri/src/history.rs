use rusqlite::{Connection, Row, params};
use serde::{Deserialize, Serialize};
use std::{
    path::Path,
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub id: u64,
    pub attempted_at: u64,
    pub updated_at: u64,
    pub url: String,
    pub title: Option<String>,
    pub status: NavigationStatus,
    pub source: String,
    pub submitted_input: Option<String>,
    pub search_query: Option<String>,
    pub search_url: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NavigationStatus {
    Attempted,
    Started,
    Completed,
}

#[derive(Clone, Debug)]
pub struct PendingNavigation {
    pub target_url: String,
    pub source: String,
    pub submitted_input: Option<String>,
    pub search_query: Option<String>,
    pub search_url: Option<String>,
}

pub struct HistoryStore {
    connection: Mutex<Connection>,
    pending: Mutex<Option<PendingNavigation>>,
}

impl NavigationStatus {
    fn as_database_value(self) -> &'static str {
        match self {
            Self::Attempted => "attempted",
            Self::Started => "started",
            Self::Completed => "completed",
        }
    }

    fn from_database_value(value: &str) -> Result<Self, String> {
        match value {
            "attempted" => Ok(Self::Attempted),
            "started" => Ok(Self::Started),
            "completed" => Ok(Self::Completed),
            _ => Err(format!(
                "History database contains an invalid navigation status: {value}"
            )),
        }
    }
}

impl HistoryStore {
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }

        let connection = Connection::open(path).map_err(|error| error.to_string())?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(|error| error.to_string())?;
        connection
            .execute_batch(
                "
                PRAGMA journal_mode = WAL;
                PRAGMA synchronous = FULL;
                CREATE TABLE IF NOT EXISTS history_entries (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    attempted_at INTEGER NOT NULL CHECK (attempted_at >= 0),
                    updated_at INTEGER NOT NULL CHECK (updated_at >= 0),
                    url TEXT NOT NULL,
                    title TEXT,
                    status TEXT NOT NULL CHECK (status IN ('attempted', 'started', 'completed')),
                    source TEXT NOT NULL,
                    submitted_input TEXT,
                    search_query TEXT,
                    search_url TEXT
                );
                CREATE INDEX IF NOT EXISTS history_entries_newest
                    ON history_entries (attempted_at DESC, id DESC);
                CREATE INDEX IF NOT EXISTS history_entries_url_latest
                    ON history_entries (url, id DESC);
                ",
            )
            .map_err(|error| error.to_string())?;

        Ok(Self {
            connection: Mutex::new(connection),
            pending: Mutex::new(None),
        })
    }

    pub fn set_pending(&self, pending: PendingNavigation) -> Result<(), String> {
        *self.pending.lock().map_err(|error| error.to_string())? = Some(pending);
        Ok(())
    }

    pub fn record_attempt(&self, url: &str) -> Result<HistoryEntry, String> {
        let context = {
            let mut pending = self.pending.lock().map_err(|error| error.to_string())?;
            if pending.as_ref().is_some_and(|item| item.target_url == url) {
                pending.take()
            } else {
                None
            }
        };
        let detected_search = duckduckgo_search_query(url);
        let now = unix_millis();
        let mut entry = HistoryEntry {
            id: 0,
            attempted_at: now,
            updated_at: now,
            url: url.to_owned(),
            title: None,
            status: NavigationStatus::Attempted,
            source: context
                .as_ref()
                .map(|item| item.source.clone())
                .unwrap_or_else(|| {
                    if detected_search.is_some() {
                        "duckduckgo".to_owned()
                    } else {
                        "page".to_owned()
                    }
                }),
            submitted_input: context
                .as_ref()
                .and_then(|item| item.submitted_input.clone())
                .or_else(|| detected_search.clone()),
            search_query: context
                .as_ref()
                .and_then(|item| item.search_query.clone())
                .or_else(|| detected_search.clone()),
            search_url: context
                .as_ref()
                .and_then(|item| item.search_url.clone())
                .or_else(|| detected_search.map(|_| url.to_owned())),
        };
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        connection
            .execute(
                "INSERT INTO history_entries (
                    attempted_at, updated_at, url, title, status, source, submitted_input,
                    search_query, search_url
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    sqlite_integer(entry.attempted_at)?,
                    sqlite_integer(entry.updated_at)?,
                    entry.url,
                    entry.title,
                    entry.status.as_database_value(),
                    entry.source,
                    entry.submitted_input,
                    entry.search_query,
                    entry.search_url,
                ],
            )
            .map_err(|error| error.to_string())?;
        entry.id = u64::try_from(connection.last_insert_rowid())
            .map_err(|_| "History database generated an invalid ID.".to_owned())?;
        Ok(entry)
    }

    pub fn update_status(&self, id: u64, status: NavigationStatus) -> Result<(), String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        connection
            .execute(
                "UPDATE history_entries
                 SET status = ?, updated_at = ?
                 WHERE id = ? AND status <> ?",
                params![
                    status.as_database_value(),
                    sqlite_integer(unix_millis())?,
                    sqlite_integer(id)?,
                    status.as_database_value(),
                ],
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    pub fn update_title(&self, id: u64, title: &str) -> Result<(), String> {
        let title = title.trim();
        if title.is_empty() {
            return Ok(());
        }
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        connection
            .execute(
                "UPDATE history_entries
                 SET title = ?, updated_at = ?
                 WHERE id = ? AND (title IS NULL OR title <> ?)",
                params![
                    title,
                    sqlite_integer(unix_millis())?,
                    sqlite_integer(id)?,
                    title
                ],
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    pub const MAX_PAGE_LIMIT: u64 = 200;

    pub fn for_each_entry<F>(&self, mut callback: F) -> Result<usize, String>
    where
        F: FnMut(HistoryEntry) -> Result<(), String>,
    {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let mut statement = connection
            .prepare(
                "SELECT id, attempted_at, updated_at, url, title, status, source,
                        submitted_input, search_query, search_url
                 FROM history_entries
                 ORDER BY attempted_at DESC, id DESC",
            )
            .map_err(|error| error.to_string())?;
        let mut rows = statement.query([]).map_err(|error| error.to_string())?;
        let mut count = 0usize;
        while let Some(row) = rows.next().map_err(|error| error.to_string())? {
            let entry = Self::entry_from_row(row)?;
            callback(entry)?;
            count += 1;
        }
        Ok(count)
    }

    pub fn entries_newest_first(&self) -> Result<Vec<HistoryEntry>, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let mut statement = connection
            .prepare(
                "SELECT id, attempted_at, updated_at, url, title, status, source,
                        submitted_input, search_query, search_url
                 FROM history_entries
                 ORDER BY attempted_at DESC, id DESC",
            )
            .map_err(|error| error.to_string())?;
        let mut rows = statement.query([]).map_err(|error| error.to_string())?;
        let mut entries = Vec::new();
        while let Some(row) = rows.next().map_err(|error| error.to_string())? {
            entries.push(Self::entry_from_row(row)?);
        }
        Ok(entries)
    }

    pub fn history_page(
        &self,
        limit: u64,
        offset: u64,
        query: Option<String>,
    ) -> Result<(Vec<HistoryEntry>, u64), String> {
        let limit = limit.clamp(1, Self::MAX_PAGE_LIMIT);
        let normalized = query
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        let escaped = normalized.as_ref().map(|value| escape_like_pattern(value));

        let connection = self.connection.lock().map_err(|error| error.to_string())?;

        let total: i64 = if let Some(ref pattern) = escaped {
            connection
                .query_row(
                    "SELECT COUNT(*) FROM history_entries WHERE (
                        url LIKE '%' || ?1 || '%' ESCAPE '\\' COLLATE NOCASE
                        OR COALESCE(title, '') LIKE '%' || ?1 || '%' ESCAPE '\\' COLLATE NOCASE
                        OR COALESCE(submitted_input, '') LIKE '%' || ?1 || '%' ESCAPE '\\' COLLATE NOCASE
                        OR COALESCE(search_query, '') LIKE '%' || ?1 || '%' ESCAPE '\\' COLLATE NOCASE
                        OR source LIKE '%' || ?1 || '%' ESCAPE '\\' COLLATE NOCASE
                    )",
                    params![pattern],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?
        } else {
            connection
                .query_row("SELECT COUNT(*) FROM history_entries", [], |row| row.get(0))
                .map_err(|error| error.to_string())?
        };
        let total = u64::try_from(total)
            .map_err(|_| "History database returned a negative total.".to_owned())?;

        let mut entries = Vec::new();
        if let Some(pattern) = escaped {
            let mut statement = connection
                .prepare(
                    "SELECT id, attempted_at, updated_at, url, title, status, source,
                            submitted_input, search_query, search_url
                     FROM history_entries
                     WHERE (
                        url LIKE '%' || ?1 || '%' ESCAPE '\\' COLLATE NOCASE
                        OR COALESCE(title, '') LIKE '%' || ?1 || '%' ESCAPE '\\' COLLATE NOCASE
                        OR COALESCE(submitted_input, '') LIKE '%' || ?1 || '%' ESCAPE '\\' COLLATE NOCASE
                        OR COALESCE(search_query, '') LIKE '%' || ?1 || '%' ESCAPE '\\' COLLATE NOCASE
                        OR source LIKE '%' || ?1 || '%' ESCAPE '\\' COLLATE NOCASE
                     )
                     ORDER BY attempted_at DESC, id DESC
                     LIMIT ?2 OFFSET ?3",
                )
                .map_err(|error| error.to_string())?;
            let mut rows = statement
                .query(params![
                    pattern,
                    sqlite_integer(limit)?,
                    sqlite_integer(offset)?
                ])
                .map_err(|error| error.to_string())?;
            while let Some(row) = rows.next().map_err(|error| error.to_string())? {
                entries.push(Self::entry_from_row(row)?);
            }
        } else {
            let mut statement = connection
                .prepare(
                    "SELECT id, attempted_at, updated_at, url, title, status, source,
                            submitted_input, search_query, search_url
                     FROM history_entries
                     ORDER BY attempted_at DESC, id DESC
                     LIMIT ?1 OFFSET ?2",
                )
                .map_err(|error| error.to_string())?;
            let mut rows = statement
                .query(params![sqlite_integer(limit)?, sqlite_integer(offset)?])
                .map_err(|error| error.to_string())?;
            while let Some(row) = rows.next().map_err(|error| error.to_string())? {
                entries.push(Self::entry_from_row(row)?);
            }
        }
        Ok((entries, total))
    }

    fn entry_from_row(row: &Row<'_>) -> Result<HistoryEntry, String> {
        let id = sqlite_unsigned(
            row.get::<_, i64>(0).map_err(|error| error.to_string())?,
            "ID",
        )?;
        let attempted_at = sqlite_unsigned(
            row.get::<_, i64>(1).map_err(|error| error.to_string())?,
            "attempt timestamp",
        )?;
        let updated_at = sqlite_unsigned(
            row.get::<_, i64>(2).map_err(|error| error.to_string())?,
            "update timestamp",
        )?;
        let status = row.get::<_, String>(5).map_err(|error| error.to_string())?;

        Ok(HistoryEntry {
            id,
            attempted_at,
            updated_at,
            url: row.get(3).map_err(|error| error.to_string())?,
            title: row.get(4).map_err(|error| error.to_string())?,
            status: NavigationStatus::from_database_value(&status)?,
            source: row.get(6).map_err(|error| error.to_string())?,
            submitted_input: row.get(7).map_err(|error| error.to_string())?,
            search_query: row.get(8).map_err(|error| error.to_string())?,
            search_url: row.get(9).map_err(|error| error.to_string())?,
        })
    }
}

fn sqlite_integer(value: u64) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| "Timestamp exceeds SQLite's integer range.".to_owned())
}

fn sqlite_unsigned(value: i64, column: &str) -> Result<u64, String> {
    u64::try_from(value).map_err(|_| format!("History database contains a negative {column}."))
}

pub(crate) fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn duckduckgo_search_query(url: &str) -> Option<String> {
    let url = tauri::Url::parse(url).ok()?;
    let host = url.host_str()?;
    if host != "duckduckgo.com" && host != "www.duckduckgo.com" {
        return None;
    }
    url.query_pairs()
        .find(|(key, value)| key == "q" && !value.trim().is_empty())
        .map(|(_, value)| value.into_owned())
}

fn escape_like_pattern(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if character == '\\' || character == '%' || character == '_' {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

pub fn timestamp_iso(timestamp: u64) -> Result<String, String> {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(timestamp) * 1_000_000)
        .map_err(|error| error.to_string())?
        .format(&Rfc3339)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_history_entries_and_updates_by_stable_id() {
        let directory = std::env::temp_dir().join(format!(
            "folio-browser-history-test-{}-{}",
            std::process::id(),
            unix_millis()
        ));
        let path = directory.join("history.sqlite3");

        let first = {
            let history = HistoryStore::open(&path).unwrap();
            let first = history.record_attempt("https://example.com/").unwrap();
            let second = history.record_attempt("https://example.com/").unwrap();

            // A late event from the first visit must not update the newer visit to the same URL.
            history
                .update_status(first.id, NavigationStatus::Completed)
                .unwrap();
            history.update_title(first.id, "First title").unwrap();
            history
                .update_status(second.id, NavigationStatus::Started)
                .unwrap();
            history.update_title(second.id, "Second title").unwrap();

            let entries = history.entries_newest_first().unwrap();
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0].id, second.id);
            assert_eq!(entries[0].status, NavigationStatus::Started);
            assert_eq!(entries[0].title.as_deref(), Some("Second title"));
            assert_eq!(entries[1].id, first.id);
            assert_eq!(entries[1].status, NavigationStatus::Completed);
            assert_eq!(entries[1].title.as_deref(), Some("First title"));
            first
        };

        assert!(path.exists());
        let history = HistoryStore::open(&path).unwrap();
        let entries = history.entries_newest_first().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].id, first.id);
        drop(history);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn formats_export_timestamp_as_iso_utc() {
        assert_eq!(timestamp_iso(0).unwrap(), "1970-01-01T00:00:00Z");
    }
}
