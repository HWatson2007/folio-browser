use crate::history::{HistoryEntry, HistoryStore, timestamp_iso};
use serde::Serialize;
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
use tauri::State;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportEntry {
    #[serde(flatten)]
    entry: HistoryEntry,
    attempted_at_iso: String,
    updated_at_iso: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct HistoryPage {
    entries: Vec<HistoryEntry>,
    total: u64,
}

#[tauri::command]
pub(super) fn get_history(
    history: State<'_, Arc<HistoryStore>>,
) -> Result<Vec<HistoryEntry>, String> {
    history.entries_newest_first()
}

#[tauri::command]
pub(super) fn get_history_page(
    history: State<'_, Arc<HistoryStore>>,
    limit: Option<u64>,
    offset: Option<u64>,
    query: Option<String>,
) -> Result<HistoryPage, String> {
    let (entries, total) = history.history_page(
        limit.unwrap_or(HistoryStore::MAX_PAGE_LIMIT),
        offset.unwrap_or(0),
        query,
    )?;
    Ok(HistoryPage { entries, total })
}

#[tauri::command]
pub(super) fn export_history(
    history: State<'_, Arc<HistoryStore>>,
    path: String,
    format: String,
) -> Result<usize, String> {
    if format != "json" && format != "csv" {
        return Err("Export format must be json or csv.".to_owned());
    }
    let destination = PathBuf::from(path);
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .map(PathBuf::from);
    if let Some(ref directory) = parent {
        std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    }
    let temp_path = temp_export_path(&destination)?;
    let export_result = (|| -> Result<usize, String> {
        let file = File::create(&temp_path)
            .map_err(|error| format!("Could not create export: {error}"))?;
        let mut writer = BufWriter::new(file);
        let mut count = 0usize;

        match format.as_str() {
            "json" => {
                writer.write_all(b"[").map_err(|error| error.to_string())?;
                let mut first = true;
                history.for_each_entry(|entry| {
                    let exported = ExportEntry {
                        attempted_at_iso: timestamp_iso(entry.attempted_at)?,
                        updated_at_iso: timestamp_iso(entry.updated_at)?,
                        entry,
                    };
                    if !first {
                        writer
                            .write_all(b",\n")
                            .map_err(|error| error.to_string())?;
                    }
                    first = false;
                    // Pretty-print each record but keep the outer array streamed
                    // so only one entry is materialized at a time.
                    let pretty = serde_json::to_string_pretty(&exported)
                        .map_err(|error| error.to_string())?;
                    writer
                        .write_all(pretty.as_bytes())
                        .map_err(|error| error.to_string())?;
                    count += 1;
                    Ok(())
                })?;
                if !first {
                    writer.write_all(b"\n").map_err(|error| error.to_string())?;
                }
                writer
                    .write_all(b"]\n")
                    .map_err(|error| error.to_string())?;
            }
            "csv" => {
                writer
                    .write_all(b"id,attempted_at,attempted_at_iso,updated_at,updated_at_iso,url,title,status,source,submitted_input,search_query,search_url\n")
                    .map_err(|error| error.to_string())?;
                history.for_each_entry(|entry| {
                    let attempted_at_iso = timestamp_iso(entry.attempted_at)?;
                    let updated_at_iso = timestamp_iso(entry.updated_at)?;
                    let status = serde_json::to_value(entry.status)
                        .ok()
                        .and_then(|value| value.as_str().map(str::to_owned))
                        .unwrap_or_default();
                    let fields = [
                        entry.id.to_string(),
                        entry.attempted_at.to_string(),
                        attempted_at_iso,
                        entry.updated_at.to_string(),
                        updated_at_iso,
                        entry.url.clone(),
                        entry.title.clone().unwrap_or_default(),
                        status,
                        entry.source.clone(),
                        entry.submitted_input.clone().unwrap_or_default(),
                        entry.search_query.clone().unwrap_or_default(),
                        entry.search_url.clone().unwrap_or_default(),
                    ];
                    let row = fields
                        .iter()
                        .map(|field| format!("\"{}\"", field.replace('"', "\"\"")))
                        .collect::<Vec<_>>()
                        .join(",");
                    writeln!(writer, "{row}").map_err(|error| error.to_string())?;
                    count += 1;
                    Ok(())
                })?;
            }
            _ => unreachable!(),
        }

        writer.flush().map_err(|error| error.to_string())?;
        Ok(count)
    })();

    match export_result {
        Ok(count) => {
            // Atomic replace: temp file is on the same directory/filesystem as the
            // destination, so rename is atomic on both Windows and POSIX.
            std::fs::rename(&temp_path, &destination).map_err(|error| {
                let _ = std::fs::remove_file(&temp_path);
                error.to_string()
            })?;
            Ok(count)
        }
        Err(error) => {
            let _ = std::fs::remove_file(&temp_path);
            Err(error)
        }
    }
}

fn temp_export_path(destination: &Path) -> Result<PathBuf, String> {
    let file_name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("folio-export");
    let temp_name = format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4().as_simple());
    Ok(destination
        .parent()
        .map(|parent| parent.join(&temp_name))
        .unwrap_or_else(|| PathBuf::from(temp_name)))
}
