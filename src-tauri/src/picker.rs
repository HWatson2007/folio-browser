use crate::profile::{ProfileId, ProfileLock, ProfileRegistry, ProfileSummary};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tauri::webview::WebviewBuilder;
use tauri::{AppHandle, LogicalPosition, LogicalSize, Manager, State, WebviewUrl};

const LAUNCH_TIMEOUT: Duration = Duration::from_secs(12);
const LAUNCH_POLL_INTERVAL: Duration = Duration::from_millis(40);

struct PickerState {
    registry: Arc<ProfileRegistry>,
    migration_error: Option<String>,
}

impl PickerState {
    fn registry(&self) -> Result<&ProfileRegistry, String> {
        match &self.migration_error {
            Some(error) => Err(format!(
                "Your existing Folio data could not be migrated safely. No profile changes were made. {error}"
            )),
            None => Ok(&self.registry),
        }
    }
}

#[tauri::command]
fn get_migration_error(state: State<'_, PickerState>) -> Option<String> {
    state.migration_error.clone()
}

#[tauri::command]
fn list_profiles(state: State<'_, PickerState>) -> Result<Vec<ProfileSummary>, String> {
    let registry = state.registry()?;
    let mut profiles = registry.load()?;
    profiles.sort_by(|a, b| {
        b.last_used_at
            .unwrap_or(0)
            .cmp(&a.last_used_at.unwrap_or(0))
    });
    Ok(profiles
        .iter()
        .map(|record| {
            ProfileSummary::from(
                record,
                ProfileLock::is_locked(&registry.lock_path(&record.id)),
            )
        })
        .collect())
}

#[tauri::command]
fn create_profile(state: State<'_, PickerState>, name: String) -> Result<ProfileSummary, String> {
    let registry = state.registry()?;
    let name = name.trim().to_owned();
    if name.is_empty() {
        return Err("Enter a name for the new profile.".to_owned());
    }
    let record = registry.create(&name)?;
    Ok(ProfileSummary::from(&record, false))
}

#[tauri::command]
fn rename_profile(state: State<'_, PickerState>, id: String, name: String) -> Result<(), String> {
    let registry = state.registry()?;
    let id = ProfileId::parse(&id)?;
    let name = name.trim().to_owned();
    if name.is_empty() {
        return Err("Enter a name for the profile.".to_owned());
    }
    registry.rename(&id, &name)
}

#[tauri::command]
fn delete_profile(state: State<'_, PickerState>, id: String) -> Result<(), String> {
    let registry = state.registry()?;
    let id = ProfileId::parse(&id)?;
    registry.delete(&id)
}

/// Keeps the child picker webview aligned with its native parent window.
///
/// Child webviews do not automatically resize when the parent is maximized or resized.
fn apply_picker_layout(app: &AppHandle) -> Result<(), String> {
    let picker = app
        .get_webview("picker")
        .ok_or_else(|| "The profile picker webview is not ready.".to_owned())?;
    let window = picker.window();
    let physical_size = window.inner_size().map_err(|error| error.to_string())?;
    let scale = window.scale_factor().map_err(|error| error.to_string())?;
    let size = physical_size.to_logical::<f64>(scale);

    picker
        .set_position(LogicalPosition::new(0.0, 0.0))
        .map_err(|error| error.to_string())?;
    picker
        .set_size(LogicalSize::new(size.width, size.height))
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn launch_profile(state: State<'_, PickerState>, id: String) -> Result<(), String> {
    let registry = state.registry()?;
    let id = ProfileId::parse(&id)?;
    let _reservation = registry.reserve_launch(&id)?;
    let token = ProfileId::new();
    let ready_path = registry.launch_ready_path(&id, &token);
    let _ = std::fs::remove_file(&ready_path);

    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let mut child = std::process::Command::new(executable)
        .arg("--profile")
        .arg(id.as_str())
        .arg("--launch-token")
        .arg(token.as_str())
        .spawn()
        .map_err(|error| format!("Could not open the profile: {error}"))?;

    let start = Instant::now();
    loop {
        if ready_path.exists() {
            let _ = std::fs::remove_file(&ready_path);
            let _ = registry.touch_last_used(&id);
            return Ok(());
        }
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            return Err(format!(
                "The profile browser exited before it was ready ({status})."
            ));
        }
        if start.elapsed() >= LAUNCH_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Err("The profile browser did not become ready in time.".to_owned());
        }
        std::thread::sleep(LAUNCH_POLL_INTERVAL);
    }
}

pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            get_migration_error,
            list_profiles,
            create_profile,
            rename_profile,
            delete_profile,
            launch_profile
        ])
        .setup(|app| {
            let registry =
                ProfileRegistry::new(app.path().app_data_dir()?, app.path().app_local_data_dir()?);
            let launcher_dir = registry.launcher_dir();
            let migration_error = registry.migrate_legacy().err();
            app.manage(PickerState {
                registry: Arc::new(registry),
                migration_error,
            });

            let window = tauri::window::WindowBuilder::new(app, "picker")
                .title("Folio — Choose a profile")
                .inner_size(780.0, 580.0)
                .min_inner_size(600.0, 460.0)
                .center()
                .background_color(tauri::webview::Color(243, 241, 235, 255))
                .build()?;

            let physical_size = window.inner_size()?;
            let scale = window.scale_factor()?;
            let size = physical_size.to_logical::<f64>(scale);

            let picker = WebviewBuilder::new("picker", WebviewUrl::App("picker.html".into()))
                .data_directory(launcher_dir)
                .background_color(tauri::webview::Color(243, 241, 235, 255));
            window.add_child(
                picker,
                LogicalPosition::new(0.0, 0.0),
                LogicalSize::new(size.width, size.height),
            )?;

            let resize_handle = app.handle().clone();
            window.on_window_event(move |event| {
                if matches!(event, tauri::WindowEvent::Resized(_)) {
                    let _ = apply_picker_layout(&resize_handle);
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Folio Browser picker");
}
