use crate::history::unix_millis;
use rusqlite::{Connection, Row, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
    time::Duration,
};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DownloadStatus {
    Requested,
    Downloading,
    Completed,
    Failed,
    Canceled,
    Interrupted,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadEntry {
    pub id: u64,
    pub requested_at: u64,
    pub updated_at: u64,
    pub completed_at: Option<u64>,
    pub url: String,
    pub source_page_url: Option<String>,
    pub suggested_filename: String,
    pub path: Option<String>,
    pub mime_type: Option<String>,
    pub content_disposition: Option<String>,
    pub status: DownloadStatus,
    pub bytes_received: u64,
    pub total_bytes: Option<u64>,
    pub interrupt_reason: Option<String>,
}

pub struct DownloadStore {
    connection: Mutex<Connection>,
}

impl DownloadStatus {
    fn as_database_value(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::Downloading => "downloading",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
            Self::Interrupted => "interrupted",
        }
    }

    fn from_database_value(value: &str) -> Result<Self, String> {
        match value {
            "requested" => Ok(Self::Requested),
            "downloading" => Ok(Self::Downloading),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "canceled" => Ok(Self::Canceled),
            "interrupted" => Ok(Self::Interrupted),
            _ => Err(format!(
                "Download database contains an invalid status: {value}"
            )),
        }
    }
}

impl DownloadStore {
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
                CREATE TABLE IF NOT EXISTS download_entries (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    requested_at INTEGER NOT NULL CHECK (requested_at >= 0),
                    updated_at INTEGER NOT NULL CHECK (updated_at >= 0),
                    completed_at INTEGER,
                    url TEXT NOT NULL,
                    source_page_url TEXT,
                    suggested_filename TEXT NOT NULL,
                    path TEXT,
                    mime_type TEXT,
                    content_disposition TEXT,
                    status TEXT NOT NULL CHECK (status IN (
                        'requested', 'downloading', 'completed', 'failed', 'canceled', 'interrupted'
                    )),
                    bytes_received INTEGER NOT NULL DEFAULT 0 CHECK (bytes_received >= 0),
                    total_bytes INTEGER,
                    interrupt_reason TEXT
                );
                CREATE INDEX IF NOT EXISTS download_entries_newest
                    ON download_entries (requested_at DESC, id DESC);
                ",
            )
            .map_err(|error| error.to_string())?;
        let now = db_integer(unix_millis())?;
        connection
            .execute(
                "UPDATE download_entries
                 SET status = 'interrupted', updated_at = ?, completed_at = ?,
                     interrupt_reason = 'browser_closed'
                 WHERE status IN ('requested', 'downloading')",
                params![now, now],
            )
            .map_err(|error| error.to_string())?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn record_request(
        &self,
        url: &str,
        source_page_url: Option<&str>,
        suggested_filename: &str,
        mime_type: Option<&str>,
        content_disposition: Option<&str>,
        total_bytes: Option<u64>,
    ) -> Result<DownloadEntry, String> {
        let now = unix_millis();
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        connection
            .execute(
                "INSERT INTO download_entries (
                    requested_at, updated_at, url, source_page_url, suggested_filename,
                    mime_type, content_disposition, status, bytes_received, total_bytes
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, 'requested', 0, ?)",
                params![
                    db_integer(now)?,
                    db_integer(now)?,
                    url,
                    source_page_url,
                    suggested_filename,
                    mime_type,
                    content_disposition,
                    optional_db_integer(total_bytes)?,
                ],
            )
            .map_err(|error| error.to_string())?;
        let id = u64::try_from(connection.last_insert_rowid())
            .map_err(|_| "Download database generated an invalid ID.".to_owned())?;
        drop(connection);
        self.get(id)?
            .ok_or_else(|| "Could not read the new download record.".to_owned())
    }

    pub fn begin(&self, id: u64, path: &Path) -> Result<DownloadEntry, String> {
        self.update_state(
            id,
            DownloadStatus::Downloading,
            Some(path.to_string_lossy().as_ref()),
            None,
            false,
        )
    }

    pub fn cancel_before_start(&self, id: u64) -> Result<DownloadEntry, String> {
        self.update_state(
            id,
            DownloadStatus::Canceled,
            None,
            Some("save_dialog_canceled"),
            true,
        )
    }

    pub fn update_progress(
        &self,
        id: u64,
        received: u64,
        total: Option<u64>,
    ) -> Result<DownloadEntry, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        connection
            .execute(
                "UPDATE download_entries
                 SET bytes_received = ?, total_bytes = COALESCE(?, total_bytes), updated_at = ?
                 WHERE id = ? AND status = 'downloading'",
                params![
                    db_integer(received)?,
                    optional_db_integer(total)?,
                    db_integer(unix_millis())?,
                    db_integer(id)?,
                ],
            )
            .map_err(|error| error.to_string())?;
        drop(connection);
        self.get(id)?
            .ok_or_else(|| "Download no longer exists.".to_owned())
    }

    pub fn finish(
        &self,
        id: u64,
        status: DownloadStatus,
        received: u64,
        total: Option<u64>,
        reason: Option<&str>,
    ) -> Result<DownloadEntry, String> {
        let now = unix_millis();
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        connection
            .execute(
                "UPDATE download_entries
                 SET status = ?, bytes_received = ?, total_bytes = COALESCE(?, total_bytes),
                     interrupt_reason = ?, updated_at = ?, completed_at = ?
                 WHERE id = ?",
                params![
                    status.as_database_value(),
                    db_integer(received)?,
                    optional_db_integer(total)?,
                    reason,
                    db_integer(now)?,
                    db_integer(now)?,
                    db_integer(id)?,
                ],
            )
            .map_err(|error| error.to_string())?;
        drop(connection);
        self.get(id)?
            .ok_or_else(|| "Download no longer exists.".to_owned())
    }

    fn update_state(
        &self,
        id: u64,
        status: DownloadStatus,
        path: Option<&str>,
        reason: Option<&str>,
        completed: bool,
    ) -> Result<DownloadEntry, String> {
        let now = unix_millis();
        let completed_at = completed.then_some(db_integer(now)?);
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        connection
            .execute(
                "UPDATE download_entries
                 SET status = ?, path = COALESCE(?, path), interrupt_reason = ?,
                     updated_at = ?, completed_at = ?
                 WHERE id = ?",
                params![
                    status.as_database_value(),
                    path,
                    reason,
                    db_integer(now)?,
                    completed_at,
                    db_integer(id)?,
                ],
            )
            .map_err(|error| error.to_string())?;
        drop(connection);
        self.get(id)?
            .ok_or_else(|| "Download no longer exists.".to_owned())
    }

    pub fn entries_newest_first(&self) -> Result<Vec<DownloadEntry>, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let mut statement = connection
            .prepare(
                "SELECT id, requested_at, updated_at, completed_at, url, source_page_url,
                        suggested_filename, path, mime_type, content_disposition, status,
                        bytes_received, total_bytes, interrupt_reason
                 FROM download_entries ORDER BY requested_at DESC, id DESC",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([], Self::entry_from_row)
            .map_err(|error| error.to_string())?;
        rows.map(|entry| entry.map_err(|error| error.to_string()))
            .collect()
    }

    pub fn get(&self, id: u64) -> Result<Option<DownloadEntry>, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let mut statement = connection
            .prepare(
                "SELECT id, requested_at, updated_at, completed_at, url, source_page_url,
                        suggested_filename, path, mime_type, content_disposition, status,
                        bytes_received, total_bytes, interrupt_reason
                 FROM download_entries WHERE id = ?",
            )
            .map_err(|error| error.to_string())?;
        match statement.query_row([db_integer(id)?], Self::entry_from_row) {
            Ok(entry) => Ok(Some(entry)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    fn entry_from_row(row: &Row<'_>) -> rusqlite::Result<DownloadEntry> {
        let status: String = row.get(10)?;
        Ok(DownloadEntry {
            id: row.get::<_, i64>(0)?.try_into().unwrap_or_default(),
            requested_at: row.get::<_, i64>(1)?.try_into().unwrap_or_default(),
            updated_at: row.get::<_, i64>(2)?.try_into().unwrap_or_default(),
            completed_at: row
                .get::<_, Option<i64>>(3)?
                .and_then(|value| value.try_into().ok()),
            url: row.get(4)?,
            source_page_url: row.get(5)?,
            suggested_filename: row.get(6)?,
            path: row.get(7)?,
            mime_type: row.get(8)?,
            content_disposition: row.get(9)?,
            status: DownloadStatus::from_database_value(&status).map_err(|message| {
                rusqlite::Error::FromSqlConversionFailure(
                    10,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::other(message)),
                )
            })?,
            bytes_received: row.get::<_, i64>(11)?.try_into().unwrap_or_default(),
            total_bytes: row
                .get::<_, Option<i64>>(12)?
                .and_then(|value| value.try_into().ok()),
            interrupt_reason: row.get(13)?,
        })
    }
}

fn db_integer(value: u64) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| "Value exceeds SQLite's integer range.".to_owned())
}

fn optional_db_integer(value: Option<u64>) -> Result<Option<i64>, String> {
    value.map(db_integer).transpose()
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::{cell::RefCell, collections::HashSet, sync::Arc};
    use tauri::{Emitter, Manager};
    use tauri_plugin_dialog::DialogExt;
    use webview2_com::{
        BytesReceivedChangedEventHandler, DownloadStartingEventHandler,
        Microsoft::Web::WebView2::Win32::*, StateChangedEventHandler, take_pwstr,
    };
    use windows::core::{HSTRING, Interface, PWSTR};

    const EVENT_NAME: &str = "browser:download";
    const PROGRESS_EMIT_INTERVAL_MS: u64 = 100;
    const PROGRESS_WRITE_INTERVAL_MS: u64 = 500;

    struct PendingDownload {
        args: ICoreWebView2DownloadStartingEventArgs,
        operation: ICoreWebView2DownloadOperation,
        deferral: ICoreWebView2Deferral,
        total: Option<u64>,
    }

    thread_local! {
        pub(super) static ACTIVE_OPERATIONS: RefCell<HashMap<u64, ICoreWebView2DownloadOperation>> =
            RefCell::new(HashMap::new());
        static PENDING_DOWNLOADS: RefCell<HashMap<u64, PendingDownload>> =
            RefCell::new(HashMap::new());
    }

    struct ProgressClock {
        emitted_at: u64,
        persisted_at: u64,
    }

    pub struct DownloadManager {
        pub store: DownloadStore,
        active: Mutex<HashSet<u64>>,
        clocks: Mutex<HashMap<u64, ProgressClock>>,
    }

    impl DownloadManager {
        pub fn new(store: DownloadStore) -> Self {
            Self {
                store,
                active: Mutex::new(HashSet::new()),
                clocks: Mutex::new(HashMap::new()),
            }
        }

        fn emit(app: &tauri::AppHandle, entry: DownloadEntry) {
            let _ = app.emit_to("chrome", EVENT_NAME, entry);
        }

        fn report_progress(
            &self,
            app: &tauri::AppHandle,
            id: u64,
            received: u64,
            total: Option<u64>,
        ) {
            let now = unix_millis();
            let (persist, emit) = if let Ok(mut clocks) = self.clocks.lock() {
                let clock = clocks.entry(id).or_insert(ProgressClock {
                    emitted_at: 0,
                    persisted_at: 0,
                });
                let persist = now.saturating_sub(clock.persisted_at) >= PROGRESS_WRITE_INTERVAL_MS;
                let emit = now.saturating_sub(clock.emitted_at) >= PROGRESS_EMIT_INTERVAL_MS;
                if persist {
                    clock.persisted_at = now;
                }
                if emit {
                    clock.emitted_at = now;
                }
                (persist, emit)
            } else {
                (true, true)
            };

            if persist {
                if let Ok(entry) = self.store.update_progress(id, received, total)
                    && emit
                {
                    Self::emit(app, entry);
                }
            } else if emit && let Ok(Some(mut entry)) = self.store.get(id) {
                entry.bytes_received = received;
                entry.total_bytes = total.or(entry.total_bytes);
                entry.updated_at = now;
                Self::emit(app, entry);
            }
        }

        pub fn ensure_active(&self, id: u64) -> Result<(), String> {
            let active = self.active.lock().map_err(|error| error.to_string())?;
            if active.contains(&id) {
                Ok(())
            } else {
                Err("This download is no longer in progress.".to_owned())
            }
        }

        fn remove_active(&self, id: u64) {
            if let Ok(mut active) = self.active.lock() {
                active.remove(&id);
            }
            ACTIVE_OPERATIONS.with(|operations| {
                operations.borrow_mut().remove(&id);
            });
            if let Ok(mut clocks) = self.clocks.lock() {
                clocks.remove(&id);
            }
        }
    }

    pub fn attach(
        webview: &tauri::Webview,
        app: tauri::AppHandle,
        manager: Arc<DownloadManager>,
    ) -> Result<(), String> {
        let source_webview_label = webview.label().to_owned();
        webview
            .with_webview(move |platform| {
                let controller = platform.controller();
                let webview = match unsafe { controller.CoreWebView2() } {
                    Ok(value) => value,
                    Err(error) => {
                        eprintln!("Could not access WebView2 for downloads: {error}");
                        return;
                    }
                };
                let webview4: ICoreWebView2_4 = match webview.cast() {
                    Ok(value) => value,
                    Err(error) => {
                        eprintln!("Could not enable WebView2 downloads: {error}");
                        return;
                    }
                };
                let manager_for_start = manager.clone();
                let app_for_start = app.clone();
                let handler = DownloadStartingEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else {
                        return Ok(());
                    };
                    let operation = unsafe { args.DownloadOperation() }?;
                    let url = operation_string(&operation, |operation, value| unsafe {
                        operation.Uri(value)
                    })?;
                    let mime_type = operation_string(&operation, |operation, value| unsafe {
                        operation.MimeType(value)
                    })
                    .ok()
                    .filter(|value| !value.is_empty());
                    let content_disposition =
                        operation_string(&operation, |operation, value| unsafe {
                            operation.ContentDisposition(value)
                        })
                        .ok()
                        .filter(|value| !value.is_empty());
                    let source_page_url = app_for_start
                        .get_webview(&source_webview_label)
                        .and_then(|webview| webview.url().ok())
                        .map(|url| url.to_string());
                    let mut suggested_path = PWSTR::null();
                    unsafe { args.ResultFilePath(&mut suggested_path) }?;
                    let suggested = clean_filename(
                        Path::new(&take_pwstr(suggested_path))
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("download"),
                    );
                    let total = operation_total(&operation);
                    let entry = match manager_for_start.store.record_request(
                        &url,
                        source_page_url.as_deref(),
                        &suggested,
                        mime_type.as_deref(),
                        content_disposition.as_deref(),
                        total,
                    ) {
                        Ok(entry) => entry,
                        Err(error) => {
                            eprintln!("Could not record download: {error}");
                            unsafe { args.SetCancel(true) }?;
                            return Ok(());
                        }
                    };
                    DownloadManager::emit(&app_for_start, entry.clone());

                    let deferral = unsafe { args.GetDeferral() }?;
                    PENDING_DOWNLOADS.with(|pending| {
                        pending.borrow_mut().insert(
                            entry.id,
                            PendingDownload {
                                args,
                                operation,
                                deferral,
                                total,
                            },
                        );
                    });

                    let dialog = app_for_start
                        .dialog()
                        .file()
                        .set_title("Save download")
                        .set_file_name(&suggested);
                    let dialog = if let Some(window) = app_for_start.get_window("main") {
                        dialog.set_parent(&window)
                    } else {
                        dialog
                    };
                    let app_for_dialog = app_for_start.clone();
                    let manager_for_dialog = manager_for_start.clone();
                    dialog.save_file(move |selection| {
                        let selected = selection.and_then(|path| path.into_path().ok());
                        let dispatch_handle = app_for_dialog.clone();
                        let completion_handle = app_for_dialog.clone();
                        let completion_manager = manager_for_dialog.clone();
                        if let Err(error) = dispatch_handle.run_on_main_thread(move || {
                            complete_pending_download(
                                entry.id,
                                selected,
                                &completion_handle,
                                &completion_manager,
                            );
                        }) {
                            eprintln!("Could not finish the download prompt: {error}");
                        }
                    });
                    Ok(())
                }));
                if let Err(error) = unsafe { webview4.add_DownloadStarting(&handler, &mut 0) } {
                    eprintln!("Could not register WebView2 download handler: {error}");
                }
            })
            .map_err(|error| error.to_string())
    }

    fn complete_pending_download(
        id: u64,
        selected: Option<PathBuf>,
        app: &tauri::AppHandle,
        manager: &Arc<DownloadManager>,
    ) {
        let pending = PENDING_DOWNLOADS.with(|downloads| downloads.borrow_mut().remove(&id));
        let Some(pending) = pending else {
            return;
        };

        if let Some(path) = selected {
            let path_text = path.to_string_lossy().into_owned();
            let hpath = HSTRING::from(path_text.as_str());
            let configured = unsafe {
                pending
                    .args
                    .SetResultFilePath(&hpath)
                    .and_then(|_| pending.args.SetHandled(true))
            };
            if configured.is_ok() {
                if let Ok(started) = manager.store.begin(id, &path) {
                    DownloadManager::emit(app, started);
                }
                if let Ok(mut active) = manager.active.lock() {
                    active.insert(id);
                }
                ACTIVE_OPERATIONS.with(|operations| {
                    operations
                        .borrow_mut()
                        .insert(id, pending.operation.clone());
                });
                attach_operation_handlers(id, pending.operation, app.clone(), manager.clone());
            } else {
                let _ = unsafe { pending.args.SetCancel(true) };
                if let Ok(failed) = manager.store.finish(
                    id,
                    DownloadStatus::Failed,
                    0,
                    pending.total,
                    Some("destination_error"),
                ) {
                    DownloadManager::emit(app, failed);
                }
            }
        } else {
            let _ = unsafe { pending.args.SetCancel(true) };
            if let Ok(canceled) = manager.store.cancel_before_start(id) {
                DownloadManager::emit(app, canceled);
            }
        }
        let _ = unsafe { pending.deferral.Complete() };
    }

    fn attach_operation_handlers(
        id: u64,
        operation: ICoreWebView2DownloadOperation,
        app: tauri::AppHandle,
        manager: Arc<DownloadManager>,
    ) {
        let progress_operation = operation.clone();
        let progress_app = app.clone();
        let progress_manager = manager.clone();
        let progress_handler = BytesReceivedChangedEventHandler::create(Box::new(move |_, _| {
            let received = operation_received(&progress_operation);
            let total = operation_total(&progress_operation);
            progress_manager.report_progress(&progress_app, id, received, total);
            Ok(())
        }));
        let _ = unsafe { operation.add_BytesReceivedChanged(&progress_handler, &mut 0) };

        let state_operation = operation.clone();
        let state_handler = StateChangedEventHandler::create(Box::new(move |_, _| {
            let mut state = COREWEBVIEW2_DOWNLOAD_STATE::default();
            unsafe { state_operation.State(&mut state) }?;
            if state == COREWEBVIEW2_DOWNLOAD_STATE_IN_PROGRESS {
                return Ok(());
            }
            let received = operation_received(&state_operation);
            let total = operation_total(&state_operation);
            let (status, reason) = if state == COREWEBVIEW2_DOWNLOAD_STATE_COMPLETED {
                (DownloadStatus::Completed, None)
            } else {
                let mut interrupt = COREWEBVIEW2_DOWNLOAD_INTERRUPT_REASON::default();
                let _ = unsafe { state_operation.InterruptReason(&mut interrupt) };
                let reason = format!("{interrupt:?}");
                let status = if interrupt == COREWEBVIEW2_DOWNLOAD_INTERRUPT_REASON_USER_CANCELED {
                    DownloadStatus::Canceled
                } else {
                    DownloadStatus::Failed
                };
                (status, Some(reason))
            };
            if let Ok(entry) = manager
                .store
                .finish(id, status, received, total, reason.as_deref())
            {
                DownloadManager::emit(&app, entry);
            }
            manager.remove_active(id);
            Ok(())
        }));
        let _ = unsafe { operation.add_StateChanged(&state_handler, &mut 0) };
    }

    fn operation_string(
        operation: &ICoreWebView2DownloadOperation,
        read: impl FnOnce(&ICoreWebView2DownloadOperation, *mut PWSTR) -> windows::core::Result<()>,
    ) -> windows::core::Result<String> {
        let mut value = PWSTR::null();
        read(operation, &mut value)?;
        Ok(take_pwstr(value))
    }

    fn operation_received(operation: &ICoreWebView2DownloadOperation) -> u64 {
        let mut value = 0i64;
        let _ = unsafe { operation.BytesReceived(&mut value) };
        value.try_into().unwrap_or_default()
    }

    fn operation_total(operation: &ICoreWebView2DownloadOperation) -> Option<u64> {
        let mut value = -1i64;
        let _ = unsafe { operation.TotalBytesToReceive(&mut value) };
        value.try_into().ok()
    }
}

#[cfg(windows)]
pub use platform::DownloadManager;

#[cfg(windows)]
pub fn attach_download_handler(
    webview: &tauri::Webview,
    app: tauri::AppHandle,
    manager: std::sync::Arc<DownloadManager>,
) -> Result<(), String> {
    platform::attach(webview, app, manager)
}

#[cfg(windows)]
pub fn cancel_active_download(
    webview: &tauri::Webview,
    manager: &DownloadManager,
    id: u64,
) -> Result<(), String> {
    manager.ensure_active(id)?;
    webview
        .with_webview(move |_| {
            platform::ACTIVE_OPERATIONS.with(|operations| {
                if let Some(operation) = operations.borrow().get(&id) {
                    let _ = unsafe { operation.Cancel() };
                }
            });
        })
        .map_err(|error| error.to_string())
}

pub fn clean_filename(value: &str) -> String {
    let mut cleaned = value
        .chars()
        .map(|character| {
            if character.is_control()
                || matches!(
                    character,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
                )
            {
                '_'
            } else {
                character
            }
        })
        .collect::<String>();
    cleaned = cleaned.trim().trim_end_matches(['.', ' ']).to_owned();
    if cleaned.is_empty() {
        "download".to_owned()
    } else {
        cleaned
    }
}

pub fn open_completed_download(store: &DownloadStore, id: u64, reveal: bool) -> Result<(), String> {
    let entry = store
        .get(id)?
        .ok_or_else(|| "Download no longer exists.".to_owned())?;
    if entry.status != DownloadStatus::Completed {
        return Err("Only completed downloads can be opened.".to_owned());
    }
    let path = PathBuf::from(
        entry
            .path
            .ok_or_else(|| "This download has no saved path.".to_owned())?,
    );
    if !path.exists() {
        return Err("The downloaded file is no longer at its saved location.".to_owned());
    }
    let mut command = Command::new("explorer.exe");
    if reveal {
        command.arg(format!("/select,{}", path.to_string_lossy()));
    } else {
        command.arg(path);
    }
    command.spawn().map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_download_lifecycle_and_marks_stale_work_interrupted() {
        let directory = std::env::temp_dir().join(format!(
            "folio-download-test-{}-{}",
            std::process::id(),
            unix_millis()
        ));
        let path = directory.join("downloads.sqlite3");
        let id = {
            let store = DownloadStore::open(&path).unwrap();
            let entry = store
                .record_request(
                    "https://example.com/file",
                    Some("https://example.com/downloads"),
                    "file.zip",
                    Some("application/zip"),
                    Some("attachment; filename=file.zip"),
                    Some(100),
                )
                .unwrap();
            store
                .begin(entry.id, Path::new("C:/Downloads/file.zip"))
                .unwrap();
            store.update_progress(entry.id, 42, Some(100)).unwrap();
            entry.id
        };
        let store = DownloadStore::open(&path).unwrap();
        let entry = store.get(id).unwrap().unwrap();
        assert_eq!(entry.status, DownloadStatus::Interrupted);
        assert_eq!(
            entry.source_page_url.as_deref(),
            Some("https://example.com/downloads")
        );
        assert_eq!(
            entry.content_disposition.as_deref(),
            Some("attachment; filename=file.zip")
        );
        assert_eq!(entry.bytes_received, 42);
        assert_eq!(entry.interrupt_reason.as_deref(), Some("browser_closed"));
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn removes_path_syntax_from_remote_filenames() {
        assert_eq!(clean_filename("../bad:<name>.zip"), ".._bad__name_.zip");
        assert_eq!(clean_filename("   "), "download");
    }
}
