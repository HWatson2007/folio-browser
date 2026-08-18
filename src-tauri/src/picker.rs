use crate::profile::{ProfileId, ProfileLock, ProfileRegistry, ProfileSummary};
use std::sync::Arc;
use tauri::webview::WebviewBuilder;
use tauri::{LogicalPosition, LogicalSize, Manager, State, WebviewUrl};

#[tauri::command]
fn list_profiles(registry: State<'_, Arc<ProfileRegistry>>) -> Result<Vec<ProfileSummary>, String> {
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
fn create_profile(
    registry: State<'_, Arc<ProfileRegistry>>,
    name: String,
) -> Result<ProfileSummary, String> {
    let name = name.trim().to_owned();
    if name.is_empty() {
        return Err("Enter a name for the new profile.".to_owned());
    }
    let record = registry.create(&name)?;
    Ok(ProfileSummary::from(&record, false))
}

#[tauri::command]
fn rename_profile(
    registry: State<'_, Arc<ProfileRegistry>>,
    id: String,
    name: String,
) -> Result<(), String> {
    let id = ProfileId::parse(&id)?;
    let name = name.trim().to_owned();
    if name.is_empty() {
        return Err("Enter a name for the profile.".to_owned());
    }
    registry.rename(&id, &name)
}

#[tauri::command]
fn delete_profile(registry: State<'_, Arc<ProfileRegistry>>, id: String) -> Result<(), String> {
    let id = ProfileId::parse(&id)?;
    registry.delete(&id)
}

#[tauri::command]
fn launch_profile(registry: State<'_, Arc<ProfileRegistry>>, id: String) -> Result<(), String> {
    let id = ProfileId::parse(&id)?;
    if ProfileLock::is_locked(&registry.lock_path(&id)) {
        return Err("This profile is already open in another window.".to_owned());
    }
    if registry.find(&id)?.is_none() {
        return Err("That profile no longer exists.".to_owned());
    }
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let child = std::process::Command::new(executable)
        .arg("--profile")
        .arg(id.as_str())
        .spawn()
        .map_err(|error| format!("Could not open the profile: {error}"))?;
    drop(child);
    registry.touch_last_used(&id)
}

pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
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
            if let Err(error) = registry.migrate_legacy() {
                eprintln!("Folio profile migration failed: {error}");
            }
            app.manage(Arc::new(registry));

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

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Folio Browser picker");
}
