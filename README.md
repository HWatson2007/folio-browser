# Folio Browser

Folio is a lightweight multi-tab Windows browser built with Rust, Tauri 2, TypeScript, HTML, CSS, and the installed Microsoft Edge WebView2 Runtime.

## Features

- Multiple live tabs in one native window, with page state preserved while switching
- Liquid-glass tab switcher with website favicons, newest-first ordering, and Maple Mono NF CN labels
- New tabs open blank with address focus; each profile launch still starts at DuckDuckGo
- Popup links open in a foreground tab
- **Multi-profile**: fully isolated profiles that run concurrently (one process per profile)
- Profile picker shown on every launch; each profile keeps its own cookies, cache, and history
- Address bar with HTTP/HTTPS navigation and DuckDuckGo search
- Back, forward, reload, and home controls
- Durable recording of every top-level navigation attempt
- Exact DuckDuckGo query recording, including searches submitted on the DuckDuckGo page
- Searchable local history ledger
- Native Save As export to JSON or CSV
- Native per-download Save As prompts with live progress and cancellation
- Persistent, profile-isolated download records with open and reveal actions
- Unix millisecond and ISO 8601 UTC timestamps in exports

## How profiles work

Launching the executable always shows a **profile picker** first. Picking a profile spawns a
separate browser process for it. Each process owns a distinct WebView2 user-data folder and its
own history database, so cookies, cache, localStorage, and the browsing ledger are never shared
between profiles. Several profiles can be open at once.

```text
folio-browser.exe                  → picker window
folio-browser.exe --profile <id>   → browser window for that profile
```

Deleting a profile removes both its history database and its WebView2 data. A profile that is
currently open cannot be deleted. Reopening an already-open profile is refused instead of
starting a second window on the same user-data folder.

## Data locations

```text
%APPDATA%\com.folio.browser\
    profiles.json                 # shared profile registry
    profiles\<id>\
        history.sqlite3           # per-profile browsing ledger
        downloads.sqlite3         # per-profile download ledger
        profile.lock              # exclusive lock while the profile is open

%LOCALAPPDATA%\com.folio.browser\
    launcher\                     # profile picker's own WebView2 data
    profiles\<id>\webview\        # per-profile cookies, cache, storage
```

Each navigation attempt and its later status or title changes are written as SQLite
transactions. Exported files are complete snapshots queried from that database. The first
launch after this feature migrates an existing single-profile install into a `Default`
profile automatically.

Remote websites run in separate child webviews without Tauri permissions. The picker can
manage profiles but cannot navigate the web or read any profile's history; the trusted local
chrome webview can invoke tab, browser, history, and export commands for its own profile only.

The tab switcher supports `Ctrl+T`, `Ctrl+W`, `Ctrl+Tab`, and `Ctrl+Shift+Tab`. Closing the
last tab closes that profile's browser window. Open tabs are intentionally not restored after
restart.

## Development

Prerequisites are Rust with the MSVC target, Visual Studio C++ build tools, Windows SDK, Node.js, pnpm, WebView2 Runtime, and Git.

Install the pinned frontend dependencies:

```powershell
pnpm install --frozen-lockfile
```

Run the app in development mode:

```powershell
pnpm dev:app
```

Do not launch an executable produced by raw `cargo build` for interactive development. Tauri debug mode resolves local app assets through the Vite server, which `pnpm dev:app` starts automatically.

Run validation:

```powershell
pnpm build
cargo test --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo fmt --manifest-path src-tauri/Cargo.toml --all --check
```

Build the Windows application through Tauri:

```powershell
pnpm build:app
```

## Repository layout

```text
src-tauri/src/
    lib.rs       # entry point; dispatches to picker or browser
    cli.rs       # --profile argument parsing
    profile.rs   # registry, per-profile paths, locks, migration
    history.rs   # per-profile browsing SQLite ledger
    download.rs  # WebView2 downloads and per-profile download ledger
    picker.rs    # launcher process and profile commands
    browser.rs   # browser process, chrome + dynamic per-tab content webviews
src/
    main.ts      # browser chrome UI
    picker.ts    # profile picker UI
```

Application commands are gated by Tauri's ACL: `src-tauri/capabilities/picker.json`
restricts the picker to profile management, and `default.json` restricts the chrome to
tab, browser, history, and export commands. Remote content webviews have no capabilities.

The bundled Maple Mono NF CN webfont is distributed under the SIL Open Font License 1.1;
see `assets/MapleMono-LICENSE.txt`.
