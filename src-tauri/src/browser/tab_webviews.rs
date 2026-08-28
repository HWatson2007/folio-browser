use super::tabs::{TabManager, TabSummary};
use super::{HOME_URL, PLACEHOLDER_URL};
use crate::download::attach_download_handler;
use crate::history::{HistoryEntry, NavigationStatus, PendingNavigation};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde::Serialize;
use std::{
    collections::HashMap,
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

pub(super) struct LayoutState {
    pub(super) toolbar_height: Mutex<f64>,
    pub(super) content_hidden: AtomicBool,
    pub(super) tab_overlay_height: Mutex<Option<f64>>,
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
                                // This data URI is retained in tab state and included in every
                                // tab-list IPC event. Reject abnormally large favicons before
                                // base64 encoding multiplies their memory and transport cost.
                                if bytes.len() > 256 * 1024 {
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

pub(super) fn open_content_tab(
    app: &tauri::AppHandle,
    tabs: &Arc<TabManager>,
    layout: &Arc<LayoutState>,
    initial: Option<(tauri::Url, Option<PendingNavigation>)>,
) -> Result<TabSummary, String> {
    let view_guard = tabs.view_lock.lock().map_err(|error| error.to_string())?;
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
                matches!(url.scheme(), "http" | "https")
                    || (url.path() == "/blank.html" && matches!(url.scheme(), "tauri" | "asset"))
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
            let _ = activate_tab_webview_locked(app, tabs, layout, previous_active, true);
        }
    }
    drop(view_guard);
    result
}

pub(super) fn resolve_input(
    input: &str,
    source: &str,
) -> Result<(tauri::Url, PendingNavigation), String> {
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

pub(super) fn content_webview(
    app: &tauri::AppHandle,
    tabs: &TabManager,
) -> Result<(u64, tauri::Webview), String> {
    let (id, label) = tabs.active()?;
    app.get_webview(&label)
        .map(|webview| (id, webview))
        .ok_or_else(|| "The active tab's webview is not ready.".to_owned())
}

pub(super) fn apply_layout(
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
    let _view_guard = tabs.view_lock.lock().map_err(|error| error.to_string())?;
    activate_tab_webview_locked(app, tabs, layout, id, focus)
}

fn activate_tab_webview_locked(
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
pub(super) fn get_tabs(tabs: State<'_, Arc<TabManager>>) -> Result<Vec<TabSummary>, String> {
    tabs.summaries()
}

#[tauri::command]
pub(super) fn create_tab(
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
pub(super) fn activate_tab(
    app: tauri::AppHandle,
    tabs: State<'_, Arc<TabManager>>,
    layout: State<'_, Arc<LayoutState>>,
    id: u64,
) -> Result<(), String> {
    activate_tab_webview(&app, &tabs, &layout, id, true)
}

#[tauri::command]
pub(super) fn cycle_tab(
    app: tauri::AppHandle,
    tabs: State<'_, Arc<TabManager>>,
    layout: State<'_, Arc<LayoutState>>,
    direction: i32,
) -> Result<(), String> {
    let id = tabs.cycle_target(direction)?;
    activate_tab_webview(&app, &tabs, &layout, id, true)
}

#[tauri::command]
pub(super) fn close_tab(
    app: tauri::AppHandle,
    tabs: State<'_, Arc<TabManager>>,
    layout: State<'_, Arc<LayoutState>>,
    id: u64,
) -> Result<(), String> {
    let _view_guard = tabs.view_lock.lock().map_err(|error| error.to_string())?;
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
        activate_tab_webview_locked(&app, &tabs, &layout, next_id, true)
    } else {
        emit_tabs(&app, &tabs);
        Ok(())
    }
}

#[tauri::command]
pub(super) fn navigate(
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
    if let Err(error) = content.navigate(url) {
        tabs.history.clear_pending(tab_id);
        return Err(error.to_string());
    }
    Ok(())
}

#[tauri::command]
pub(super) fn navigate_home(
    app: tauri::AppHandle,
    tabs: State<'_, Arc<TabManager>>,
) -> Result<(), String> {
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
    if let Err(error) = content.navigate(url) {
        tabs.history.clear_pending(tab_id);
        return Err(error.to_string());
    }
    Ok(())
}

#[tauri::command]
pub(super) fn go_back(
    app: tauri::AppHandle,
    tabs: State<'_, Arc<TabManager>>,
) -> Result<(), String> {
    content_webview(&app, &tabs)?
        .1
        .eval("window.history.back()")
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(super) fn go_forward(
    app: tauri::AppHandle,
    tabs: State<'_, Arc<TabManager>>,
) -> Result<(), String> {
    content_webview(&app, &tabs)?
        .1
        .eval("window.history.forward()")
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(super) fn reload(
    app: tauri::AppHandle,
    tabs: State<'_, Arc<TabManager>>,
) -> Result<(), String> {
    content_webview(&app, &tabs)?
        .1
        .reload()
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(super) fn current_url(
    app: tauri::AppHandle,
    tabs: State<'_, Arc<TabManager>>,
) -> Result<String, String> {
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
pub(super) fn set_content_visible(
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
pub(super) fn set_content_offset(
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
pub(super) fn set_tab_overlay_height(
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
