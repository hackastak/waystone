// FlintBrain desktop — Tauri v2 skeleton (Spike B).
//
// Proves the JS ↔ Rust ↔ SQLite(FTS5) round-trip that the whole app depends on:
// the renderer calls `invoke("search_notes", { query })`, Rust runs an FTS5
// MATCH query and returns ranked hits. For the skeleton the DB is in-memory and
// seeded at startup; in the real app this becomes the on-disk FTS5 index built
// from the markdown vault.

use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::sync::Mutex;

use notify::{RecursiveMode, Watcher};
use rusqlite::Connection;
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

#[derive(Serialize)]
struct NoteHit {
    id: String,
    title: String,
    snippet: String,
    rank: f64,
}

// App-wide SQLite connection, guarded by a Mutex so Tauri can share it as state.
struct Db(Mutex<Connection>);

fn init_db() -> Connection {
    let conn = Connection::open_in_memory().expect("failed to open sqlite");
    conn.execute_batch(
        r#"
        CREATE VIRTUAL TABLE notes USING fts5(id UNINDEXED, title, body);
        INSERT INTO notes (id, title, body) VALUES
          ('1', 'Welcome to FlintBrain', 'Your PARA second brain. Try searching for rust, markdown, or para.'),
          ('2', 'Rust + Tauri backend',  'The backend is Rust. SQLite FTS5 powers full-text search.'),
          ('3', 'Markdown editing',       'Milkdown gives you a WYSIWYG markdown editor inside the webview.'),
          ('4', 'The PARA method',        'Projects, Areas, Resources, Archive. Structure is the feature.');
        "#,
    )
    .expect("failed to seed db");
    conn
}

#[tauri::command]
fn search_notes(query: String, db: State<'_, Db>) -> Result<Vec<NoteHit>, String> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT id, title, snippet(notes, 2, '[', ']', '…', 8), rank
             FROM notes WHERE notes MATCH ?1 ORDER BY rank LIMIT 20",
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
// vault read/write/index path is built on. For the spike these take an absolute
// path; later they'll take a note id resolved against the vault root.
#[tauri::command]
fn write_note(path: String, contents: String) -> Result<(), String> {
    fs::write(&path, contents).map_err(|e| e.to_string())
}

#[tauri::command]
fn read_note(path: String) -> Result<String, String> {
    fs::read_to_string(&path).map_err(|e| e.to_string())
}

// Watch the vault folder for external changes and push a `vault-change` event
// to the renderer. This is the inverse of `invoke`: Rust → JS push.
//
// The watcher must stay alive to keep firing, so it's created *inside* a thread
// that then blocks on the channel forever — the thread owns it. For the spike
// we just forward changed paths; Phase 3 adds re-indexing + echo-loop
// suppression (don't reprocess FlintBrain's own writes).
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
        .manage(Db(Mutex::new(init_db())))
        .invoke_handler(tauri::generate_handler![
            search_notes,
            write_note,
            read_note,
            watch_vault
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
