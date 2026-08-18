use rusqlite::{Connection, Row, params};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::{
    Emitter, LogicalPosition, LogicalSize, Manager, State, WebviewUrl,
    webview::{NewWindowResponse, PageLoadEvent, WebviewBuilder},
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

const HOME_URL: &str = "https://duckduckgo.com/";
const TOOLBAR_HEIGHT: f64 = 76.0;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct HistoryEntry {
    id: u64,
    attempted_at: u64,
    updated_at: u64,
    url: String,
    title: Option<String>,
    status: NavigationStatus,
    source: String,
    submitted_input: Option<String>,
    search_query: Option<String>,
    search_url: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum NavigationStatus {
    Attempted,
    Started,
    Completed,
}

#[derive(Clone, Debug)]
struct PendingNavigation {
    target_url: String,
    source: String,
    submitted_input: Option<String>,
    search_query: Option<String>,
    search_url: Option<String>,
}

struct HistoryStore {
    connection: Mutex<Connection>,
    pending: Mutex<Option<PendingNavigation>>,
}

struct LayoutState {
    toolbar_height: Mutex<f64>,
    history_open: AtomicBool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct NavigationEvent {
    url: String,
    status: NavigationStatus,
    title: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportEntry {
    #[serde(flatten)]
    entry: HistoryEntry,
    attempted_at_iso: String,
    updated_at_iso: String,
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
    fn open(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
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

    fn set_pending(&self, pending: PendingNavigation) -> Result<(), String> {
        *self.pending.lock().map_err(|error| error.to_string())? = Some(pending);
        Ok(())
    }

    fn record_attempt(&self, url: &str) -> Result<HistoryEntry, String> {
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

    fn update_status(&self, url: &str, status: NavigationStatus) -> Result<(), String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        connection
            .execute(
                "UPDATE history_entries
                 SET status = ?, updated_at = ?
                 WHERE id = (
                    SELECT id FROM history_entries WHERE url = ? ORDER BY id DESC LIMIT 1
                 ) AND status <> ?",
                params![
                    status.as_database_value(),
                    sqlite_integer(unix_millis())?,
                    url,
                    status.as_database_value(),
                ],
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    fn update_title(&self, url: &str, title: &str) -> Result<(), String> {
        let title = title.trim();
        if title.is_empty() {
            return Ok(());
        }
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        connection
            .execute(
                "UPDATE history_entries
                 SET title = ?, updated_at = ?
                 WHERE id = (
                    SELECT id FROM history_entries WHERE url = ? ORDER BY id DESC LIMIT 1
                 ) AND (title IS NULL OR title <> ?)",
                params![title, sqlite_integer(unix_millis())?, url, title],
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    fn entries_newest_first(&self) -> Result<Vec<HistoryEntry>, String> {
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

fn unix_millis() -> u64 {
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

fn timestamp_iso(timestamp: u64) -> Result<String, String> {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(timestamp) * 1_000_000)
        .map_err(|error| error.to_string())?
        .format(&Rfc3339)
        .map_err(|error| error.to_string())
}

fn resolve_input(input: &str, source: &str) -> Result<(tauri::Url, PendingNavigation), String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("Enter an address or search term.".to_owned());
    }

    let direct_url = if input.starts_with("http://") || input.starts_with("https://") {
        tauri::Url::parse(input).ok()
    } else if !input.chars().any(char::is_whitespace)
        && (input.contains('.') || input.starts_with("localhost"))
    {
        tauri::Url::parse(&format!("https://{input}")).ok()
    } else {
        None
    };

    let (url, search_query) = match direct_url {
        Some(url) => (url, None),
        None => (
            tauri::Url::parse_with_params(HOME_URL, &[("q", input)])
                .map_err(|error| error.to_string())?,
            Some(input.to_owned()),
        ),
    };

    if !matches!(url.scheme(), "http" | "https") {
        return Err("Only HTTP and HTTPS addresses are supported.".to_owned());
    }

    let target_url = url.to_string();
    Ok((
        url,
        PendingNavigation {
            target_url: target_url.clone(),
            source: source.to_owned(),
            submitted_input: Some(input.to_owned()),
            search_query: search_query.clone(),
            search_url: search_query.map(|_| target_url),
        },
    ))
}

fn content_webview(app: &tauri::AppHandle) -> Result<tauri::Webview, String> {
    app.get_webview("content")
        .ok_or_else(|| "The content webview is not ready.".to_owned())
}

fn apply_layout(app: &tauri::AppHandle, layout: &LayoutState) -> Result<(), String> {
    let content = content_webview(app)?;
    let chrome = app
        .get_webview("chrome")
        .ok_or_else(|| "The browser chrome is not ready.".to_owned())?;
    let window = content.window();
    let physical_size = window.inner_size().map_err(|error| error.to_string())?;
    let scale = window.scale_factor().map_err(|error| error.to_string())?;
    let size = physical_size.to_logical::<f64>(scale);
    let offset = *layout
        .toolbar_height
        .lock()
        .map_err(|error| error.to_string())?;
    let history_open = layout.history_open.load(Ordering::Relaxed);

    chrome
        .set_position(LogicalPosition::new(0.0, 0.0))
        .map_err(|error| error.to_string())?;
    chrome
        .set_size(LogicalSize::new(
            size.width,
            if history_open { size.height } else { offset },
        ))
        .map_err(|error| error.to_string())?;
    content
        .set_position(LogicalPosition::new(0.0, offset))
        .map_err(|error| error.to_string())?;
    content
        .set_size(LogicalSize::new(
            size.width,
            (size.height - offset).max(1.0),
        ))
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn navigate(
    app: tauri::AppHandle,
    history: State<'_, Arc<HistoryStore>>,
    input: String,
    source: String,
) -> Result<(), String> {
    let source = match source.as_str() {
        "address" | "history" | "popup" | "home" => source,
        _ => "other".to_owned(),
    };
    let (url, pending) = resolve_input(&input, &source)?;
    history.set_pending(pending)?;
    content_webview(&app)?
        .navigate(url)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn navigate_home(
    app: tauri::AppHandle,
    history: State<'_, Arc<HistoryStore>>,
) -> Result<(), String> {
    let url = tauri::Url::parse(HOME_URL).map_err(|error| error.to_string())?;
    history.set_pending(PendingNavigation {
        target_url: HOME_URL.to_owned(),
        source: "home".to_owned(),
        submitted_input: None,
        search_query: None,
        search_url: None,
    })?;
    content_webview(&app)?
        .navigate(url)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn go_back(app: tauri::AppHandle) -> Result<(), String> {
    content_webview(&app)?
        .eval("window.history.back()")
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn go_forward(app: tauri::AppHandle) -> Result<(), String> {
    content_webview(&app)?
        .eval("window.history.forward()")
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn reload(app: tauri::AppHandle) -> Result<(), String> {
    content_webview(&app)?
        .reload()
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn current_url(app: tauri::AppHandle) -> Result<String, String> {
    content_webview(&app)?
        .url()
        .map(|url| url.to_string())
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn set_content_visible(
    app: tauri::AppHandle,
    layout: State<'_, Arc<LayoutState>>,
    visible: bool,
) -> Result<(), String> {
    let content = content_webview(&app)?;
    let chrome = app
        .get_webview("chrome")
        .ok_or_else(|| "The browser chrome is not ready.".to_owned())?;
    layout.history_open.store(!visible, Ordering::Relaxed);
    if visible {
        apply_layout(&app, &layout)?;
        content.show().map_err(|error| error.to_string())?;
        content.set_focus().map_err(|error| error.to_string())
    } else {
        content.hide().map_err(|error| error.to_string())?;
        apply_layout(&app, &layout)?;
        chrome.set_focus().map_err(|error| error.to_string())
    }
}

#[tauri::command]
fn set_content_offset(
    app: tauri::AppHandle,
    layout: State<'_, Arc<LayoutState>>,
    offset: f64,
) -> Result<(), String> {
    if !(48.0..=180.0).contains(&offset) {
        return Err("Invalid toolbar height.".to_owned());
    }
    *layout
        .toolbar_height
        .lock()
        .map_err(|error| error.to_string())? = offset;
    apply_layout(&app, &layout)
}

#[tauri::command]
fn get_history(history: State<'_, Arc<HistoryStore>>) -> Result<Vec<HistoryEntry>, String> {
    history.entries_newest_first()
}

#[tauri::command]
fn export_history(
    history: State<'_, Arc<HistoryStore>>,
    path: String,
    format: String,
) -> Result<usize, String> {
    let entries = history.entries_newest_first()?;
    let export_entries = entries
        .iter()
        .cloned()
        .map(|entry| {
            Ok(ExportEntry {
                attempted_at_iso: timestamp_iso(entry.attempted_at)?,
                updated_at_iso: timestamp_iso(entry.updated_at)?,
                entry,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let path = PathBuf::from(path);
    let file = File::create(&path).map_err(|error| format!("Could not create export: {error}"))?;
    let mut writer = BufWriter::new(file);

    match format.as_str() {
        "json" => serde_json::to_writer_pretty(&mut writer, &export_entries)
            .map_err(|error| error.to_string())?,
        "csv" => {
            writer
                .write_all(b"id,attempted_at,attempted_at_iso,updated_at,updated_at_iso,url,title,status,source,submitted_input,search_query,search_url\n")
                .map_err(|error| error.to_string())?;
            for exported in &export_entries {
                let entry = &exported.entry;
                let fields = [
                    entry.id.to_string(),
                    entry.attempted_at.to_string(),
                    exported.attempted_at_iso.clone(),
                    entry.updated_at.to_string(),
                    exported.updated_at_iso.clone(),
                    entry.url.clone(),
                    entry.title.clone().unwrap_or_default(),
                    serde_json::to_value(entry.status)
                        .ok()
                        .and_then(|value| value.as_str().map(str::to_owned))
                        .unwrap_or_default(),
                    entry.source.clone(),
                    entry.submitted_input.clone().unwrap_or_default(),
                    entry.search_query.clone().unwrap_or_default(),
                    entry.search_url.clone().unwrap_or_default(),
                ];
                let row = fields
                    .iter()
                    .map(|field| format!("\"{}\"", field.replace('"', "\"\"")))
                    .collect::<Vec<_>>()
                    .join(",");
                writeln!(writer, "{row}").map_err(|error| error.to_string())?;
            }
        }
        _ => return Err("Export format must be json or csv.".to_owned()),
    }

    writer.flush().map_err(|error| error.to_string())?;
    Ok(entries.len())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            navigate,
            navigate_home,
            go_back,
            go_forward,
            reload,
            current_url,
            set_content_visible,
            set_content_offset,
            get_history,
            export_history
        ])
        .setup(|app| {
            let history_path = app.path().app_data_dir()?.join("history.sqlite3");
            let history = Arc::new(HistoryStore::open(&history_path)?);
            app.manage(history.clone());
            let layout = Arc::new(LayoutState {
                toolbar_height: Mutex::new(TOOLBAR_HEIGHT),
                history_open: AtomicBool::new(false),
            });
            app.manage(layout.clone());

            let window = tauri::window::WindowBuilder::new(app, "main")
                .title("Folio Browser")
                .inner_size(1240.0, 820.0)
                .min_inner_size(720.0, 520.0)
                .center()
                .background_color(tauri::webview::Color(243, 241, 235, 255))
                .build()?;

            let physical_size = window.inner_size()?;
            let scale = window.scale_factor()?;
            let size = physical_size.to_logical::<f64>(scale);

            let navigation_history = history.clone();
            let navigation_handle = app.handle().clone();
            let load_history = history.clone();
            let load_handle = app.handle().clone();
            let popup_handle = app.handle().clone();
            let title_history = history;

            let content = WebviewBuilder::new(
                "content",
                WebviewUrl::External(tauri::Url::parse(HOME_URL)?),
            );
            let content = content
                .enable_clipboard_access()
                .background_color(tauri::webview::Color(255, 255, 255, 255))
                .on_navigation(move |url| {
                    let url = url.to_string();
                    if let Ok(entry) = navigation_history.record_attempt(&url) {
                        let _ = navigation_handle.emit_to(
                            "chrome",
                            "browser:navigation",
                            NavigationEvent {
                                url: entry.url,
                                status: entry.status,
                                title: entry.title,
                            },
                        );
                    }
                    matches!(url.split(':').next(), Some("http" | "https"))
                })
                .on_page_load(move |_webview, payload| {
                    let url = payload.url().to_string();
                    let status = match payload.event() {
                        PageLoadEvent::Started => NavigationStatus::Started,
                        PageLoadEvent::Finished => NavigationStatus::Completed,
                    };
                    let _ = load_history.update_status(&url, status);
                    let _ = load_handle.emit_to(
                        "chrome",
                        "browser:navigation",
                        NavigationEvent {
                            url,
                            status,
                            title: None,
                        },
                    );
                })
                .on_new_window(move |url, _features| {
                    let _ =
                        popup_handle.emit_to("chrome", "browser:popup-requested", url.to_string());
                    NewWindowResponse::Deny
                })
                .on_document_title_changed(move |webview, title| {
                    if let Ok(url) = webview.url() {
                        let url = url.to_string();
                        let _ = title_history.update_title(&url, &title);
                        let _ = webview
                            .window()
                            .set_title(&format!("{title} - Folio Browser"));
                    }
                });

            window.add_child(
                content,
                LogicalPosition::new(0.0, TOOLBAR_HEIGHT),
                LogicalSize::new(size.width, (size.height - TOOLBAR_HEIGHT).max(1.0)),
            )?;

            let chrome = WebviewBuilder::new("chrome", WebviewUrl::App("index.html".into()));
            let chrome = chrome.background_color(tauri::webview::Color(243, 241, 235, 255));
            window.add_child(
                chrome,
                LogicalPosition::new(0.0, 0.0),
                LogicalSize::new(size.width, TOOLBAR_HEIGHT),
            )?;

            let resize_handle = app.handle().clone();
            window.on_window_event(move |event| {
                if matches!(event, tauri::WindowEvent::Resized(_)) {
                    let _ = apply_layout(&resize_handle, &layout);
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Folio Browser");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_plain_text_as_search_with_exact_details() {
        let (url, pending) = resolve_input("rust webview2 history", "address").unwrap();
        assert_eq!(
            pending.submitted_input.as_deref(),
            Some("rust webview2 history")
        );
        assert_eq!(
            pending.search_query.as_deref(),
            Some("rust webview2 history")
        );
        assert_eq!(pending.search_url.as_deref(), Some(url.as_str()));
        assert!(url.as_str().starts_with("https://duckduckgo.com/"));
    }

    #[test]
    fn resolves_domain_as_https_address() {
        let (url, pending) = resolve_input("example.com/path", "address").unwrap();
        assert_eq!(url.as_str(), "https://example.com/path");
        assert!(pending.search_query.is_none());
    }

    #[test]
    fn detects_search_submitted_inside_duckduckgo() {
        assert_eq!(
            duckduckgo_search_query("https://duckduckgo.com/?q=exact+phrase&ia=web").as_deref(),
            Some("exact phrase")
        );
        assert!(duckduckgo_search_query("https://example.com/?q=private").is_none());
    }

    #[test]
    fn persists_history_entries_and_updates_only_the_latest_matching_url() {
        let directory = std::env::temp_dir().join(format!(
            "folio-browser-history-test-{}-{}",
            std::process::id(),
            unix_millis()
        ));
        let path = directory.join("history.sqlite3");

        let first = {
            let history = HistoryStore::open(&path).unwrap();
            let first = history.record_attempt("https://example.com/").unwrap();
            history
                .update_status("https://example.com/", NavigationStatus::Completed)
                .unwrap();
            history
                .update_title("https://example.com/", "First title")
                .unwrap();
            let second = history.record_attempt("https://example.com/").unwrap();
            history
                .update_status("https://example.com/", NavigationStatus::Started)
                .unwrap();
            history
                .update_title("https://example.com/", "Second title")
                .unwrap();

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
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn formats_export_timestamp_as_iso_utc() {
        assert_eq!(timestamp_iso(0).unwrap(), "1970-01-01T00:00:00Z");
    }
}
