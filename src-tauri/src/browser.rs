use crate::download::{
    DownloadEntry, DownloadManager, DownloadStore, attach_download_handler, cancel_active_download,
    open_completed_download,
};
use crate::history::{
    HistoryEntry, HistoryStore, NavigationStatus, PendingNavigation, timestamp_iso,
};
use crate::profile::{ProfileId, ProfileLock, ProfileRegistry, ProfileSummary};
use serde::Serialize;
use std::{
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
    webview::{NewWindowResponse, PageLoadEvent, WebviewBuilder},
};

const HOME_URL: &str = "https://duckduckgo.com/";
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

            let profile = registry
                .find(&profile_id)?
                .ok_or_else(|| format!("Unknown profile id: {}", profile_id.as_str()))?;

            let lock = ProfileLock::acquire(&registry.lock_path(&profile_id), std::process::id())
                .map_err(|message| format!("{message}: {}", profile.name))?;
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

            let navigation_history = history.clone();
            let navigation_handle = app.handle().clone();
            let load_history = history.clone();
            let load_handle = app.handle().clone();
            let popup_handle = app.handle().clone();
            let title_history = history;
            let profile_name = profile.name.clone();

            let content = WebviewBuilder::new(
                "content",
                WebviewUrl::External(tauri::Url::parse(HOME_URL)?),
            );
            let content = content
                .data_directory(webview_dir.clone())
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
                            .set_title(&format!("{title} — {profile_name} — Folio"));
                    }
                });

            let content = window.add_child(
                content,
                LogicalPosition::new(0.0, TOOLBAR_HEIGHT),
                LogicalSize::new(size.width, (size.height - TOOLBAR_HEIGHT).max(1.0)),
            )?;
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
