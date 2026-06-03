# FlintBrain — desktop skeleton (Spike B)

Proof-of-architecture for FlintBrain: **Tauri v2 + Vite/React + Milkdown**, with a
single Rust command hitting **SQLite FTS5**. It exists to validate the riskiest
integration before building for real — the JS ↔ Rust ↔ SQLite round-trip.

See the full plan in `~/Developer/My_Notes/1. Projects/FlintBrain/Migration_Plan.md`.

## The architecture this proves (the renderer ↔ Rust split)

```
Renderer (JS, in the webview)          Rust backend (Tauri)
─────────────────────────────          ─────────────────────────────
React UI + Milkdown editor    ──invoke("search_notes")──►  SQLite FTS5 query
  (markdown lives here)       ◄──────  Vec<NoteHit>  ──────  (rusqlite, bundled)
```

- **Milkdown stays in JS** — it's a web editor (ProseMirror); it cannot move to Rust.
- **Search / DB / file I/O live in Rust** — `src-tauri/src/lib.rs` seeds an in-memory
  FTS5 table and exposes `search_notes(query)`. In the real app this becomes the
  on-disk index over the markdown vault.
- The only seam between them is `invoke(...)` — exactly where the old Next.js
  `fetch('/api/notes')` boundary used to be.

## Prerequisites

Node + pnpm are already set up. **You still need Rust** (Xcode CLT is present):

```sh
# one-time, installs to ~/.cargo and ~/.rustup
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
# then restart the shell, or:  source "$HOME/.cargo/env"
```

## Run

```sh
pnpm install          # already done
pnpm tauri dev        # compiles Rust (slow first time ~minutes), opens the window
```

Type in the search box (`rust`, `para`, `markdown`) — results come from Rust/FTS5.
Edit on the right — that's Milkdown.

## What's verified vs. not

- ✅ **Frontend**: `pnpm build` passes (tsc + Vite) — React, Milkdown, and the
  `invoke` call all compile.
- ⏳ **Rust**: written but not yet compiled here (Rust wasn't installed). First
  `pnpm tauri dev` will compile it. One thing to confirm: `rusqlite`'s `bundled`
  feature includes FTS5 (it should); if `CREATE VIRTUAL TABLE ... fts5` errors,
  that's the flag to check. The pinned `rusqlite = "0.32"` may need a version bump.

## Layout

```
src/App.tsx              renderer: Milkdown editor + FTS5 search UI
src-tauri/src/lib.rs     Rust: Db state + search_notes command (FTS5)
src-tauri/Cargo.toml     adds rusqlite (bundled)
```
# flintbrain
