use crate::download::{DownloadManager, DownloadStore};
use crate::history::HistoryStore;
use crate::profile::{ProfileId, ProfileRegistry, ProfileSummary};
use std::sync::{Arc, Mutex, atomic::AtomicBool};
use tauri::{
    LogicalPosition, LogicalSize, Manager, State, WebviewUrl,
    webview::{Color, WebviewBuilder},
};

const HOME_URL: &str = "https://duckduckgo.com/";
const PLACEHOLDER_URL: &str = "about:blank";
const TOOLBAR_HEIGHT: f64 = 76.0;

mod download_commands;
mod history_commands;
mod tab_webviews;
mod tabs;
use tabs::TabManager;

#[tauri::command]
fn get_current_profile(profile: State<'_, crate::profile::ProfileRecord>) -> ProfileSummary {
    ProfileSummary::from(&profile, true)
}

pub fn run(profile_id: ProfileId, launch_token: Option<ProfileId>) {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            tab_webviews::get_tabs,
            tab_webviews::create_tab,
            tab_webviews::activate_tab,
            tab_webviews::cycle_tab,
            tab_webviews::close_tab,
            tab_webviews::navigate,
            tab_webviews::navigate_home,
            tab_webviews::go_back,
            tab_webviews::go_forward,
            tab_webviews::reload,
            tab_webviews::current_url,
            tab_webviews::set_content_visible,
            tab_webviews::set_content_offset,
            tab_webviews::set_tab_overlay_height,
            history_commands::get_history,
            history_commands::get_history_page,
            history_commands::export_history,
            get_current_profile,
            download_commands::get_downloads,
            download_commands::cancel_download,
            download_commands::open_download,
            download_commands::show_download_in_folder
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

            let layout = Arc::new(tab_webviews::LayoutState {
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
            let initial_tab =
                tab_webviews::open_content_tab(app.handle(), &tabs, &layout, Some((home, None)))?;

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
                    let _ =
                        tab_webviews::apply_layout(&resize_handle, &resize_layout, &resize_tabs);
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
        let (url, pending) =
            tab_webviews::resolve_input("rust webview2 history", "address").unwrap();
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
        let (url, pending) = tab_webviews::resolve_input("example.com/path", "address").unwrap();
        assert_eq!(url.as_str(), "https://example.com/path");
        assert!(pending.search_query.is_none());
    }
}
