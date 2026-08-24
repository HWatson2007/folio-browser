const COMMANDS: &[&str] = &[
    // browser chrome
    "navigate",
    "navigate_home",
    "go_back",
    "go_forward",
    "reload",
    "current_url",
    "set_content_visible",
    "set_content_offset",
    "get_history",
    "get_history_page",
    "export_history",
    "get_current_profile",
    "get_downloads",
    "cancel_download",
    "open_download",
    "show_download_in_folder",
    // profile picker
    "get_migration_error",
    "list_profiles",
    "create_profile",
    "rename_profile",
    "delete_profile",
    "launch_profile",
];

fn main() {
    let attributes = tauri_build::Attributes::new()
        .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS));
    if let Err(error) = tauri_build::try_build(attributes) {
        eprintln!("tauri-build failed: {error}");
        std::process::exit(1);
    }
}
