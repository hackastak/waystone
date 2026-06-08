import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { Crepe } from "@milkdown/crepe";
import "@milkdown/crepe/theme/common/style.css";
import "@milkdown/crepe/theme/nord.css";
import "./App.css";

type NoteHit = { id: string; title: string; snippet: string; rank: number };
type IndexStats = {
  notes: number;
  links: number;
  tags: number;
  dangling_links: number;
};

const SEED_DOC = `# Hello from Milkdown

This is a **WYSIWYG markdown** editor running inside the Tauri webview.

- Type markdown, watch it render
- [ ] task lists work (GFM)
- [x] this is the renderer half (JS)

> Search (left) runs in **Rust** over SQLite FTS5. The editor (here) is JS.
> That split is the whole point of the Tauri architecture.
`;

// Milkdown is a web editor (ProseMirror) — it lives in the renderer, not Rust.
function Editor() {
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const root = ref.current;
    if (!root) return;
    const crepe = new Crepe({ root, defaultValue: SEED_DOC });
    crepe.create();
    return () => {
      crepe.destroy();
    };
  }, []);

  return <div className="editor" ref={ref} />;
}

function App() {
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<NoteHit[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [vault, setVault] = useState<string | null>(null);
  const [fsResult, setFsResult] = useState<string | null>(null);
  const [lastChange, setLastChange] = useState<string | null>(null);
  const [stats, setStats] = useState<IndexStats | null>(null);

  // When a vault is chosen: index it (Rust scans the markdown into FTS5), then
  // watch it for external changes. open_vault is read-only — it never writes to
  // your files — so it's safe to point at any folder.
  useEffect(() => {
    if (!vault) return;
    let unlisten: (() => void) | undefined;
    (async () => {
      unlisten = await listen<string[]>("vault-change", (e) => {
        setLastChange(e.payload.join("\n"));
      });
      try {
        const s = await invoke<IndexStats>("open_vault", { path: vault });
        setStats(s);
        setError(null);
      } catch (e) {
        setError(String(e));
        setStats(null);
      }
      await invoke("watch_vault", { path: vault });
    })();
    return () => unlisten?.();
  }, [vault]);

  // Native folder picker (tauri-plugin-dialog). `directory: true` makes it a
  // vault-folder chooser — this is the first piece of the real vault picker.
  async function chooseVault() {
    const selected = await open({ directory: true, title: "Choose your vault folder" });
    if (typeof selected === "string") setVault(selected);
  }

  // Prove the Rust file-I/O round-trip: write a .md into the vault, read it back.
  async function testFileIO() {
    if (!vault) return;
    const path = `${vault}/flintbrain-test.md`;
    const contents = "# Test note\n\nWritten by Rust via `invoke`. If you can read this, file I/O works.\n";
    try {
      await invoke("write_note", { path, contents });
      const readBack = await invoke<string>("read_note", { path });
      setFsResult(`Wrote + read ${path}:\n\n${readBack}`);
    } catch (e) {
      setFsResult(`Error: ${String(e)}`);
    }
  }

  async function onSearch(value: string) {
    setQuery(value);
    try {
      // The round-trip we're proving: JS → Rust command → SQLite FTS5 → back.
      const results = await invoke<NoteHit[]>("search_notes", { query: value });
      setHits(results);
      setError(null);
    } catch (e) {
      setError(String(e));
      setHits([]);
    }
  }

  return (
    <div className="app">
      <aside className="sidebar">
        <h1>FlintBrain</h1>
        <p className="tag">Tauri v2 · Rust · SQLite FTS5 · Milkdown</p>

        <button className="vault-btn" onClick={chooseVault}>
          {vault ? "Change vault folder" : "Choose vault folder…"}
        </button>
        {vault && <p className="vault-path">{vault}</p>}
        {stats && (
          <p className="hint">
            Indexed <b>{stats.notes}</b> notes · {stats.links} links (
            {stats.dangling_links} dangling) · {stats.tags} tags
          </p>
        )}
        {vault && (
          <button className="vault-btn" onClick={testFileIO}>
            Write &amp; read a test note
          </button>
        )}
        {fsResult && <pre className="fs-result">{fsResult}</pre>}
        {vault && (
          <p className="hint">
            Watching for external changes. Edit a file in this folder from
            another app to see it below.
          </p>
        )}
        {lastChange && (
          <pre className="fs-result">Changed:{"\n"}{lastChange}</pre>
        )}

        <input
          className="search"
          placeholder={vault ? "Search your vault…" : "Choose a vault first…"}
          value={query}
          onChange={(e) => onSearch(e.target.value)}
        />

        {error && <p className="error">{error}</p>}

        <ul className="results">
          {hits.map((h) => (
            <li key={h.id}>
              <strong>{h.title}</strong>
              <span
                // FTS5 wraps matched terms in [ ] (see snippet() in lib.rs);
                // render those as <mark>. Demo-only innerHTML.
                dangerouslySetInnerHTML={{
                  __html: h.snippet
                    .replace(/\[/g, "<mark>")
                    .replace(/\]/g, "</mark>"),
                }}
              />
            </li>
          ))}
          {query && !hits.length && !error && (
            <li className="empty">No matches</li>
          )}
        </ul>

        <p className="hint">
          Search executes in <b>Rust</b> over the on-disk FTS5 index built from
          your markdown via <code>invoke("search_notes")</code>. The editor on
          the right is JS (Milkdown). That's the renderer ↔ Rust split.
        </p>
      </aside>

      <main className="main">
        <Editor />
      </main>
    </div>
  );
}

export default App;
