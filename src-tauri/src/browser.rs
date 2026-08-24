use crate::download::{
    DownloadEntry, DownloadManager, DownloadStore, attach_download_handler, cancel_active_download,
    open_completed_download,
};
use crate::history::{
    HistoryEntry, HistoryStore, NavigationStatus, PendingNavigation, timestamp_iso,
};
use crate::profile::{ProfileId, ProfileRegistry, ProfileSummary};
use serde::Serialize;
use std::{
    collections::HashMap,
    fs::File,
    io::{BufWriter, Write},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tauri::{
    Emitter, LogicalPosition, LogicalSize, Manager, State, WebviewUrl,
    webview::{NewWindowResponse, WebviewBuilder},
};
use webview2_com::{
    ContentLoadingEventHandler, NavigationCompletedEventHandler, NavigationStartingEventHandler,
    take_pwstr,
};
use windows::core::{HSTRING, PWSTR};

const HOME_URL: &str = "https://duckduckgo.com/";
const PLACEHOLDER_URL: &str = "about:blank";
const TOOLBAR_HEIGHT: f64 = 76.0;

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

#[derive(Clone)]
struct TrackedNavigation {
    entry_id: u64,
    url: String,
}

#[derive(Default)]
struct NavigationTrackerState {
    by_navigation_id: HashMap<u64, TrackedNavigation>,
    current: Option<TrackedNavigation>,
}

#[derive(Default)]
struct NavigationTracker {
    state: Mutex<NavigationTrackerState>,
}

impl NavigationTracker {
    fn track(&self, navigation_id: u64, entry: &HistoryEntry) -> Result<(), String> {
        let tracked = TrackedNavigation {
            entry_id: entry.id,
            url: entry.url.clone(),
        };
        let mut state = self.state.lock().map_err(|error| error.to_string())?;
        state
            .by_navigation_id
            .insert(navigation_id, tracked.clone());
        state.current = Some(tracked);
        Ok(())
    }

    fn get(&self, navigation_id: u64) -> Option<TrackedNavigation> {
        self.state
            .lock()
            .ok()?
            .by_navigation_id
            .get(&navigation_id)
            .cloned()
    }

    fn finish(&self, navigation_id: u64) -> Option<TrackedNavigation> {
        self.state
            .lock()
            .ok()?
            .by_navigation_id
            .remove(&navigation_id)
    }

    fn current(&self) -> Option<TrackedNavigation> {
        self.state.lock().ok()?.current.clone()
    }
}

fn emit_navigation(app: &tauri::AppHandle, tracked: &TrackedNavigation, status: NavigationStatus) {
    let _ = app.emit_to(
        "chrome",
        "browser:navigation",
        NavigationEvent {
            url: tracked.url.clone(),
            status,
            title: None,
        },
    );
}

fn navigation_id(
    read: impl FnOnce(*mut u64) -> windows::core::Result<()>,
) -> windows::core::Result<u64> {
    let mut id = 0;
    read(&mut id)?;
    Ok(id)
}

fn attach_navigation_history(
    webview: &tauri::Webview,
    app: tauri::AppHandle,
    history: Arc<HistoryStore>,
    tracker: Arc<NavigationTracker>,
) -> Result<(), String> {
    webview
        .with_webview(move |platform| {
            let result = (|| -> windows::core::Result<()> {
                let controller = platform.controller();
                let webview = unsafe { controller.CoreWebView2()? };

                let start_history = history.clone();
                let start_tracker = tracker.clone();
                let start_app = app.clone();
                let start_handler =
                    NavigationStartingEventHandler::create(Box::new(move |_, args| {
                        let Some(args) = args else {
                            return Ok(());
                        };
                        let mut raw_url = PWSTR::null();
                        unsafe { args.Uri(&mut raw_url)? };
                        let url = take_pwstr(raw_url);
                        if url == PLACEHOLDER_URL {
                            return Ok(());
                        }
                        let id = navigation_id(|value| unsafe { args.NavigationId(value) })?;
                        if let Ok(entry) = start_history.record_attempt(&url) {
                            if matches!(url.split(':').next(), Some("http" | "https")) {
                                let _ = start_tracker.track(id, &entry);
                            }
                            let _ = start_app.emit_to(
                                "chrome",
                                "browser:navigation",
                                NavigationEvent {
                                    url: entry.url,
                                    status: entry.status,
                                    title: entry.title,
                                },
                            );
                        }
                        Ok(())
                    }));
                unsafe { webview.add_NavigationStarting(&start_handler, &mut 0)? };

                let loading_history = history.clone();
                let loading_tracker = tracker.clone();
                let loading_app = app.clone();
                let loading_handler =
                    ContentLoadingEventHandler::create(Box::new(move |_, args| {
                        let Some(args) = args else {
                            return Ok(());
                        };
                        let id = navigation_id(|value| unsafe { args.NavigationId(value) })?;
                        if let Some(tracked) = loading_tracker.get(id) {
                            let status = NavigationStatus::Started;
                            let _ = loading_history.update_status(tracked.entry_id, status);
                            emit_navigation(&loading_app, &tracked, status);
                        }
                        Ok(())
                    }));
                unsafe { webview.add_ContentLoading(&loading_handler, &mut 0)? };

                let completed_handler =
                    NavigationCompletedEventHandler::create(Box::new(move |_, args| {
                        let Some(args) = args else {
                            return Ok(());
                        };
                        let id = navigation_id(|value| unsafe { args.NavigationId(value) })?;
                        if let Some(tracked) = tracker.finish(id) {
                            let status = NavigationStatus::Completed;
                            let _ = history.update_status(tracked.entry_id, status);
                            emit_navigation(&app, &tracked, status);
                        }
                        Ok(())
                    }));
                unsafe { webview.add_NavigationCompleted(&completed_handler, &mut 0)? };

                let home = HSTRING::from(HOME_URL);
                unsafe { webview.Navigate(&home)? };
                Ok(())
            })();
            if let Err(error) = result {
                eprintln!("Could not attach navigation history handlers: {error}");
            }
        })
        .map_err(|error| error.to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportEntry {
    #[serde(flatten)]
    entry: HistoryEntry,
    attempted_at_iso: String,
    updated_at_iso: String,
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HistoryPage {
    entries: Vec<HistoryEntry>,
    total: u64,
}

#[tauri::command]
fn get_history(history: State<'_, Arc<HistoryStore>>) -> Result<Vec<HistoryEntry>, String> {
    history.entries_newest_first()
}

#[tauri::command]
fn get_history_page(
    history: State<'_, Arc<HistoryStore>>,
    limit: Option<u64>,
    offset: Option<u64>,
    query: Option<String>,
) -> Result<HistoryPage, String> {
    let (entries, total) = history.history_page(
        limit.unwrap_or(HistoryStore::MAX_PAGE_LIMIT),
        offset.unwrap_or(0),
        query,
    )?;
    Ok(HistoryPage { entries, total })
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

#[tauri::command]
fn get_current_profile(profile: State<'_, crate::profile::ProfileRecord>) -> ProfileSummary {
    ProfileSummary::from(&profile, true)
}

#[tauri::command]
fn get_downloads(downloads: State<'_, Arc<DownloadManager>>) -> Result<Vec<DownloadEntry>, String> {
    downloads.store.entries_newest_first()
}

#[tauri::command]
fn cancel_download(
    app: tauri::AppHandle,
    downloads: State<'_, Arc<DownloadManager>>,
    id: u64,
) -> Result<(), String> {
    cancel_active_download(&content_webview(&app)?, &downloads, id)
}

#[tauri::command]
fn open_download(downloads: State<'_, Arc<DownloadManager>>, id: u64) -> Result<(), String> {
    open_completed_download(&downloads.store, id, false)
}

#[tauri::command]
fn show_download_in_folder(
    downloads: State<'_, Arc<DownloadManager>>,
    id: u64,
) -> Result<(), String> {
    open_completed_download(&downloads.store, id, true)
}

pub fn run(profile_id: ProfileId, launch_token: Option<ProfileId>) {
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
            get_history_page,
            export_history,
            get_current_profile,
            get_downloads,
            cancel_download,
            open_download,
            show_download_in_folder
        ])
        .setup(move |app| {
            let app_data = app.path().app_data_dir()?;
            let local_data = app.path().app_local_data_dir()?;
            let registry = ProfileRegistry::new(app_data, local_data);

            let (profile, lock) = registry
                .acquire_for_launch(&profile_id)
                .map_err(|message| {
                    if message == "That profile no longer exists." {
                        format!("Unknown profile id: {}", profile_id.as_str())
                    } else {
                        message
                    }
                })?;
            app.manage(lock);

            let history_path = registry.history_path(&profile_id);
            let downloads_path = registry.downloads_path(&profile_id);
            let webview_dir = registry.webview_dir(&profile_id);
            let history = Arc::new(HistoryStore::open(&history_path)?);
            app.manage(history.clone());
            let downloads = Arc::new(DownloadManager::new(DownloadStore::open(&downloads_path)?));
            app.manage(downloads.clone());

            let layout = Arc::new(LayoutState {
                toolbar_height: Mutex::new(TOOLBAR_HEIGHT),
                history_open: AtomicBool::new(false),
            });
            app.manage(layout.clone());

            let profile_state = profile.clone();
            app.manage(profile_state);

            let window = tauri::window::WindowBuilder::new(app, "main")
                .title(format!("Folio — {}", profile.name))
                .inner_size(1240.0, 820.0)
                .min_inner_size(720.0, 520.0)
                .center()
                .background_color(tauri::webview::Color(243, 241, 235, 255))
                .build()?;

            let physical_size = window.inner_size()?;
            let scale = window.scale_factor()?;
            let size = physical_size.to_logical::<f64>(scale);

            let popup_handle = app.handle().clone();
            let title_history = history.clone();
            let navigation_tracker = Arc::new(NavigationTracker::default());
            let title_tracker = navigation_tracker.clone();
            let placeholder_allowed = Arc::new(AtomicBool::new(true));
            let navigation_filter_placeholder = placeholder_allowed.clone();
            let profile_name = profile.name.clone();

            // Start on a neutral page so the native WebView2 handlers can be attached before
            // the first remote navigation begins.
            let content = WebviewBuilder::new(
                "content",
                WebviewUrl::External(tauri::Url::parse(PLACEHOLDER_URL)?),
            );
            let content = content
                .data_directory(webview_dir.clone())
                .enable_clipboard_access()
                .background_color(tauri::webview::Color(255, 255, 255, 255))
                .on_navigation(move |url| {
                    if url.as_str() == PLACEHOLDER_URL {
                        navigation_filter_placeholder.swap(false, Ordering::Relaxed)
                    } else {
                        matches!(url.scheme(), "http" | "https")
                    }
                })
                .on_new_window(move |url, _features| {
                    let _ =
                        popup_handle.emit_to("chrome", "browser:popup-requested", url.to_string());
                    NewWindowResponse::Deny
                })
                .on_document_title_changed(move |webview, title| {
                    if let Some(tracked) = title_tracker.current() {
                        let _ = title_history.update_title(tracked.entry_id, &title);
                    }
                    let _ = webview
                        .window()
                        .set_title(&format!("{title} — {profile_name} — Folio"));
                });

            let content = window.add_child(
                content,
                LogicalPosition::new(0.0, TOOLBAR_HEIGHT),
                LogicalSize::new(size.width, (size.height - TOOLBAR_HEIGHT).max(1.0)),
            )?;
            placeholder_allowed.store(false, Ordering::Relaxed);
            attach_navigation_history(&content, app.handle().clone(), history, navigation_tracker)?;
            attach_download_handler(&content, app.handle().clone(), downloads)?;

            let chrome = WebviewBuilder::new("chrome", WebviewUrl::App("index.html".into()))
                .data_directory(webview_dir)
                .background_color(tauri::webview::Color(243, 241, 235, 255));
            window.add_child(
                chrome,
                LogicalPosition::new(0.0, 0.0),
                LogicalSize::new(size.width, TOOLBAR_HEIGHT),
            )?;

            let resize_handle = app.handle().clone();
            let resize_layout = layout.clone();
            window.on_window_event(move |event| {
                if matches!(event, tauri::WindowEvent::Resized(_)) {
                    let _ = apply_layout(&resize_handle, &resize_layout);
                }
            });

            if let Some(token) = launch_token {
                registry.signal_launch_ready(&profile_id, &token)?;
            }

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
}
