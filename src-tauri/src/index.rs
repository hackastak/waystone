//! The disposable SQLite/FTS5 index over the markdown vault.
//!
//! The markdown files are the source of truth; this index is a *cache* that can
//! be deleted and rebuilt from the files at any time with zero data loss. It
//! lives in OS app-data (not in the vault), keyed per vault path, so the vault
//! folder stays pure markdown — clean for Dropbox/git/sync. See
//! `Phase1_Format_Decisions.md`.
//!
//! Reindex is two passes: (1) read every `.md` into memory and collect its
//! fields + outgoing `[[wiki-links]]`; (2) in one transaction, wipe the tables
//! and repopulate. Pass 1 builds a title→id map so pass 2 can resolve links by
//! title (an unresolved link is stored "dangling", with `target_id = NULL`).
//!
//! NOTE (Phase 1): this indexer is **read-only** w.r.t. the vault — it never
//! writes to your files. Notes that lack an `id` in frontmatter get an
//! ephemeral nanoid for this build of the index. Persisting ids back into files
//! (`id` write-back on import) is a later, deliberate step, kept separate so
//! pointing FlintBrain at a folder can never mutate it by surprise.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};
use serde::Serialize;
use walkdir::WalkDir;

use crate::frontmatter::{self, FrontMatter};

/// Summary returned to the renderer after a (re)index.
#[derive(Debug, Default, Serialize)]
pub struct IndexStats {
    pub notes: usize,
    pub links: usize,
    pub tags: usize,
    pub dangling_links: usize,
}

/// One note as collected from disk in pass 1, before links are resolved.
struct NoteRecord {
    id: String,
    rel_path: String,
    title: String,
    para: String,
    favorite: bool,
    created: String,
    updated: String,
    body: String,
    tags: Vec<String>,
    link_titles: Vec<String>,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS notes (
    id        TEXT PRIMARY KEY,
    path      TEXT NOT NULL,   -- relative to the vault root, forward slashes
    title     TEXT NOT NULL,
    para      TEXT NOT NULL,   -- inbox|project|area|resource|archive
    favorite  INTEGER NOT NULL DEFAULT 0,
    created   TEXT,
    updated   TEXT,
    body      TEXT NOT NULL
);

-- Standalone FTS5 (not external-content): we rebuild it from files each index,
-- so we don't need the old INSERT/UPDATE/DELETE sync triggers. `id` rides along
-- UNINDEXED so search can return it without a join.
CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(id UNINDEXED, title, body);

-- Derived from [[wiki-links]] each index. target_id NULL = dangling link.
CREATE TABLE IF NOT EXISTS note_links (
    source_id    TEXT NOT NULL,
    target_title TEXT NOT NULL,
    target_id    TEXT
);

CREATE TABLE IF NOT EXISTS tags (
    note_id TEXT NOT NULL,
    tag     TEXT NOT NULL
);
"#;

/// Open (creating if needed) the index db for `vault` and ensure the schema.
pub fn open(vault: &Path) -> Result<Connection, String> {
    let path = index_path(vault)?;
    let conn = Connection::open(&path).map_err(|e| e.to_string())?;
    conn.execute_batch(SCHEMA).map_err(|e| e.to_string())?;
    Ok(conn)
}

/// `~/Library/Application Support/FlintBrain/<vault-key>/index.db` (per OS).
/// The dir is created if missing.
pub fn index_path(vault: &Path) -> Result<PathBuf, String> {
    let base = dirs::data_dir().ok_or("could not locate the OS data directory")?;
    let dir = base.join("FlintBrain").join(vault_key(vault));
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("index.db"))
}

/// Rebuild the whole index from the markdown files under `vault`.
pub fn reindex(conn: &mut Connection, vault: &Path) -> Result<IndexStats, String> {
    // --- Pass 1: read every markdown file into memory. ---
    let mut records: Vec<NoteRecord> = Vec::new();
    for entry in WalkDir::new(vault).into_iter().filter_map(Result::ok) {
        let path = entry.path();
        if is_hidden(path, vault) || !is_markdown(path) {
            continue;
        }
        match read_note(path, vault) {
            Ok(rec) => records.push(rec),
            // A bad file (non-UTF8, malformed YAML) is skipped, not fatal.
            Err(e) => eprintln!("index: skipping {}: {e}", path.display()),
        }
    }

    // Deterministic order so duplicate titles resolve the same way every run.
    records.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

    // title -> id map for resolving [[wiki-links]] by title. On duplicate
    // titles the first by path order wins (deterministic; real disambiguation
    // is deferred — see the format decisions doc).
    let mut title_to_id: HashMap<String, String> = HashMap::new();
    for r in &records {
        title_to_id
            .entry(normalize_title(&r.title))
            .or_insert_with(|| r.id.clone());
    }

    // --- Pass 2: one transaction — wipe, then repopulate (disposable index). ---
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    tx.execute_batch(
        "DELETE FROM notes; DELETE FROM notes_fts; DELETE FROM note_links; DELETE FROM tags;",
    )
    .map_err(|e| e.to_string())?;

    let mut stats = IndexStats::default();
    for r in &records {
        tx.execute(
            "INSERT INTO notes (id, path, title, para, favorite, created, updated, body)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![r.id, r.rel_path, r.title, r.para, r.favorite, r.created, r.updated, r.body],
        )
        .map_err(|e| e.to_string())?;

        tx.execute(
            "INSERT INTO notes_fts (id, title, body) VALUES (?1, ?2, ?3)",
            params![r.id, r.title, r.body],
        )
        .map_err(|e| e.to_string())?;

        for tag in &r.tags {
            tx.execute(
                "INSERT INTO tags (note_id, tag) VALUES (?1, ?2)",
                params![r.id, tag],
            )
            .map_err(|e| e.to_string())?;
            stats.tags += 1;
        }

        for title in &r.link_titles {
            let target_id = title_to_id.get(&normalize_title(title)).cloned();
            if target_id.is_none() {
                stats.dangling_links += 1;
            }
            tx.execute(
                "INSERT INTO note_links (source_id, target_title, target_id) VALUES (?1, ?2, ?3)",
                params![r.id, title, target_id],
            )
            .map_err(|e| e.to_string())?;
            stats.links += 1;
        }

        stats.notes += 1;
    }

    tx.commit().map_err(|e| e.to_string())?;
    Ok(stats)
}

/// Read one file into a `NoteRecord`, filling in fallbacks for missing fields.
fn read_note(path: &Path, vault: &Path) -> Result<NoteRecord, String> {
    let raw = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let split = frontmatter::split(&raw);
    let fm: FrontMatter = match split.frontmatter {
        Some(y) if !y.trim().is_empty() => {
            serde_yaml::from_str(y).map_err(|e| format!("bad frontmatter: {e}"))?
        }
        _ => FrontMatter::default(),
    };
    let body = split.body.to_string();

    let rel_path = path
        .strip_prefix(vault)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");

    // title: frontmatter wins, else the first H1, else the filename.
    let title = fm
        .title
        .clone()
        .or_else(|| first_h1(&body))
        .unwrap_or_else(|| filename_stem(path));

    // id: frontmatter wins, else an ephemeral nanoid (not written back — see
    // the module note). 21 chars is the nanoid default.
    let id = fm.id.clone().unwrap_or_else(|| nanoid::nanoid!());

    let para = para_from_path(&rel_path);
    let (created, updated) = timestamps(&fm, path);
    let link_titles = extract_wikilinks(&body);

    Ok(NoteRecord {
        id,
        rel_path,
        title,
        para,
        favorite: fm.favorite,
        created,
        updated,
        body,
        tags: fm.tags,
        link_titles,
    })
}

/// PARA category = the first path segment, with a `N. ` numbered prefix
/// stripped. Subfolders inherit their top-level category. Anything at the vault
/// root or in an unrecognized folder falls into `inbox` (never dropped).
fn para_from_path(rel_path: &str) -> String {
    let first = rel_path.split('/').next().unwrap_or("");
    // "1. Projects" -> "Projects"; "Projects" -> "Projects".
    let name = first.splitn(2, ". ").nth(1).unwrap_or(first).trim().to_lowercase();
    match name.as_str() {
        "projects" | "project" => "project",
        "areas" | "area" => "area",
        "resources" | "resource" => "resource",
        "archive" => "archive",
        "inbox" => "inbox",
        _ => "inbox",
    }
    .to_string()
}

/// Pull every `[[target]]` out of the body. `[[Note|alias]]` and `[[Note#h]]`
/// resolve on the part before `|`/`#` (full alias/heading support is deferred).
fn extract_wikilinks(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = body.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'[' && bytes[i + 1] == b'[' {
            if let Some(close) = body[i + 2..].find("]]") {
                let inner = &body[i + 2..i + 2 + close];
                let target = inner.split(['|', '#']).next().unwrap_or(inner).trim();
                if !target.is_empty() {
                    out.push(target.to_string());
                }
                i = i + 2 + close + 2;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// created/updated: frontmatter is the source of truth; fall back to the file's
/// mtime (as RFC-3339) when a field is absent. We never write these back.
fn timestamps(fm: &FrontMatter, path: &Path) -> (String, String) {
    let mtime = fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .map(|t| chrono::DateTime::<chrono::Local>::from(t).to_rfc3339())
        .unwrap_or_default();
    let created = fm.created.clone().unwrap_or_else(|| mtime.clone());
    let updated = fm.updated.clone().unwrap_or(mtime);
    (created, updated)
}

fn first_h1(body: &str) -> Option<String> {
    body.lines()
        .find_map(|l| l.strip_prefix("# ").map(|t| t.trim().to_string()))
        .filter(|s| !s.is_empty())
}

fn filename_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Untitled".to_string())
}

fn is_markdown(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("md") | Some("markdown")
    ) && path.is_file()
}

/// Skip dotfiles/dotfolders anywhere in the path under the vault (e.g. `.git`,
/// a future `.flintbrain/`).
fn is_hidden(path: &Path, vault: &Path) -> bool {
    path.strip_prefix(vault)
        .unwrap_or(path)
        .components()
        .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
}

/// Case-insensitive title key for link resolution.
fn normalize_title(t: &str) -> String {
    t.trim().to_lowercase()
}

/// A stable per-vault folder name. FNV-1a over the canonicalized path — small,
/// deterministic, no crypto crate. Not security-sensitive; we only need the
/// same vault to map to the same index dir across runs.
fn vault_key(vault: &Path) -> String {
    let canon = fs::canonicalize(vault).unwrap_or_else(|_| vault.to_path_buf());
    let s = canon.to_string_lossy();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    format!("{hash:016x}")
}
