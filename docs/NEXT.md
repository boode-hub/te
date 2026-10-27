# Where to resume

State (2026-10-28): builds clean, `cargo test --release` passes (4 suites; `bench` and `memory` are opt-in).

## Done
- Huge-file engine (mmap + piece table + sparse newline index), regex scan in 8 MiB blocks (doc.rs)
- Tabs, split view (1/2/3/4 panes; Ctrl+\ toggles 2), folder tree (Ctrl+B), recent files
- Menu bar: File / Edit / Search / View / Encode / Format
- Logo: Itachi's Mangekyou Sharingan (assets/logo/itachi.svg), rendered by build.rs with resvg
  into every exe icon size and a 128 px image for the window icon and top bar
- Find bar (Ctrl+F / Ctrl+H): match case, whole word, regex, replace one/all with $1 groups, live match outlines
- Search everywhere (Ctrl+Shift+F): this file / open tabs / folder, background thread, results panel,
  caps 5k hits per file / 50k total
- Encoders/decoders/hashes and JSON/XML/HTML/YAML/TOML/SQL/CSV formatters (tools.rs)
- Tests: piece table + regex scan across block edges, every codec round trip, formatters,
  folder search, editor find/replace/tools/undo/line ops/save-and-reload (editor.rs)
- Memory: 5 x 1.5 GB docs + 25k-hit search peaks at 28 MB private; 2 MB after closing
- GUI checked by hand: tabs, HTML + JSON pretty print, split view, search across tabs, folder panel, logo

## Open (pick up here)
1. **File lock bug (next):** an open file is memory-mapped, so Windows blocks other programs from
   overwriting/truncating it while te has it open. Fix in doc.rs `load`: read files under 64 MiB
   into memory (no handle kept); mmap only bigger ones. Also share the typed-text buffer
   (`add: Arc<Vec<u8>>`, `Arc::make_mut` in `insert`) so search snapshots never copy it.
   Both edits were blocked because te had doc.rs open.
2. GUI-test by hand (not automatable while someone uses the PC): 4-pane grid, folder search result
   in an unopened file, encode/decode + hash from menus, Save as / Save all, unsaved-changes prompts.
3. Known limits: same tab in two panes shares one scroll position; undo history resets on save;
   lines > 20k chars draw truncated; regex matches > 64 KiB across an 8 MiB block edge can be cut short.
4. Solid colour presets: user said "leave it for now".
