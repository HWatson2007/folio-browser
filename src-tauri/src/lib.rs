mod browser;
mod cli;
mod history;
mod picker;
mod profile;

/// Entry point invoked by `main`. Dispatches to the picker or a per-profile browser.
pub fn run() {
    match cli::parse() {
        cli::AppMode::Picker => picker::run(),
        cli::AppMode::Browser { profile_id } => browser::run(profile_id),
    }
}
