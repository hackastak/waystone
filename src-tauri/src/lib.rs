// Waystone desktop — Tauri v2 backend.
//
// Phase 0 (Spike B) proved the JS ↔ Rust ↔ SQLite(FTS5) seam against an
// in-memory seed. Phase 1 makes it real: `open_vault` builds a disposable
// FTS5 index from the markdown files on disk (see `index.rs`), and
// `search_notes` queries that index. The markdown is the source of truth; the
// index is a rebuildable cache living in OS app-data.

mod frontmatter;
mod index;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Mutex;

use notify::{RecursiveMode, Watcher};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

#[derive(Serialize)]
struct NoteHit {
    id: String,
    title: String,
    snippet: String,
    rank: f64,
}

// The contents of one note, loaded for the editor. `body` is the markdown the
// editor renders; `frontmatter` is the raw YAML block kept verbatim so we can
// re-emit it unchanged on save (the editor never sees or touches it). `path` is
// absolute, so save_note can write back without re-resolving against the vault.
#[derive(Serialize)]
struct NoteContents {
    path: String,
    title: String,
    frontmatter: String,
    body: String,
}

// App state: the connection to the current vault's index and the vault's root
// path, or `None` before a vault is opened. The index is vault-specific and
// chosen at runtime, so unlike the spike's startup seed we can't open it until
// the user picks a folder. We keep the root path to resolve a note's
// index-relative path back to an absolute file path.
struct AppState {
    conn: Mutex<Option<rusqlite::Connection>>,
    vault: Mutex<Option<PathBuf>>,
}

// Open a vault: build/refresh its index, then hold the connection for searches.
// Returns a small stats summary so the UI can confirm what got indexed.
#[tauri::command]
fn open_vault(path: String, state: State<'_, AppState>) -> Result<index::IndexStats, String> {
    let vault = PathBuf::from(&path);
    let mut conn = index::open(&vault)?;
    let stats = index::reindex(&mut conn, &vault)?;
    *state.conn.lock().map_err(|e| e.to_string())? = Some(conn);
    *state.vault.lock().map_err(|e| e.to_string())? = Some(vault);
    Ok(stats)
}

// Load a note for editing: look up its file path in the index by id, read the
// file, and split off the frontmatter so the editor only edits the body.
#[tauri::command]
fn open_note(id: String, state: State<'_, AppState>) -> Result<NoteContents, String> {
    let conn_guard = state.conn.lock().map_err(|e| e.to_string())?;
    let conn = conn_guard.as_ref().ok_or("no vault opened")?;
    let (rel_path, title): (String, String) = conn
        .query_row(
            "SELECT path, title FROM notes WHERE id = ?1",
            [&id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| e.to_string())?;

    let vault_guard = state.vault.lock().map_err(|e| e.to_string())?;
    let vault = vault_guard.as_ref().ok_or("no vault opened")?;
    let abs = vault.join(&rel_path);

    let raw = fs::read_to_string(&abs).map_err(|e| e.to_string())?;
    let split = frontmatter::split(&raw);
    Ok(NoteContents {
        path: abs.to_string_lossy().into_owned(),
        title,
        frontmatter: split.frontmatter.unwrap_or("").to_string(),
        body: split.body.to_string(),
    })
}

// Save an edited note: re-prepend the untouched frontmatter block and write the
// file. The `---\n … ---\n` fences match how frontmatter::split peels them off,
// so a load→save round-trip with no edits reproduces the original framing. A
// note that had no frontmatter is written as plain body.
#[tauri::command]
fn save_note(path: String, frontmatter: String, body: String) -> Result<(), String> {
    let contents = if frontmatter.is_empty() {
        body
    } else {
        format!("---\n{frontmatter}---\n{body}")
    };
    fs::write(&path, contents).map_err(|e| e.to_string())
}

#[tauri::command]
fn search_notes(query: String, state: State<'_, AppState>) -> Result<Vec<NoteHit>, String> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let guard = state.conn.lock().map_err(|e| e.to_string())?;
    let conn = match guard.as_ref() {
        Some(c) => c,
        None => return Ok(Vec::new()), // no vault opened yet
    };

    let mut stmt = conn
        .prepare(
            "SELECT id, title, snippet(notes_fts, 2, '[', ']', '…', 8), rank
             FROM notes_fts WHERE notes_fts MATCH ?1 ORDER BY rank LIMIT 20",
        )
        .map_err(|e| e.to_string())?;

    // Prefix-match the last token so partial words match as you type.
    let fts_query = format!("{q}*");
    let rows = stmt
        .query_map([fts_query], |r| {
            Ok(NoteHit {
                id: r.get(0)?,
                title: r.get(1)?,
                snippet: r.get(2)?,
                rank: r.get(3)?,
            })
        })
        .map_err(|e| e.to_string())?;

    let mut hits = Vec::new();
    for row in rows {
        hits.push(row.map_err(|e| e.to_string())?);
    }
    Ok(hits)
}

// File I/O lives in Rust (not the JS fs plugin) — this is the seam the real
// vault read/write/index path is built on.
#[tauri::command]
fn write_note(path: String, contents: String) -> Result<(), String> {
    fs::write(&path, contents).map_err(|e| e.to_string())
}

#[tauri::command]
fn read_note(path: String) -> Result<String, String> {
    fs::read_to_string(&path).map_err(|e| e.to_string())
}

// Watch the vault folder for external changes and push a `vault-change` event
// to the renderer (the inverse of `invoke`: Rust → JS push). Phase 3 will turn
// these events into incremental re-indexing + echo-loop suppression.
#[tauri::command]
fn watch_vault(path: String, app: AppHandle) -> Result<(), String> {
    std::thread::spawn(move || {
        let (tx, rx) = mpsc::channel();
        let mut watcher = match notify::recommended_watcher(tx) {
            Ok(w) => w,
            Err(e) => return eprintln!("failed to create watcher: {e}"),
        };
        if let Err(e) = watcher.watch(Path::new(&path), RecursiveMode::Recursive) {
            return eprintln!("failed to watch {path}: {e}");
        }
        // Blocking on `rx` keeps `watcher` (and the thread) alive.
        for res in rx {
            match res {
                Ok(event) => {
                    let paths: Vec<String> = event
                        .paths
                        .iter()
                        .map(|p| p.to_string_lossy().into_owned())
                        .collect();
                    let _ = app.emit("vault-change", paths);
                }
                Err(e) => eprintln!("watch error: {e}"),
            }
        }
    });
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            conn: Mutex::new(None),
            vault: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            open_vault,
            search_notes,
            open_note,
            save_note,
            write_note,
            read_note,
            watch_vault
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
