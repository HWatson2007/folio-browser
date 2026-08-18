# Folio Browser

Folio is a lightweight, single-tab Windows browser built with Rust, Tauri 2, TypeScript, HTML, CSS, and the installed Microsoft Edge WebView2 Runtime.

## Features

- One native window and one remote browsing tab
- Address bar with HTTP/HTTPS navigation and DuckDuckGo search
- Back, forward, reload, and home controls
- Popup links redirected into the current tab
- Durable recording of every top-level navigation attempt
- Exact DuckDuckGo query recording, including searches submitted on the DuckDuckGo page
- Searchable local history ledger
- Native Save As export to JSON or CSV
- Unix millisecond and ISO 8601 UTC timestamps in exports

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

## History Data

Browsing history is stored in a local SQLite database at:

```text
%APPDATA%\com.folio.browser\history.sqlite3
```

Each navigation attempt and its later status or title changes are written as SQLite transactions. Exported files are complete snapshots queried from that database.

Remote websites run in a separate child webview without Tauri permissions. Only the trusted local chrome webview can invoke browser, history, or export commands.
