use super::PLACEHOLDER_URL;
use crate::download::DownloadManager;
use crate::history::HistoryStore;
use serde::Serialize;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct TabSummary {
    pub(super) id: u64,
    pub(super) title: String,
    url: String,
    favicon: Option<String>,
    loading: bool,
    pub(super) active: bool,
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

pub(super) struct TabManager {
    state: Mutex<TabsState>,
    // Native child webviews are mutated from both IPC callbacks and tab-creation workers.
    // Keep activation/hide/show/close transitions atomic so concurrent requests cannot leave
    // multiple content webviews visible or disagree with `active_id`.
    pub(super) view_lock: Mutex<()>,
    pub(super) history: Arc<HistoryStore>,
    pub(super) downloads: Arc<DownloadManager>,
    pub(super) webview_dir: PathBuf,
    pub(super) profile_name: String,
}

impl TabManager {
    pub(super) fn new(
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
            view_lock: Mutex::new(()),
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

    pub(super) fn summaries(&self) -> Result<Vec<TabSummary>, String> {
        let state = self.state.lock().map_err(|error| error.to_string())?;
        Ok(Self::summaries_locked(&state))
    }

    pub(super) fn reserve(&self) -> Result<(u64, String), String> {
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

    pub(super) fn rollback(&self, id: u64) {
        if let Ok(mut state) = self.state.lock() {
            state.tabs.retain(|tab| tab.id != id);
            if state.active_id == Some(id) {
                state.active_id = None;
            }
        }
    }

    pub(super) fn active_id(&self) -> Option<u64> {
        self.state.lock().ok()?.active_id
    }

    pub(super) fn activate(&self, id: u64) -> Result<(Option<String>, String), String> {
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

    pub(super) fn active(&self) -> Result<(u64, String), String> {
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

    pub(super) fn labels(&self) -> Result<Vec<String>, String> {
        let state = self.state.lock().map_err(|error| error.to_string())?;
        Ok(state.tabs.iter().map(|tab| tab.label.clone()).collect())
    }

    pub(super) fn remove(&self, id: u64) -> Result<(String, Option<u64>, bool), String> {
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

    pub(super) fn cycle_target(&self, direction: i32) -> Result<u64, String> {
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

    pub(super) fn update_navigation(&self, id: u64, url: &str, loading: bool) {
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

    pub(super) fn update_title(&self, id: u64, title: &str) -> bool {
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

    pub(super) fn update_favicon(&self, id: u64, favicon: Option<String>) {
        if let Ok(mut state) = self.state.lock()
            && let Some(tab) = state.tabs.iter_mut().find(|tab| tab.id == id)
        {
            tab.favicon = favicon;
        }
    }
}

fn display_url_title(url: &str) -> String {
    tauri::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .filter(|host| !host.is_empty())
        .unwrap_or_else(|| "New Tab".to_owned())
}
