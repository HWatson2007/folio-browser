use super::{tab_webviews::content_webview, tabs::TabManager};
use crate::download::{
    DownloadEntry, DownloadManager, cancel_active_download, open_completed_download,
};
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub(super) fn get_downloads(
    downloads: State<'_, Arc<DownloadManager>>,
) -> Result<Vec<DownloadEntry>, String> {
    downloads.store.entries_newest_first()
}

#[tauri::command]
pub(super) fn cancel_download(
    app: tauri::AppHandle,
    tabs: State<'_, Arc<TabManager>>,
    downloads: State<'_, Arc<DownloadManager>>,
    id: u64,
) -> Result<(), String> {
    cancel_active_download(&content_webview(&app, &tabs)?.1, &downloads, id)
}

#[tauri::command]
pub(super) fn open_download(
    downloads: State<'_, Arc<DownloadManager>>,
    id: u64,
) -> Result<(), String> {
    open_completed_download(&downloads.store, id, false)
}

#[tauri::command]
pub(super) fn show_download_in_folder(
    downloads: State<'_, Arc<DownloadManager>>,
    id: u64,
) -> Result<(), String> {
    open_completed_download(&downloads.store, id, true)
}
