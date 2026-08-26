use crate::download::{
    DownloadEntry, DownloadManager, DownloadStore, attach_download_handler, cancel_active_download,
    open_completed_download,
};
use crate::history::{
    HistoryEntry, HistoryStore, NavigationStatus, PendingNavigation, timestamp_iso,
};
use crate::profile::{ProfileId, ProfileRegistry, ProfileSummary};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
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
    time::Duration,
};
use tauri::{
    Emitter, LogicalPosition, LogicalSize, Manager, State, WebviewUrl,
    webview::{Color, NewWindowResponse, WebviewBuilder},
};
use webview2_com::{
    AcceleratorKeyPressedEventHandler, ContentLoadingEventHandler, FaviconChangedEventHandler,
    GetFaviconCompletedHandler,
    Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_FAVICON_IMAGE_FORMAT_PNG, COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN,
        COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN, ICoreWebView2_15,
    },
    NavigationCompletedEventHandler, NavigationStartingEventHandler, take_pwstr,
};
use windows::{
    Win32::{
        Foundation::HWND,
        UI::{
            Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_MENU, VK_SHIFT},
            WindowsAndMessaging::{HWND_TOP, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos},
        },
    },
    core::{Interface, PWSTR},
};

const HOME_URL: &str = "https://duckduckgo.com/";
const PLACEHOLDER_URL: &str = "about:blank";
const TOOLBAR_HEIGHT: f64 = 76.0;

struct LayoutState {
    toolbar_height: Mutex<f64>,
    content_hidden: AtomicBool,
    tab_overlay_height: Mutex<Option<f64>>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TabSummary {
    id: u64,
    title: String,
    url: String,
    favicon: Option<String>,
    loading: bool,
    active: bool,
}

struct TabRecord {
    id: u64,
    label: String,
    title: String,
    url: String,
    favicon: Option<String>,
    loading: bool,
}

struct TabsState {
    next_id: u64,
    active_id: Option<u64>,
    tabs: Vec<TabRecord>,
}

struct TabManager {
    state: Mutex<TabsState>,
    history: Arc<HistoryStore>,
    downloads: Arc<DownloadManager>,
    webview_dir: PathBuf,
    profile_name: String,
}

impl TabManager {
    fn new(
        history: Arc<HistoryStore>,
        downloads: Arc<DownloadManager>,
        webview_dir: PathBuf,
        profile_name: String,
    ) -> Self {
        Self {
            state: Mutex::new(TabsState {
                next_id: 1,
                active_id: None,
                tabs: Vec::new(),
            }),
            history,
            downloads,
            webview_dir,
            profile_name,
        }
    }

    fn summaries_locked(state: &TabsState) -> Vec<TabSummary> {
        state
            .tabs
            .iter()
            .rev()
            .map(|tab| TabSummary {
                id: tab.id,
                title: tab.title.clone(),
                url: if tab.url == PLACEHOLDER_URL {
                    String::new()
                } else {
                    tab.url.clone()
                },
                favicon: tab.favicon.clone(),
                loading: tab.loading,
                active: state.active_id == Some(tab.id),
            })
            .collect()
    }

    fn summaries(&self) -> Result<Vec<TabSummary>, String> {
        let state = self.state.lock().map_err(|error| error.to_string())?;
        Ok(Self::summaries_locked(&state))
    }

    fn reserve(&self) -> Result<(u64, String), String> {
        let mut state = self.state.lock().map_err(|error| error.to_string())?;
        let id = state.next_id;
        state.next_id = state.next_id.saturating_add(1);
        let label = format!("content-{id}");
        state.tabs.push(TabRecord {
            id,
            label: label.clone(),
            title: "New Tab".to_owned(),
            url: PLACEHOLDER_URL.to_owned(),
            favicon: None,
            loading: false,
        });
        Ok((id, label))
    }

    fn rollback(&self, id: u64) {
        if let Ok(mut state) = self.state.lock() {
            state.tabs.retain(|tab| tab.id != id);
            if state.active_id == Some(id) {
                state.active_id = None;
            }
        }
    }

    fn active_id(&self) -> Option<u64> {
        self.state.lock().ok()?.active_id
    }

    fn activate(&self, id: u64) -> Result<(Option<String>, String), String> {
        let mut state = self.state.lock().map_err(|error| error.to_string())?;
        let new_tab = state
            .tabs
            .iter()
            .find(|tab| tab.id == id)
            .ok_or_else(|| "That tab is no longer open.".to_owned())?;
        let new_label = new_tab.label.clone();
        let old_label = state.active_id.and_then(|active_id| {
            state
                .tabs
                .iter()
                .find(|tab| tab.id == active_id)
                .map(|tab| tab.label.clone())
        });
        state.active_id = Some(id);
        Ok((old_label, new_label))
    }

    fn active(&self) -> Result<(u64, String), String> {
        let state = self.state.lock().map_err(|error| error.to_string())?;
        let id = state
            .active_id
            .ok_or_else(|| "There is no active tab.".to_owned())?;
        let label = state
            .tabs
            .iter()
            .find(|tab| tab.id == id)
            .map(|tab| tab.label.clone())
            .ok_or_else(|| "The active tab is no longer available.".to_owned())?;
        Ok((id, label))
    }

    fn labels(&self) -> Result<Vec<String>, String> {
        let state = self.state.lock().map_err(|error| error.to_string())?;
        Ok(state.tabs.iter().map(|tab| tab.label.clone()).collect())
    }

    fn remove(&self, id: u64) -> Result<(String, Option<u64>, bool), String> {
        let mut state = self.state.lock().map_err(|error| error.to_string())?;
        let index = state
            .tabs
            .iter()
            .position(|tab| tab.id == id)
            .ok_or_else(|| "That tab is no longer open.".to_owned())?;
        let was_active = state.active_id == Some(id);
        let removed = state.tabs.remove(index);
        let next_id = if state.tabs.is_empty() {
            None
        } else if was_active {
            Some(state.tabs[index.min(state.tabs.len() - 1)].id)
        } else {
            state.active_id
        };
        state.active_id = next_id;
        Ok((removed.label, next_id, was_active))
    }

    fn cycle_target(&self, direction: i32) -> Result<u64, String> {
        let state = self.state.lock().map_err(|error| error.to_string())?;
        if state.tabs.is_empty() {
            return Err("There are no open tabs.".to_owned());
        }
        let current = state
            .tabs
            .iter()
            .position(|tab| Some(tab.id) == state.active_id)
            .unwrap_or(0) as i32;
        let len = state.tabs.len() as i32;
        let next = (current + direction.signum()).rem_euclid(len) as usize;
        Ok(state.tabs[next].id)
    }

    fn update_navigation(&self, id: u64, url: &str, loading: bool) {
        if let Ok(mut state) = self.state.lock()
            && let Some(tab) = state.tabs.iter_mut().find(|tab| tab.id == id)
        {
            let changed = tab.url != url;
            tab.url = url.to_owned();
            tab.loading = loading;
            if changed && url != PLACEHOLDER_URL {
                tab.title = display_url_title(url);
                tab.favicon = None;
            }
        }
    }

    fn update_title(&self, id: u64, title: &str) -> bool {
        if let Ok(mut state) = self.state.lock() {
            let active = state.active_id == Some(id);
            if let Some(tab) = state.tabs.iter_mut().find(|tab| tab.id == id) {
                let title = title.trim();
                if !title.is_empty() {
                    tab.title = title.to_owned();
                }
            }
            active
        } else {
            false
        }
    }

    fn update_favicon(&self, id: u64, favicon: Option<String>) {
        if let Ok(mut state) = self.state.lock()
            && let Some(tab) = state.tabs.iter_mut().find(|tab| tab.id == id)
        {
            tab.favicon = favicon;
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct NavigationEvent {
    tab_id: u64,
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

fn display_url_title(url: &str) -> String {
    tauri::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .filter(|host| !host.is_empty())
        .unwrap_or_else(|| "New Tab".to_owned())
}

fn emit_tabs(app: &tauri::AppHandle, tabs: &TabManager) {
    if let Ok(summaries) = tabs.summaries() {
        let _ = app.emit_to("chrome", "browser:tabs", summaries);
    }
}

fn emit_navigation(
    app: &tauri::AppHandle,
    tabs: &TabManager,
    tab_id: u64,
    tracked: &TrackedNavigation,
    status: NavigationStatus,
) {
    tabs.update_navigation(tab_id, &tracked.url, status != NavigationStatus::Completed);
    let _ = app.emit_to(
        "chrome",
        "browser:navigation",
        NavigationEvent {
            tab_id,
            url: tracked.url.clone(),
            status,
            title: None,
        },
    );
    emit_tabs(app, tabs);
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
    tabs: Arc<TabManager>,
    tab_id: u64,
    tracker: Arc<NavigationTracker>,
) -> Result<(), String> {
    webview
        .with_webview(move |platform| {
            let result = (|| -> windows::core::Result<()> {
                let controller = platform.controller();
                let webview = unsafe { controller.CoreWebView2()? };

                let start_history = tabs.history.clone();
                let start_tracker = tracker.clone();
                let start_app = app.clone();
                let start_tabs = tabs.clone();
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
                        if let Ok(entry) = start_history.record_attempt(tab_id, &url) {
                            if matches!(url.split(':').next(), Some("http" | "https")) {
                                let _ = start_tracker.track(id, &entry);
                            }
                            let _ = start_app.emit_to(
                                "chrome",
                                "browser:navigation",
                                NavigationEvent {
                                    tab_id,
                                    url: entry.url.clone(),
                                    status: entry.status,
                                    title: entry.title,
                                },
                            );
                            start_tabs.update_navigation(tab_id, &entry.url, true);
                            emit_tabs(&start_app, &start_tabs);
                        }
                        Ok(())
                    }));
                unsafe { webview.add_NavigationStarting(&start_handler, &mut 0)? };

                let loading_history = tabs.history.clone();
                let loading_tracker = tracker.clone();
                let loading_app = app.clone();
                let loading_tabs = tabs.clone();
                let loading_handler =
                    ContentLoadingEventHandler::create(Box::new(move |_, args| {
                        let Some(args) = args else {
                            return Ok(());
                        };
                        let id = navigation_id(|value| unsafe { args.NavigationId(value) })?;
                        if let Some(tracked) = loading_tracker.get(id) {
                            let status = NavigationStatus::Started;
                            let _ = loading_history.update_status(tracked.entry_id, status);
                            emit_navigation(&loading_app, &loading_tabs, tab_id, &tracked, status);
                        }
                        Ok(())
                    }));
                unsafe { webview.add_ContentLoading(&loading_handler, &mut 0)? };

                let completed_history = tabs.history.clone();
                let completed_tabs = tabs.clone();
                let completed_handler =
                    NavigationCompletedEventHandler::create(Box::new(move |_, args| {
                        let Some(args) = args else {
                            return Ok(());
                        };
                        let id = navigation_id(|value| unsafe { args.NavigationId(value) })?;
                        if let Some(tracked) = tracker.finish(id) {
                            let status = NavigationStatus::Completed;
                            let _ = completed_history.update_status(tracked.entry_id, status);
                            emit_navigation(&app, &completed_tabs, tab_id, &tracked, status);
                        }
                        Ok(())
                    }));
                unsafe { webview.add_NavigationCompleted(&completed_handler, &mut 0)? };

                Ok(())
            })();
            if let Err(error) = result {
                eprintln!("Could not attach navigation history handlers: {error}");
            }
        })
        .map_err(|error| error.to_string())
}

fn attach_favicon_handler(
    webview: &tauri::Webview,
    app: tauri::AppHandle,
    tabs: Arc<TabManager>,
    tab_id: u64,
) -> Result<(), String> {
    webview
        .with_webview(move |platform| {
            let result = (|| -> windows::core::Result<()> {
                let controller = platform.controller();
                let core = unsafe { controller.CoreWebView2()? };
                let core15: ICoreWebView2_15 = core.cast()?;
                let favicon_app = app.clone();
                let favicon_tabs = tabs.clone();
                let handler = FaviconChangedEventHandler::create(Box::new(move |sender, _| {
                    let Some(sender) = sender else {
                        return Ok(());
                    };
                    let sender15: ICoreWebView2_15 = sender.cast()?;
                    let completed_app = favicon_app.clone();
                    let completed_tabs = favicon_tabs.clone();
                    let completed =
                        GetFaviconCompletedHandler::create(Box::new(move |result, stream| {
                            result?;
                            let Some(stream) = stream else {
                                return Ok(());
                            };
                            let mut bytes = Vec::new();
                            let mut buffer = [0u8; 4096];
                            loop {
                                let mut read = 0u32;
                                unsafe {
                                    stream
                                        .Read(
                                            buffer.as_mut_ptr().cast(),
                                            buffer.len() as u32,
                                            Some(&mut read),
                                        )
                                        .ok()?;
                                }
                                if read == 0 {
                                    break;
                                }
                                bytes.extend_from_slice(&buffer[..read as usize]);
                                if bytes.len() > 4 * 1024 * 1024 {
                                    return Ok(());
                                }
                            }
                            if !bytes.is_empty() {
                                completed_tabs.update_favicon(
                                    tab_id,
                                    Some(format!("data:image/png;base64,{}", BASE64.encode(bytes))),
                                );
                                emit_tabs(&completed_app, &completed_tabs);
                            }
                            Ok(())
                        }));
                    unsafe {
                        sender15.GetFavicon(COREWEBVIEW2_FAVICON_IMAGE_FORMAT_PNG, &completed)?;
                    }
                    Ok(())
                }));
                unsafe { core15.add_FaviconChanged(&handler, &mut 0)? };
                Ok(())
            })();
            if let Err(error) = result {
                eprintln!("Could not attach the favicon handler: {error}");
            }
        })
        .map_err(|error| error.to_string())
}

fn attach_shortcut_handler(webview: &tauri::Webview, app: tauri::AppHandle) -> Result<(), String> {
    webview
        .with_webview(move |platform| {
            let controller = platform.controller();
            let handler = AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else {
                    return Ok(());
                };
                let mut kind = Default::default();
                unsafe { args.KeyEventKind(&mut kind)? };
                if kind != COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN
                    && kind != COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN
                {
                    return Ok(());
                }

                let mut key = 0u32;
                unsafe { args.VirtualKey(&mut key)? };
                let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) < 0 };
                let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) < 0 };
                let alt = unsafe { GetKeyState(VK_MENU.0 as i32) < 0 };
                let shortcut = if ctrl {
                    match key {
                        0x54 => Some("new-tab"),
                        0x57 => Some("close-tab"),
                        0x4c => Some("focus-address"),
                        0x52 => Some("reload"),
                        0x09 if shift => Some("previous-tab"),
                        0x09 => Some("next-tab"),
                        _ => None,
                    }
                } else if alt {
                    match key {
                        0x25 => Some("back"),
                        0x27 => Some("forward"),
                        _ => None,
                    }
                } else {
                    None
                };
                if let Some(shortcut) = shortcut {
                    unsafe { args.SetHandled(true)? };
                    let _ = app.emit_to("chrome", "browser:shortcut", shortcut);
                }
                Ok(())
            }));
            if let Err(error) = unsafe { controller.add_AcceleratorKeyPressed(&handler, &mut 0) } {
                eprintln!("Could not attach browser shortcuts: {error}");
            }
        })
        .map_err(|error| error.to_string())
}

fn open_content_tab(
    app: &tauri::AppHandle,
    tabs: &Arc<TabManager>,
    layout: &Arc<LayoutState>,
    initial: Option<(tauri::Url, Option<PendingNavigation>)>,
) -> Result<TabSummary, String> {
    let previous_active = tabs.active_id();
    let (tab_id, label) = tabs.reserve()?;
    let result = (|| -> Result<TabSummary, String> {
        let window = app
            .get_window("main")
            .ok_or_else(|| "The browser window is not ready.".to_owned())?;
        let physical_size = window.inner_size().map_err(|error| error.to_string())?;
        let scale = window.scale_factor().map_err(|error| error.to_string())?;
        let size = physical_size.to_logical::<f64>(scale);
        let offset = *layout
            .toolbar_height
            .lock()
            .map_err(|error| error.to_string())?;

        let popup_handle = app.clone();
        let tracker = Arc::new(NavigationTracker::default());
        let title_tracker = tracker.clone();
        let title_history = tabs.history.clone();
        let title_tabs = tabs.clone();
        let title_app = app.clone();
        let profile_name = tabs.profile_name.clone();

        let content = WebviewBuilder::new(&label, WebviewUrl::App("blank.html".into()))
            .data_directory(tabs.webview_dir.clone())
            .enable_clipboard_access()
            .background_color(Color(255, 255, 255, 255))
            .on_navigation(move |url| {
                url.path() == "/blank.html" || matches!(url.scheme(), "http" | "https")
            })
            .on_new_window(move |url, _features| {
                let _ = popup_handle.emit_to("chrome", "browser:popup-requested", url.to_string());
                NewWindowResponse::Deny
            })
            .on_document_title_changed(move |webview, title| {
                if let Some(tracked) = title_tracker.current() {
                    let _ = title_history.update_title(tracked.entry_id, &title);
                }
                if title_tabs.update_title(tab_id, &title) {
                    let _ = webview
                        .window()
                        .set_title(&format!("{title} — {profile_name} — Folio"));
                }
                emit_tabs(&title_app, &title_tabs);
            });

        let content = window
            .add_child(
                content,
                LogicalPosition::new(0.0, offset),
                LogicalSize::new(size.width, (size.height - offset).max(1.0)),
            )
            .map_err(|error| error.to_string())?;
        attach_navigation_history(&content, app.clone(), tabs.clone(), tab_id, tracker)?;
        attach_favicon_handler(&content, app.clone(), tabs.clone(), tab_id)?;
        attach_shortcut_handler(&content, app.clone())?;
        attach_download_handler(&content, app.clone(), tabs.downloads.clone())?;

        let (old_label, _) = tabs.activate(tab_id)?;
        if let Some(old_label) = old_label
            && old_label != label
            && let Some(old) = app.get_webview(&old_label)
        {
            old.hide().map_err(|error| error.to_string())?;
        }
        if layout.content_hidden.load(Ordering::Relaxed) {
            content.hide().map_err(|error| error.to_string())?;
        } else {
            content.show().map_err(|error| error.to_string())?;
        }
        raise_chrome(app)?;

        if let Some((url, pending)) = initial {
            if let Some(pending) = pending {
                tabs.history.set_pending(tab_id, pending)?;
            }
            content.navigate(url).map_err(|error| error.to_string())?;
            if !layout.content_hidden.load(Ordering::Relaxed) {
                content.set_focus().map_err(|error| error.to_string())?;
            }
        }

        emit_tabs(app, tabs);
        tabs.summaries()?
            .into_iter()
            .find(|tab| tab.id == tab_id)
            .ok_or_else(|| "Could not read the new tab.".to_owned())
    })();

    if result.is_err() {
        tabs.history.clear_pending(tab_id);
        if let Some(webview) = app.get_webview(&label) {
            let _ = webview.close();
        }
        tabs.rollback(tab_id);
        if let Some(previous_active) = previous_active {
            let _ = activate_tab_webview(app, tabs, layout, previous_active, true);
        }
    }
    result
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

fn raise_chrome(app: &tauri::AppHandle) -> Result<(), String> {
    let Some(chrome) = app.get_webview("chrome") else {
        return Ok(());
    };
    chrome
        .with_webview(|platform| {
            let result = (|| -> windows::core::Result<()> {
                let controller = platform.controller();
                let mut hwnd = HWND::default();
                unsafe {
                    controller.ParentWindow(&mut hwnd)?;
                    SetWindowPos(
                        hwnd,
                        Some(HWND_TOP),
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                    )?;
                }
                Ok(())
            })();
            if let Err(error) = result {
                eprintln!("Could not raise the browser chrome: {error}");
            }
        })
        .map_err(|error| error.to_string())
}

fn content_webview(
    app: &tauri::AppHandle,
    tabs: &TabManager,
) -> Result<(u64, tauri::Webview), String> {
    let (id, label) = tabs.active()?;
    app.get_webview(&label)
        .map(|webview| (id, webview))
        .ok_or_else(|| "The active tab's webview is not ready.".to_owned())
}

fn apply_layout(
    app: &tauri::AppHandle,
    layout: &LayoutState,
    tabs: &TabManager,
) -> Result<(), String> {
    let window = app
        .get_window("main")
        .ok_or_else(|| "The browser window is not ready.".to_owned())?;
    let physical_size = window.inner_size().map_err(|error| error.to_string())?;
    let scale = window.scale_factor().map_err(|error| error.to_string())?;
    let size = physical_size.to_logical::<f64>(scale);
    let offset = *layout
        .toolbar_height
        .lock()
        .map_err(|error| error.to_string())?;
    let content_hidden = layout.content_hidden.load(Ordering::Relaxed);
    let overlay_height = *layout
        .tab_overlay_height
        .lock()
        .map_err(|error| error.to_string())?;

    if let Some(chrome) = app.get_webview("chrome") {
        let chrome_height = if content_hidden {
            size.height
        } else {
            overlay_height.unwrap_or(offset).clamp(offset, size.height)
        };
        chrome
            .set_position(LogicalPosition::new(0.0, 0.0))
            .map_err(|error| error.to_string())?;
        chrome
            .set_size(LogicalSize::new(size.width, chrome_height))
            .map_err(|error| error.to_string())?;
        raise_chrome(app)?;
    }

    for label in tabs.labels()? {
        if let Some(content) = app.get_webview(&label) {
            content
                .set_position(LogicalPosition::new(0.0, offset))
                .map_err(|error| error.to_string())?;
            content
                .set_size(LogicalSize::new(
                    size.width,
                    (size.height - offset).max(1.0),
                ))
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

fn activate_tab_webview(
    app: &tauri::AppHandle,
    tabs: &TabManager,
    layout: &LayoutState,
    id: u64,
    focus: bool,
) -> Result<(), String> {
    let (old_label, new_label) = tabs.activate(id)?;
    if let Some(old_label) = old_label
        && old_label != new_label
        && let Some(old) = app.get_webview(&old_label)
    {
        old.hide().map_err(|error| error.to_string())?;
    }
    let new = app
        .get_webview(&new_label)
        .ok_or_else(|| "That tab's webview is not ready.".to_owned())?;
    if layout.content_hidden.load(Ordering::Relaxed) {
        new.hide().map_err(|error| error.to_string())?;
    } else {
        new.show().map_err(|error| error.to_string())?;
        if focus {
            new.set_focus().map_err(|error| error.to_string())?;
        }
    }
    if let Some(active) = tabs.summaries()?.into_iter().find(|tab| tab.active) {
        let _ = new
            .window()
            .set_title(&format!("{} — {} — Folio", active.title, tabs.profile_name));
    }
    emit_tabs(app, tabs);
    Ok(())
}

#[tauri::command]
fn get_tabs(tabs: State<'_, Arc<TabManager>>) -> Result<Vec<TabSummary>, String> {
    tabs.summaries()
}

#[tauri::command]
fn create_tab(
    app: tauri::AppHandle,
    tabs: State<'_, Arc<TabManager>>,
    layout: State<'_, Arc<LayoutState>>,
    url: Option<String>,
) -> Result<(), String> {
    let initial = match url {
        Some(value) => {
            let url = tauri::Url::parse(&value).map_err(|error| error.to_string())?;
            if !matches!(url.scheme(), "http" | "https") {
                return Err("Only HTTP and HTTPS popup addresses are supported.".to_owned());
            }
            let target_url = url.to_string();
            Some((
                url,
                Some(PendingNavigation {
                    target_url,
                    source: "popup".to_owned(),
                    submitted_input: None,
                    search_query: None,
                    search_url: None,
                }),
            ))
        }
        None => None,
    };

    // `add_child` dispatches native creation to the main thread and waits for it. Starting it
    // inside WebView2's IPC callback (or in a main-thread task) makes that thread wait on itself.
    // Let the callback return, then initiate creation from a worker that can safely wait.
    let create_app = app.clone();
    let create_tabs = tabs.inner().clone();
    let create_layout = layout.inner().clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(10));
        if let Err(error) = open_content_tab(&create_app, &create_tabs, &create_layout, initial) {
            let _ = create_app.emit_to("chrome", "browser:tab-error", error);
        }
    });
    Ok(())
}

#[tauri::command]
fn activate_tab(
    app: tauri::AppHandle,
    tabs: State<'_, Arc<TabManager>>,
    layout: State<'_, Arc<LayoutState>>,
    id: u64,
) -> Result<(), String> {
    activate_tab_webview(&app, &tabs, &layout, id, true)
}

#[tauri::command]
fn cycle_tab(
    app: tauri::AppHandle,
    tabs: State<'_, Arc<TabManager>>,
    layout: State<'_, Arc<LayoutState>>,
    direction: i32,
) -> Result<(), String> {
    let id = tabs.cycle_target(direction)?;
    activate_tab_webview(&app, &tabs, &layout, id, true)
}

#[tauri::command]
fn close_tab(
    app: tauri::AppHandle,
    tabs: State<'_, Arc<TabManager>>,
    layout: State<'_, Arc<LayoutState>>,
    id: u64,
) -> Result<(), String> {
    let (label, next_id, was_active) = tabs.remove(id)?;
    tabs.history.clear_pending(id);
    if let Some(webview) = app.get_webview(&label) {
        webview.close().map_err(|error| error.to_string())?;
    }
    let Some(next_id) = next_id else {
        return app
            .get_window("main")
            .ok_or_else(|| "The browser window is not ready.".to_owned())?
            .close()
            .map_err(|error| error.to_string());
    };
    if was_active {
        activate_tab_webview(&app, &tabs, &layout, next_id, true)
    } else {
        emit_tabs(&app, &tabs);
        Ok(())
    }
}

#[tauri::command]
fn navigate(
    app: tauri::AppHandle,
    tabs: State<'_, Arc<TabManager>>,
    input: String,
    source: String,
) -> Result<(), String> {
    let source = match source.as_str() {
        "address" | "history" | "popup" | "home" => source,
        _ => "other".to_owned(),
    };
    let (url, pending) = resolve_input(&input, &source)?;
    let (tab_id, content) = content_webview(&app, &tabs)?;
    tabs.history.set_pending(tab_id, pending)?;
    content.navigate(url).map_err(|error| error.to_string())
}

#[tauri::command]
fn navigate_home(app: tauri::AppHandle, tabs: State<'_, Arc<TabManager>>) -> Result<(), String> {
    let url = tauri::Url::parse(HOME_URL).map_err(|error| error.to_string())?;
    let (tab_id, content) = content_webview(&app, &tabs)?;
    tabs.history.set_pending(
        tab_id,
        PendingNavigation {
            target_url: HOME_URL.to_owned(),
            source: "home".to_owned(),
            submitted_input: None,
            search_query: None,
            search_url: None,
        },
    )?;
    content.navigate(url).map_err(|error| error.to_string())
}

#[tauri::command]
fn go_back(app: tauri::AppHandle, tabs: State<'_, Arc<TabManager>>) -> Result<(), String> {
    content_webview(&app, &tabs)?
        .1
        .eval("window.history.back()")
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn go_forward(app: tauri::AppHandle, tabs: State<'_, Arc<TabManager>>) -> Result<(), String> {
    content_webview(&app, &tabs)?
        .1
        .eval("window.history.forward()")
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn reload(app: tauri::AppHandle, tabs: State<'_, Arc<TabManager>>) -> Result<(), String> {
    content_webview(&app, &tabs)?
        .1
        .reload()
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn current_url(app: tauri::AppHandle, tabs: State<'_, Arc<TabManager>>) -> Result<String, String> {
    let url = content_webview(&app, &tabs)?
        .1
        .url()
        .map_err(|error| error.to_string())?;
    Ok(
        if url.as_str() == PLACEHOLDER_URL || url.path() == "/blank.html" {
            String::new()
        } else {
            url.to_string()
        },
    )
}

#[tauri::command]
fn set_content_visible(
    app: tauri::AppHandle,
    tabs: State<'_, Arc<TabManager>>,
    layout: State<'_, Arc<LayoutState>>,
    visible: bool,
) -> Result<(), String> {
    let (_, content) = content_webview(&app, &tabs)?;
    let chrome = app
        .get_webview("chrome")
        .ok_or_else(|| "The browser chrome is not ready.".to_owned())?;
    layout.content_hidden.store(!visible, Ordering::Relaxed);
    if !visible {
        *layout
            .tab_overlay_height
            .lock()
            .map_err(|error| error.to_string())? = None;
    }
    if visible {
        apply_layout(&app, &layout, &tabs)?;
        content.show().map_err(|error| error.to_string())?;
        content.set_focus().map_err(|error| error.to_string())
    } else {
        content.hide().map_err(|error| error.to_string())?;
        apply_layout(&app, &layout, &tabs)?;
        chrome.set_focus().map_err(|error| error.to_string())
    }
}

#[tauri::command]
fn set_content_offset(
    app: tauri::AppHandle,
    tabs: State<'_, Arc<TabManager>>,
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
    apply_layout(&app, &layout, &tabs)
}

#[tauri::command]
fn set_tab_overlay_height(
    app: tauri::AppHandle,
    tabs: State<'_, Arc<TabManager>>,
    layout: State<'_, Arc<LayoutState>>,
    height: Option<f64>,
) -> Result<(), String> {
    if height.is_some_and(|height| !(48.0..=760.0).contains(&height)) {
        return Err("Invalid tab overlay height.".to_owned());
    }
    *layout
        .tab_overlay_height
        .lock()
        .map_err(|error| error.to_string())? = height;
    apply_layout(&app, &layout, &tabs)
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
    if format != "json" && format != "csv" {
        return Err("Export format must be json or csv.".to_owned());
    }
    let destination = PathBuf::from(path);
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .map(PathBuf::from);
    if let Some(ref directory) = parent {
        std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    }
    let temp_path = temp_export_path(&destination)?;
    let export_result = (|| -> Result<usize, String> {
        let file = File::create(&temp_path)
            .map_err(|error| format!("Could not create export: {error}"))?;
        let mut writer = BufWriter::new(file);
        let mut count = 0usize;

        match format.as_str() {
            "json" => {
                writer.write_all(b"[").map_err(|error| error.to_string())?;
                let mut first = true;
                history.for_each_entry(|entry| {
                    let exported = ExportEntry {
                        attempted_at_iso: timestamp_iso(entry.attempted_at)?,
                        updated_at_iso: timestamp_iso(entry.updated_at)?,
                        entry,
                    };
                    if !first {
                        writer
                            .write_all(b",\n")
                            .map_err(|error| error.to_string())?;
                    }
                    first = false;
                    // Pretty-print each record but keep the outer array streamed
                    // so only one entry is materialized at a time.
                    let pretty = serde_json::to_string_pretty(&exported)
                        .map_err(|error| error.to_string())?;
                    writer
                        .write_all(pretty.as_bytes())
                        .map_err(|error| error.to_string())?;
                    count += 1;
                    Ok(())
                })?;
                if !first {
                    writer.write_all(b"\n").map_err(|error| error.to_string())?;
                }
                writer
                    .write_all(b"]\n")
                    .map_err(|error| error.to_string())?;
            }
            "csv" => {
                writer
                    .write_all(b"id,attempted_at,attempted_at_iso,updated_at,updated_at_iso,url,title,status,source,submitted_input,search_query,search_url\n")
                    .map_err(|error| error.to_string())?;
                history.for_each_entry(|entry| {
                    let attempted_at_iso = timestamp_iso(entry.attempted_at)?;
                    let updated_at_iso = timestamp_iso(entry.updated_at)?;
                    let status = serde_json::to_value(entry.status)
                        .ok()
                        .and_then(|value| value.as_str().map(str::to_owned))
                        .unwrap_or_default();
                    let fields = [
                        entry.id.to_string(),
                        entry.attempted_at.to_string(),
                        attempted_at_iso,
                        entry.updated_at.to_string(),
                        updated_at_iso,
                        entry.url.clone(),
                        entry.title.clone().unwrap_or_default(),
                        status,
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
                    count += 1;
                    Ok(())
                })?;
            }
            _ => unreachable!(),
        }

        writer.flush().map_err(|error| error.to_string())?;
        Ok(count)
    })();

    match export_result {
        Ok(count) => {
            // Atomic replace: temp file is on the same directory/filesystem as the
            // destination, so rename is atomic on both Windows and POSIX.
            std::fs::rename(&temp_path, &destination).map_err(|error| {
                let _ = std::fs::remove_file(&temp_path);
                error.to_string()
            })?;
            Ok(count)
        }
        Err(error) => {
            let _ = std::fs::remove_file(&temp_path);
            Err(error)
        }
    }
}

fn temp_export_path(destination: &std::path::Path) -> Result<PathBuf, String> {
    let file_name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("folio-export");
    let temp_name = format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4().as_simple());
    Ok(destination
        .parent()
        .map(|parent| parent.join(&temp_name))
        .unwrap_or_else(|| PathBuf::from(temp_name)))
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
    tabs: State<'_, Arc<TabManager>>,
    downloads: State<'_, Arc<DownloadManager>>,
    id: u64,
) -> Result<(), String> {
    cancel_active_download(&content_webview(&app, &tabs)?.1, &downloads, id)
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
            get_tabs,
            create_tab,
            activate_tab,
            cycle_tab,
            close_tab,
            navigate,
            navigate_home,
            go_back,
            go_forward,
            reload,
            current_url,
            set_content_visible,
            set_content_offset,
            set_tab_overlay_height,
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
                content_hidden: AtomicBool::new(false),
                tab_overlay_height: Mutex::new(None),
            });
            app.manage(layout.clone());

            let tabs = Arc::new(TabManager::new(
                history.clone(),
                downloads.clone(),
                webview_dir.clone(),
                profile.name.clone(),
            ));
            app.manage(tabs.clone());

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

            let home = tauri::Url::parse(HOME_URL)?;
            let initial_tab = open_content_tab(app.handle(), &tabs, &layout, Some((home, None)))?;

            let chrome = WebviewBuilder::new("chrome", WebviewUrl::App("index.html".into()))
                .data_directory(webview_dir)
                .background_color(Color(0, 0, 0, 0));
            window.add_child(
                chrome,
                LogicalPosition::new(0.0, 0.0),
                LogicalSize::new(size.width, TOOLBAR_HEIGHT),
            )?;

            let resize_handle = app.handle().clone();
            let resize_layout = layout.clone();
            let resize_tabs = tabs.clone();
            window.on_window_event(move |event| {
                if matches!(event, tauri::WindowEvent::Resized(_)) {
                    let _ = apply_layout(&resize_handle, &resize_layout, &resize_tabs);
                }
            });

            if let Some(content) = app.get_webview(&format!("content-{}", initial_tab.id)) {
                let _ = content.set_focus();
            }

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
