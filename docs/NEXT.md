# Where to resume

State (2026-10-27): builds clean, `cargo test --release` passes (3 suites). Release exe: target/release/te.exe.

## Done
- Huge-file engine (mmap + piece table + sparse newline index), regex scan in 8 MiB blocks (doc.rs)
- Tabs, split view (1/2/3/4 panes; Ctrl+\ toggles 2), folder tree (Ctrl+B), recent files
- Menu bar: File / Edit / Search / View / Encode / Format, Sharingan logo (house mark, src/logo.rs), exe + window icon (build.rs)
- Find bar (Ctrl+F / Ctrl+H): match case, whole word, regex, replace one/all with $1 groups, live match outlines
- Search everywhere (Ctrl+Shift+F): this file / open tabs / folder, background thread, results panel, caps 5k per file / 50k total
- Encoders/decoders/hashes and JSON/XML/HTML/YAML/TOML/SQL/CSV formatters (tools.rs)
- GUI-tested by hand: tabs, HTML + JSON pretty print, 2-pane split, search across tabs

## Open (pick up here)
1. GUI-test the rest: View > Grid of four, folder search + clicking a result in an unopened file,
   replace/replace all, encode/decode via menu, hashes, Save/Save as/Save all, close-with-unsaved prompts.
2. Verify the active-pane click fix (main.rs `panes()`, `resp.clicked()`), rebuilt but not re-screenshotted.
3. Memory pass: open 4+ large files, run searches, close tabs; confirm memory returns (private ~445 MB is the GPU driver baseline).
4. Review pass (/code-review) over editor.rs, search.rs, main.rs.
5. Known limits: same tab in two panes shares one scroll position; undo history resets on save;
   lines > 20k chars draw truncated; regex matches > 64 KiB across an 8 MiB block edge can be cut short.
6. Solid colour presets: user said "leave it for now".
