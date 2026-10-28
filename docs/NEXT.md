# Where to resume

State (2026-10-28, later): builds clean, `cargo test --release` passes (4 suites; `bench` and `memory` are opt-in).

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
- No file locks for files under 64 MB (read into memory, handle closed); bigger files are mapped and
  can't be truncated/replaced by other programs while their tab is open. Search snapshots share the
  typed-text buffer (copy-on-write).
- Memory: 5 x 1.5 GB docs + 25k-hit search peaks at 28 MB private; 2 MB after closing
- Split view: each new pane gets its own empty tab (no mirroring); right-click a tab > Show in <pane>;
  files dropped onto the window go to the pane under the mouse (asks Windows for the cursor, since
  winit drops carry no position), which is highlighted while dragging
- Panes scroll independently: each (pane, tab) keeps its own scroll position, so even one file in two
  panes scrolls separately; panes remember their place per tab. Draggable dividers resize panes
  (min 120 pt wide / 80 pt tall), double-click a divider to equalise; closing a tab never leaves one
  file in two panes
- Appearance window: Colours / Font & size / Profiles tabs, Reset to defaults; 8 theme parts
  (background, text, cursor, selection, current line, line numbers, search matches, bars), each a
  swatch that opens the standard Windows colour dialog (custom colours remembered in te.cfg);
  profiles saved as profiles/<name>.cfg next to te.exe
- Per-pane quick search: in split view each pane header has its own search box (< > / Enter /
  Shift+Enter, case-insensitive), separate from the main find bar, highlighting only in that pane
- GUI checked by hand: tabs, HTML + JSON pretty print, split view, search across tabs, folder panel, logo

## Open (pick up here)
1. GUI-test by hand (not automatable while someone uses the PC): per-pane search, the colour dialog,
   divider dragging, independent scroll
   on one file in two panes, a real drag-and-drop onto each pane,
   profile Load/Delete, 4-pane grid, folder search result in an unopened file, encode/decode + hash
   from menus, Save as / Save all, unsaved-changes prompts.
   Testing note: SendKeys letters arrive as Unicode packets on a non-Latin layout (no Key event), so
   use real virtual keys (keybd_event) when scripting shortcuts like Ctrl+Z.
2. Known limits: two panes on one file share one cursor/selection (scroll is separate); undo history resets on save;
   lines > 20k chars draw truncated; regex matches > 64 KiB across an 8 MiB block edge can be cut short.
3. Solid colour presets: user said "leave it for now".
