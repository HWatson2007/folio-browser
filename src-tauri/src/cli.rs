use crate::profile::ProfileId;

/// Which of the two processes this executable was launched as.
pub enum AppMode {
    /// The profile picker that appears on every normal launch.
    Picker,
    /// A dedicated browser process for one profile.
    Browser { profile_id: ProfileId },
}

pub fn parse() -> AppMode {
    let mut args = std::env::args().skip(1);
    let mut profile: Option<ProfileId> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--profile" | "-p" => match args.next() {
                Some(value) => match ProfileId::parse(&value) {
                    Ok(id) => profile = Some(id),
                    Err(error) => fatal(&format!("Invalid --profile id {value:?}: {error}")),
                },
                None => fatal("--profile requires a profile id."),
            },
            "--help" | "-h" => {
                println!(
                    "Folio Browser\n\nUSAGE:\n    folio-browser [OPTIONS]\n\nOPTIONS:\n    -p, --profile <id>    Open a specific profile directly, skipping the picker\n    -h, --help            Print help"
                );
                std::process::exit(0);
            }
            other => fatal(&format!("Unknown argument: {other}")),
        }
    }

    match profile {
        Some(profile_id) => AppMode::Browser { profile_id },
        None => AppMode::Picker,
    }
}

fn fatal(message: &str) -> ! {
    eprintln!("folio-browser: {message}");
    std::process::exit(2);
}
