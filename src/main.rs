#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod doc;
mod editor;
mod logo;
mod search;
mod tools;

use editor::{Editor, View};
use eframe::egui::{self, Align2, Color32, FontFamily, FontId, Frame, Key, Modifiers, Rect, RichText, Sense, TextEdit, Vec2, pos2, vec2};
use eframe::egui_wgpu;
use search::{Query, Search, Target};
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

const THEMES: &[(&str, &str, &str)] = &[
    ("Midnight", "#1b1d23", "#d7dae0"),
    ("Paper", "#f7f4ec", "#2b2a27"),
    ("Nord", "#2e3440", "#d8dee9"),
    ("Dracula", "#282a36", "#f8f8f2"),
    ("Solar", "#002b36", "#93a1a1"),
    ("Matrix", "#050805", "#3cff6b"),
    ("Amber", "#120c02", "#ffb000"),
    ("Rose", "#1f1418", "#f5c2d0"),
];
const LAYOUTS: &[(&str, usize)] = &[("Single", 1), ("Two side by side", 2), ("Three side by side", 3), ("Grid of four", 4)];
const RECENT_MAX: usize = 12;

struct Settings {
    font: String,
    font_path: String,
    size: f32,
    bg: Color32,
    fg: Color32,
    gutter: bool,
    spacing: f32,
    recent: Vec<String>,
}

impl Settings {
    fn path() -> PathBuf {
        // Portable: settings live next to the exe.
        std::env::current_exe().unwrap_or_default().with_file_name("te.cfg")
    }

    fn load() -> Settings {
        let mut s = Settings {
            font: "Built-in mono".into(),
            font_path: String::new(),
            size: 15.0,
            bg: Color32::from_hex(THEMES[0].1).unwrap(),
            fg: Color32::from_hex(THEMES[0].2).unwrap(),
            gutter: true,
            spacing: 1.25,
            recent: Vec::new(),
        };
        for line in std::fs::read_to_string(Self::path()).unwrap_or_default().lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            match k {
                "font" => s.font = v.into(),
                "font_path" => s.font_path = v.into(),
                "size" => s.size = v.parse().unwrap_or(s.size),
                "bg" => s.bg = Color32::from_hex(v).unwrap_or(s.bg),
                "fg" => s.fg = Color32::from_hex(v).unwrap_or(s.fg),
                "gutter" => s.gutter = v == "1",
                "spacing" => s.spacing = v.parse().unwrap_or(s.spacing),
                "recent" if s.recent.len() < RECENT_MAX => s.recent.push(v.into()),
                _ => {}
            }
        }
        s
    }

    fn serialize(&self) -> String {
        let mut out = format!(
            "font={}\nfont_path={}\nsize={}\nbg={}\nfg={}\ngutter={}\nspacing={}\n",
            self.font,
            self.font_path,
            self.size,
            self.bg.to_hex(),
            self.fg.to_hex(),
            self.gutter as u8,
            self.spacing
        );
        for r in &self.recent {
            out += &format!("recent={r}\n");
        }
        out
    }
}

/// Fonts compiled into the exe (all SIL OFL / Apache, licenses in assets/fonts): (group, name, bytes).
macro_rules! bundled {
    ($($group:literal $name:literal $file:literal),* $(,)?) => {
        &[$(($group, $name, include_bytes!(concat!("../assets/fonts/", $file)))),*]
    };
}
const BUNDLED: &[(&str, &str, &[u8])] = bundled![
    "Game & pixel" "Press Start 2P" "PressStart2P-Regular.ttf",
    "Game & pixel" "VT323" "VT323-Regular.ttf",
    "Game & pixel" "Silkscreen" "Silkscreen-Regular.ttf",
    "Game & pixel" "Pixelify Sans" "PixelifySans-VF.ttf",
    "Game & pixel" "Jersey 10" "Jersey10-Regular.ttf",
    "Sci-fi" "Orbitron" "Orbitron-VF.ttf",
    "Sci-fi" "Audiowide" "Audiowide-Regular.ttf",
    "Sci-fi" "Share Tech Mono" "ShareTechMono-Regular.ttf",
    "Sci-fi" "Major Mono Display" "MajorMonoDisplay-Regular.ttf",
    "Sci-fi" "Michroma" "Michroma-Regular.ttf",
    "Display" "Bungee" "Bungee-Regular.ttf",
    "Display" "Monoton" "Monoton-Regular.ttf",
    "Display" "Black Ops One" "BlackOpsOne-Regular.ttf",
    "Display" "Rubik Glitch" "RubikGlitch-Regular.ttf",
    "Display" "Creepster" "Creepster-Regular.ttf",
    "Display" "Bangers" "Bangers-Regular.ttf",
    "Typewriter & hand" "Special Elite" "SpecialElite-Regular.ttf",
    "Typewriter & hand" "Courier Prime" "CourierPrime-Regular.ttf",
    "Typewriter & hand" "Caveat" "Caveat-VF.ttf",
    "Coding" "JetBrains Mono" "JetBrainsMono-VF.ttf",
    "Coding" "Fira Code" "FiraCode-VF.ttf",
    "Coding" "IBM Plex Mono" "IBMPlexMono-Regular.ttf",
    "Coding" "Space Mono" "SpaceMono-Regular.ttf",
    "Coding" "Victor Mono" "VictorMono-VF.ttf",
];

/// Drop any .ttf/.otf/.ttc in here and it shows up in the picker.
fn user_fonts_dir() -> PathBuf {
    std::env::current_exe().unwrap_or_default().with_file_name("fonts")
}

fn is_font_file(name: &str) -> bool {
    let lower = name.to_lowercase();
    [".ttf", ".otf", ".ttc"].iter().any(|e| lower.ends_with(e))
}

/// Every font the picker offers: (group, display name, source). Source is "bundled:<name>",
/// a file path, or "" for egui's built-in mono.
fn all_fonts() -> Vec<(&'static str, String, String)> {
    let mut out = vec![("Built-in", "Built-in mono".to_string(), String::new())];
    out.extend(BUNDLED.iter().map(|(g, n, _)| (*g, n.to_string(), format!("bundled:{n}"))));
    let mut mine: Vec<_> = std::fs::read_dir(user_fonts_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| is_font_file(&e.file_name().to_string_lossy()))
        .map(|e| {
            let p = e.path();
            let name = p.file_stem().unwrap_or_default().to_string_lossy().into_owned();
            ("Your fonts", name, p.to_string_lossy().into_owned())
        })
        .collect();
    mine.sort();
    out.extend(mine);
    out.extend(system_fonts().into_iter().map(|(n, p)| ("Installed", n, p)));
    out
}

/// Installed fonts from the Windows registry: (display name, file path).
fn system_fonts() -> Vec<(String, String)> {
    use std::os::windows::process::CommandExt;
    let fonts_dir = Path::new(&std::env::var("WINDIR").unwrap_or("C:\\Windows".into())).join("Fonts");
    let mut out = Vec::new();
    for key in ["HKLM", "HKCU"] {
        let key = format!(r"{key}\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Fonts");
        let Ok(o) = std::process::Command::new("reg")
            .args(["query", &key])
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .output()
        else {
            continue;
        };
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            let Some((name, file)) = line.trim().split_once("    REG_SZ    ") else { continue };
            if is_font_file(file) {
                let name = name.trim_end_matches(" (TrueType)").trim_end_matches(" (OpenType)");
                out.push((name.to_string(), fonts_dir.join(file.trim()).to_string_lossy().into_owned()));
            }
        }
    }
    out.sort();
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

/// The editor always draws with the "user" family: the chosen font first, built-ins as fallback.
/// Each bundled font also gets its own family so the picker can show names in their own face.
/// Returns false if the font file couldn't be used (built-in mono is used instead).
fn apply_font(ctx: &egui::Context, path: &str) -> bool {
    let mut defs = egui::FontDefinitions::default();
    let mut family = defs.families[&FontFamily::Monospace].clone();
    for (_, name, bytes) in BUNDLED {
        defs.font_data.insert(name.to_string(), Arc::new(egui::FontData::from_static(bytes)));
        defs.families.insert(FontFamily::Name((*name).into()), [vec![name.to_string()], family.clone()].concat());
    }
    if let Some(name) = path.strip_prefix("bundled:") {
        family.insert(0, name.into());
    } else if let Ok(bytes) = std::fs::read(path)
        // egui panics on fonts it cannot parse, so check with the same parser first.
        && skrifa::FontRef::from_index(&bytes, 0).is_ok()
    {
        defs.font_data.insert("user".into(), Arc::new(egui::FontData::from_owned(bytes)));
        family.insert(0, "user".into());
    }
    let ok = path.is_empty() || family.len() > defs.families[&FontFamily::Monospace].len();
    defs.families.insert(FontFamily::Name("user".into()), family);
    ctx.set_fonts(defs);
    ok
}

fn human_size(n: usize) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut f = n as f64;
    let mut u = 0;
    while f >= 1024.0 && u < units.len() - 1 {
        f /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{n} B") } else { format!("{f:.2} {}", units[u]) }
}

/// A menu entry with its shortcut shown on the right.
fn item(ui: &mut egui::Ui, label: &str, keys: &str) -> bool {
    ui.add(egui::Button::new(label).shortcut_text(keys)).clicked()
}

/// Splits `r` into `n` panes: side by side for 2–3, a 2×2 grid for 4.
fn pane_rects(r: Rect, n: usize) -> Vec<Rect> {
    if n == 4 {
        let c = r.center();
        return vec![
            Rect::from_min_max(r.min, c),
            Rect::from_min_max(pos2(c.x, r.top()), pos2(r.right(), c.y)),
            Rect::from_min_max(pos2(r.left(), c.y), pos2(c.x, r.bottom())),
            Rect::from_min_max(c, r.max),
        ];
    }
    let w = r.width() / n as f32;
    (0..n).map(|i| Rect::from_min_size(pos2(r.left() + w * i as f32, r.top()), vec2(w, r.height()))).collect()
}

#[derive(PartialEq, Clone, Copy)]
enum Scope {
    File,
    Tabs,
    Folder,
}

struct App {
    tabs: Vec<Editor>,
    next_id: u64,
    /// Which tab each pane shows (1–4 panes); `active` is the pane that gets the keyboard.
    panes: Vec<usize>,
    active: usize,
    folder: Option<PathBuf>,
    tree_open: bool,
    dir_cache: HashMap<PathBuf, Vec<(String, PathBuf, bool)>>,
    query: Query,
    highlight: Option<(Query, regex::bytes::Regex)>,
    find_open: bool,
    replace_open: bool,
    replace: String,
    focus_find: bool,
    scope: Scope,
    goto: Option<String>,
    search: Option<Search>,
    results_open: bool,
    settings_open: bool,
    fonts: Vec<(&'static str, String, String)>,
    font_filter: String,
    s: Settings,
    saved_cfg: String,
    visuals_for: (Color32, Color32),
    status: String,
    title: String,
    logo: egui::TextureHandle,
}

impl App {
    fn new(cc: &eframe::CreationContext) -> App {
        let s = Settings::load();
        apply_font(&cc.egui_ctx, &s.font_path);
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        let logo = cc.egui_ctx.load_texture(
            "logo",
            egui::ColorImage::from_rgba_unmultiplied([64, 64], &logo::rgba(64, [255, 255, 255])),
            egui::TextureOptions::LINEAR,
        );
        let mut app = App {
            tabs: vec![Editor::new(0)],
            next_id: 1,
            panes: vec![0],
            active: 0,
            folder: None,
            tree_open: false,
            dir_cache: HashMap::new(),
            query: Query::default(),
            highlight: None,
            find_open: false,
            replace_open: false,
            replace: String::new(),
            focus_find: false,
            scope: Scope::File,
            goto: None,
            search: None,
            results_open: false,
            settings_open: false,
            fonts: Vec::new(),
            font_filter: String::new(),
            saved_cfg: s.serialize(),
            s,
            visuals_for: (Color32::TRANSPARENT, Color32::TRANSPARENT),
            status: String::new(),
            title: String::new(),
            logo,
        };
        for p in std::env::args_os().skip(1) {
            let p = PathBuf::from(p);
            if p.is_dir() { app.set_folder(p) } else { app.open_path(p) }
        }
        app
    }

    // ---------- tabs and panes ----------

    fn cur(&self) -> usize {
        self.panes[self.active]
    }

    fn ed(&mut self) -> &mut Editor {
        let i = self.cur();
        &mut self.tabs[i]
    }

    fn show_tab(&mut self, i: usize) {
        self.panes[self.active] = i;
    }

    fn new_tab(&mut self) {
        self.tabs.push(Editor::new(self.next_id));
        self.next_id += 1;
        self.show_tab(self.tabs.len() - 1);
    }

    /// Shows the file if it's already open; otherwise opens it, reusing an untouched empty tab.
    fn open_path(&mut self, p: PathBuf) {
        let same = |e: &Editor| e.path.as_ref().or(e.loading.as_ref().map(|l| &l.path)) == Some(&p);
        if let Some(i) = self.tabs.iter().position(same) {
            self.show_tab(i);
            return;
        }
        let id = self.next_id;
        self.next_id += 1;
        let cur = self.cur();
        if self.tabs[cur].is_pristine() {
            self.tabs[cur] = Editor::open(id, p);
        } else {
            self.tabs.push(Editor::open(id, p));
            self.show_tab(self.tabs.len() - 1);
        }
    }

    fn close_tab(&mut self, i: usize) {
        self.stop_search();
        if !self.tabs[i].confirm_discard() {
            return;
        }
        self.tabs.remove(i);
        if self.tabs.is_empty() {
            self.tabs.push(Editor::new(self.next_id));
            self.next_id += 1;
        }
        let last = self.tabs.len() - 1;
        for p in &mut self.panes {
            if *p > i || *p > last {
                *p -= 1;
            }
        }
    }

    fn set_layout(&mut self, n: usize) {
        while self.panes.len() < n {
            // Fill new panes with tabs not on screen yet, if there are any.
            let next = (0..self.tabs.len()).find(|t| !self.panes.contains(t)).unwrap_or(self.cur());
            self.panes.push(next);
        }
        self.panes.truncate(n);
        self.active = self.active.min(n - 1);
    }

    fn cycle_tab(&mut self, back: bool) {
        let n = self.tabs.len();
        self.show_tab((self.cur() + if back { n - 1 } else { 1 }) % n);
    }

    // ---------- files ----------

    fn open_dialog(&mut self) {
        let mut d = rfd::FileDialog::new();
        if let Some(dir) = self.tabs[self.cur()].path.as_ref().and_then(|p| p.parent()).or(self.folder.as_deref()) {
            d = d.set_directory(dir);
        }
        for p in d.pick_files().unwrap_or_default() {
            self.open_path(p);
        }
    }

    fn set_folder(&mut self, p: PathBuf) {
        self.folder = Some(p);
        self.tree_open = true;
        self.dir_cache.clear();
    }

    fn open_folder(&mut self) {
        if let Some(p) = rfd::FileDialog::new().pick_folder() {
            self.set_folder(p);
        }
    }

    fn save(&mut self, save_as: bool) {
        self.stop_search(); // a running search may still map this file, which blocks replacing it
        self.ed().save(save_as);
    }

    fn save_all(&mut self) {
        self.stop_search();
        for e in self.tabs.iter_mut().filter(|e| e.modified && e.path.is_some()) {
            e.save(false);
        }
    }

    fn add_recent(&mut self, p: &Path) {
        let p = p.to_string_lossy().into_owned();
        if p.ends_with(".te-tmp") {
            return;
        }
        self.s.recent.retain(|r| *r != p);
        self.s.recent.insert(0, p);
        self.s.recent.truncate(RECENT_MAX);
    }

    // ---------- search ----------

    fn stop_search(&mut self) {
        if let Some(s) = &mut self.search {
            s.stop();
        }
    }

    fn open_find(&mut self, scope: Scope, replace: bool) {
        if let Some(t) = self.tabs[self.cur()].selected_text(500).filter(|t| !t.contains('\n')) {
            self.query.text = t;
        }
        self.find_open = true;
        self.replace_open = replace;
        self.scope = scope;
        self.focus_find = true;
        self.goto = None;
    }

    fn regex(&mut self) -> Option<regex::bytes::Regex> {
        match self.query.build() {
            Ok(re) => Some(re),
            Err(e) => {
                self.status = e;
                None
            }
        }
    }

    fn find_next(&mut self, forward: bool) {
        if let Some(re) = self.regex() {
            self.ed().find_next(&re, forward);
        }
    }

    fn run_search(&mut self) {
        let Some(re) = self.regex() else { return };
        self.stop_search();
        let snap = |e: &Editor| (e.id, e.name(), e.doc.clone());
        let ready = |e: &&Editor| e.loading.is_none();
        let (label, tabs, folder, skip) = match self.scope {
            Scope::File => ("this file".to_string(), vec![snap(&self.tabs[self.cur()])], None, vec![]),
            Scope::Tabs => ("open tabs".to_string(), self.tabs.iter().filter(ready).map(snap).collect(), None, vec![]),
            Scope::Folder => {
                let Some(root) = self.folder.clone() else {
                    self.status = "Open a folder first (File > Open folder…)".into();
                    return;
                };
                let inside: Vec<&Editor> = self.tabs.iter().filter(ready).filter(|e| e.path.as_ref().is_some_and(|p| p.starts_with(&root))).collect();
                let skip = inside.iter().filter_map(|e| e.path.clone()).collect();
                let name = root.file_name().map_or(root.to_string_lossy(), |n| n.to_string_lossy()).into_owned();
                (name, inside.into_iter().map(snap).collect(), Some(root), skip)
            }
        };
        self.search = Some(search::start(format!("\"{}\" in {label}", self.query.text), re, tabs, folder, skip));
        self.results_open = true;
    }

    fn jump(&mut self, target: Target, r: Range<usize>) {
        let i = match target {
            Target::Tab(id) => self.tabs.iter().position(|e| e.id == id),
            Target::File(p) => {
                self.open_path(p.clone());
                self.tabs.iter().position(|e| e.path.as_ref().or(e.loading.as_ref().map(|l| &l.path)) == Some(&p))
            }
        };
        match i {
            Some(i) => {
                self.show_tab(i);
                self.tabs[i].jump(r);
            }
            None => self.status = "That tab was closed".into(),
        }
    }

    // ---------- input ----------

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let (cmd, none, shift) = (Modifiers::COMMAND, Modifiers::NONE, Modifiers::SHIFT);
        let cs = Modifiers::COMMAND | Modifiers::SHIFT;
        let k = |m, key| ctx.input_mut(|i| i.consume_key(m, key));
        if k(cs, Key::S) {
            self.save(true);
        } else if k(cmd, Key::S) {
            self.save(false);
        }
        if k(cs, Key::O) {
            self.open_folder();
        } else if k(cmd, Key::O) {
            self.open_dialog();
        }
        if k(cmd, Key::N) || k(cmd, Key::T) {
            self.new_tab();
        }
        if k(cmd, Key::W) {
            self.close_tab(self.cur());
        }
        if k(cs, Key::Tab) {
            self.cycle_tab(true);
        } else if k(cmd, Key::Tab) {
            self.cycle_tab(false);
        }
        if k(cs, Key::F) {
            self.open_find(if self.folder.is_some() { Scope::Folder } else { Scope::Tabs }, false);
        } else if k(cmd, Key::F) {
            self.open_find(Scope::File, false);
        }
        if k(cmd, Key::H) {
            self.open_find(Scope::File, true);
        }
        if k(cmd, Key::G) {
            self.goto = Some(String::new());
            self.focus_find = true;
        }
        if k(shift, Key::F3) {
            self.find_next(false);
        } else if k(none, Key::F3) {
            self.find_next(true);
        }
        if k(cmd, Key::B) {
            self.tree_open = !self.tree_open;
        }
        if k(cmd, Key::Backslash) {
            self.set_layout(if self.panes.len() == 1 { 2 } else { 1 });
        }
        if k(cmd, Key::Equals) || k(cmd, Key::Plus) {
            self.s.size = (self.s.size + 1.0).min(96.0);
        }
        if k(cmd, Key::Minus) {
            self.s.size = (self.s.size - 1.0).max(6.0);
        }
        if k(cmd, Key::Num0) {
            self.s.size = 15.0;
        }
        if k(cmd, Key::Comma) {
            self.settings_open = !self.settings_open;
        }
        if k(none, Key::Escape) {
            if self.goto.take().is_none() && !std::mem::take(&mut self.find_open) {
                self.settings_open = false;
            }
        }
        let zoom = ctx.input(|i| i.zoom_delta());
        if zoom != 1.0 {
            self.s.size = (self.s.size * zoom).clamp(6.0, 96.0);
        }
    }

    // ---------- panels ----------

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        egui::MenuBar::new().ui(ui, |ui| {
            ui.add(egui::Image::new(&self.logo).fit_to_exact_size(vec2(18.0, 18.0)).tint(self.s.fg));
            ui.add_space(4.0);
            ui.menu_button("File", |ui| {
                if item(ui, "New tab", "Ctrl+N") {
                    self.new_tab();
                }
                if item(ui, "Open…", "Ctrl+O") {
                    self.open_dialog();
                }
                if item(ui, "Open folder…", "Ctrl+Shift+O") {
                    self.open_folder();
                }
                ui.menu_button("Open recent", |ui| {
                    if self.s.recent.is_empty() {
                        ui.weak("Nothing yet");
                    }
                    for r in self.s.recent.clone() {
                        if ui.button(&r).clicked() {
                            self.open_path(r.into());
                        }
                    }
                });
                if item(ui, "Reload from disk", "") {
                    self.stop_search();
                    if self.ed().confirm_discard() {
                        self.ed().reload();
                    }
                }
                ui.separator();
                if item(ui, "Save", "Ctrl+S") {
                    self.save(false);
                }
                if item(ui, "Save as…", "Ctrl+Shift+S") {
                    self.save(true);
                }
                if item(ui, "Save a copy as…", "") {
                    self.ed().save_copy();
                }
                if item(ui, "Save all", "") {
                    self.save_all();
                }
                ui.separator();
                if item(ui, "Close tab", "Ctrl+W") {
                    self.close_tab(self.cur());
                }
                if ui.add_enabled(self.folder.is_some(), egui::Button::new("Close folder")).clicked() {
                    self.folder = None;
                    self.tree_open = false;
                }
                ui.separator();
                if item(ui, "Exit", "Alt+F4") {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("Edit", |ui| {
                if item(ui, "Undo", "Ctrl+Z") {
                    self.ed().undo_redo(false);
                }
                if item(ui, "Redo", "Ctrl+Y") {
                    self.ed().undo_redo(true);
                }
                ui.separator();
                if item(ui, "Cut", "Ctrl+X") {
                    self.ed().copy(&ctx, true);
                }
                if item(ui, "Copy", "Ctrl+C") {
                    self.ed().copy(&ctx, false);
                }
                if item(ui, "Paste", "Ctrl+V") {
                    ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
                }
                if item(ui, "Delete", "Del")
                    && let Some(r) = self.tabs[self.cur()].selection()
                {
                    self.ed().delete(r);
                }
                if item(ui, "Select all", "Ctrl+A") {
                    self.ed().select_all();
                }
                ui.separator();
                if item(ui, "Duplicate line", "Ctrl+D") {
                    self.ed().duplicate_line();
                }
                if item(ui, "Delete line", "Ctrl+Shift+K") {
                    self.ed().delete_line();
                }
                ui.separator();
                ui.menu_button("Change case", |ui| self.tool_items(ui, tools::CASE));
                ui.menu_button("Lines", |ui| self.tool_items(ui, tools::LINES));
                ui.menu_button("Line endings", |ui| {
                    let crlf = self.tabs[self.cur()].doc.crlf;
                    if ui.radio(crlf, "Windows (CRLF)").clicked() {
                        self.ed().set_line_endings(true);
                    }
                    if ui.radio(!crlf, "Unix (LF)").clicked() {
                        self.ed().set_line_endings(false);
                    }
                });
            });
            ui.menu_button("Search", |ui| {
                if item(ui, "Find…", "Ctrl+F") {
                    self.open_find(Scope::File, false);
                }
                if item(ui, "Find next", "F3") {
                    self.find_next(true);
                }
                if item(ui, "Find previous", "Shift+F3") {
                    self.find_next(false);
                }
                if item(ui, "Replace…", "Ctrl+H") {
                    self.open_find(Scope::File, true);
                }
                if item(ui, "Search open tabs…", "") {
                    self.open_find(Scope::Tabs, false);
                }
                if item(ui, "Search folder…", "Ctrl+Shift+F") {
                    if self.folder.is_none() {
                        self.open_folder();
                    }
                    self.open_find(Scope::Folder, false);
                }
                if item(ui, "Go to line…", "Ctrl+G") {
                    self.goto = Some(String::new());
                    self.focus_find = true;
                }
                ui.separator();
                ui.checkbox(&mut self.query.case, "Match case");
                ui.checkbox(&mut self.query.word, "Whole word");
                ui.checkbox(&mut self.query.regex, "Regular expression");
            });
            ui.menu_button("View", |ui| {
                if item(ui, "Appearance…", "Ctrl+,") {
                    self.settings_open = true;
                }
                ui.menu_button("Theme", |ui| {
                    for (name, bg, fg) in THEMES {
                        if ui.button(*name).clicked() {
                            self.s.bg = Color32::from_hex(bg).unwrap();
                            self.s.fg = Color32::from_hex(fg).unwrap();
                        }
                    }
                });
                ui.separator();
                for (name, n) in LAYOUTS {
                    if ui.radio(self.panes.len() == *n, *name).clicked() {
                        self.set_layout(*n);
                    }
                }
                ui.separator();
                ui.add(egui::Checkbox::new(&mut self.tree_open, "Folder panel     Ctrl+B"));
                ui.checkbox(&mut self.results_open, "Search results");
                ui.checkbox(&mut self.s.gutter, "Line numbers");
                ui.separator();
                if item(ui, "Zoom in", "Ctrl+=") {
                    self.s.size = (self.s.size + 1.0).min(96.0);
                }
                if item(ui, "Zoom out", "Ctrl+-") {
                    self.s.size = (self.s.size - 1.0).max(6.0);
                }
                if item(ui, "Reset zoom", "Ctrl+0") {
                    self.s.size = 15.0;
                }
            });
            ui.menu_button("Encode", |ui| {
                ui.weak("Works on the selection, or the whole file");
                ui.menu_button("Encode", |ui| self.tool_items(ui, tools::ENCODE));
                ui.menu_button("Decode", |ui| self.tool_items(ui, tools::DECODE));
                ui.menu_button("Hash (copies to clipboard)", |ui| {
                    for (name, f) in tools::HASH {
                        if ui.button(*name).clicked() {
                            match self.tabs[self.cur()].input() {
                                Ok(b) => {
                                    let h = f(&b);
                                    ctx.copy_text(h.clone());
                                    self.status = format!("{name} copied: {h}");
                                }
                                Err(e) => self.status = format!("{name}: {e}"),
                            }
                        }
                    }
                });
            });
            ui.menu_button("Format", |ui| {
                ui.weak("Works on the selection, or the whole file");
                for (lang, items) in tools::FORMAT {
                    ui.menu_button(*lang, |ui| self.tool_items(ui, items));
                }
            });
        });
    }

    fn tool_items(&mut self, ui: &mut egui::Ui, items: &[(&str, tools::Tool)]) {
        for (name, f) in items {
            if ui.button(*name).clicked() {
                self.ed().transform(name, f);
            }
        }
    }

    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let (fg, cur) = (self.s.fg, self.cur());
        let (mut show, mut close, mut add) = (None, None, false);
        egui::ScrollArea::horizontal().scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (i, t) in self.tabs.iter().enumerate() {
                    let on_screen = self.panes.contains(&i);
                    let label = format!("{}{}", t.name(), if t.modified { " •" } else { "" });
                    let color = if i == cur { fg } else if on_screen { fg.gamma_multiply(0.75) } else { fg.gamma_multiply(0.5) };
                    let r = ui.selectable_label(i == cur, RichText::new(label).color(color).size(13.0));
                    let r = r.on_hover_text(t.path.as_ref().map_or("not saved yet".into(), |p| p.display().to_string()));
                    if r.clicked() {
                        show = Some(i);
                    }
                    if r.middle_clicked() {
                        close = Some(i);
                    }
                    if ui.add(egui::Button::new(RichText::new("×").size(13.0).color(fg.gamma_multiply(0.4))).frame(false)).clicked() {
                        close = Some(i);
                    }
                    ui.add_space(6.0);
                }
                if ui.add(egui::Button::new(RichText::new("+").size(14.0).color(fg.gamma_multiply(0.5))).frame(false)).on_hover_text("New tab (Ctrl+N)").clicked() {
                    add = true;
                }
            });
        });
        if let Some(i) = show {
            self.show_tab(i);
        }
        if let Some(i) = close {
            self.close_tab(i);
        }
        if add {
            self.new_tab();
        }
    }

    fn find_bar(&mut self, ui: &mut egui::Ui) {
        let focus = std::mem::take(&mut self.focus_find);
        let (mut next, mut search, mut rep_one, mut rep_all) = (None, false, false, false);
        ui.horizontal(|ui| {
            let r = ui.add(TextEdit::singleline(&mut self.query.text).hint_text("Find").desired_width(240.0));
            if focus && self.goto.is_none() {
                r.request_focus();
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                if self.scope == Scope::File {
                    next = Some(!ui.input(|i| i.modifiers.shift));
                } else {
                    search = true;
                }
                r.request_focus();
            }
            ui.toggle_value(&mut self.query.case, "Aa").on_hover_text("Match case");
            ui.toggle_value(&mut self.query.word, "W").on_hover_text("Whole word");
            ui.toggle_value(&mut self.query.regex, ".*").on_hover_text("Regular expression");
            if ui.button("Prev").on_hover_text("Previous match (Shift+F3)").clicked() {
                next = Some(false);
            }
            if ui.button("Next").on_hover_text("Next match (F3)").clicked() {
                next = Some(true);
            }
            ui.separator();
            ui.label("In");
            ui.selectable_value(&mut self.scope, Scope::File, "This file");
            ui.selectable_value(&mut self.scope, Scope::Tabs, "Open tabs");
            ui.selectable_value(&mut self.scope, Scope::Folder, "Folder").on_hover_text(self.folder.as_ref().map_or("No folder open".into(), |p| p.display().to_string()));
            if ui.button("Search all").on_hover_text("List every match in the results panel").clicked() {
                search = true;
            }
            if let Err(e) = self.query.build()
                && !self.query.text.is_empty()
            {
                ui.colored_label(Color32::from_rgb(0xff, 0x6b, 0x6b), e);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(egui::Button::new("×").frame(false)).on_hover_text("Close (Esc)").clicked() {
                    self.find_open = false;
                }
                ui.toggle_value(&mut self.replace_open, "Replace");
            });
        });
        if self.replace_open {
            ui.horizontal(|ui| {
                ui.add(TextEdit::singleline(&mut self.replace).hint_text(if self.query.regex { "Replace with ($1 = group 1)" } else { "Replace with" }).desired_width(240.0));
                rep_one = ui.button("Replace").clicked();
                rep_all = ui.button("Replace all in file").clicked();
            });
        }
        if let Some(fwd) = next {
            self.find_next(fwd);
        }
        if search {
            self.run_search();
        }
        if (rep_one || rep_all)
            && let Some(re) = self.regex()
        {
            let (rep, expand) = (self.replace.clone(), self.query.regex);
            if rep_all { self.ed().replace_all(&re, &rep, expand) } else { self.ed().replace_one(&re, &rep, expand) }
        }
    }

    fn results(&mut self, ui: &mut egui::Ui) {
        let Some(s) = &self.search else {
            ui.weak("No search yet. Ctrl+Shift+F searches open tabs or the open folder.");
            return;
        };
        let fg = self.s.fg;
        let mut clicked = None;
        let mut close = false;
        {
            let res = s.results.lock().unwrap();
            let hits = res.iter().map(|f| f.hits.len()).sum::<usize>();
            ui.horizontal(|ui| {
                let mut head = format!("{}: {hits} matches in {} files", s.label, res.len());
                if s.running() {
                    head += &format!("  ·  searching… {} files", s.files_done.load(Ordering::Relaxed));
                }
                if s.truncated() {
                    head += &format!("  ·  stopped at {} matches", search::MAX_HITS);
                }
                ui.label(RichText::new(head).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    close = ui.add(egui::Button::new("×").frame(false)).on_hover_text("Close results").clicked();
                });
            });
            // One flat list of rows (file headers and hits) so only the visible ones are drawn.
            let rows: Vec<(usize, Option<usize>)> = res.iter().enumerate().flat_map(|(f, fh)| std::iter::once((f, None)).chain((0..fh.hits.len()).map(move |h| (f, Some(h))))).collect();
            let row_h = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
            egui::ScrollArea::vertical().auto_shrink(false).show_rows(ui, row_h, rows.len(), |ui, range| {
                for &(f, h) in &rows[range] {
                    let fh = &res[f];
                    match h {
                        None => {
                            let more = if fh.hits.len() >= search::FILE_HITS { ", showing the first" } else { "" };
                            ui.label(RichText::new(format!("{}   ({}{more})", fh.name, fh.hits.len())).strong().color(fg.gamma_multiply(0.85)));
                        }
                        Some(h) => {
                            let hit = &fh.hits[h];
                            let mut job = egui::text::LayoutJob::default();
                            let mono = |c: Color32| egui::TextFormat { font_id: FontId::monospace(12.5), color: c, ..Default::default() };
                            job.append(&format!("{:>8}  ", hit.line + 1), 0.0, mono(fg.gamma_multiply(0.4)));
                            job.append(&hit.preview[0], 0.0, mono(fg.gamma_multiply(0.75)));
                            job.append(&hit.preview[1], 0.0, egui::TextFormat { background: fg.gamma_multiply(0.25), ..mono(fg) });
                            job.append(&hit.preview[2], 0.0, mono(fg.gamma_multiply(0.75)));
                            if ui.add(egui::Label::new(job).truncate().sense(Sense::click())).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                                clicked = Some((fh.target.clone(), hit.range.clone()));
                            }
                        }
                    }
                }
            });
        }
        if s.running() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
        if close {
            self.results_open = false;
        }
        if let Some((t, r)) = clicked {
            self.jump(t, r);
        }
    }

    fn tree(&mut self, ui: &mut egui::Ui) {
        let Some(root) = self.folder.clone() else {
            ui.weak("No folder open");
            if ui.button("Open folder…").clicked() {
                self.open_folder();
            }
            return;
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(root.file_name().map_or(root.to_string_lossy(), |n| n.to_string_lossy())).strong()).on_hover_text(root.display().to_string());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Refresh").on_hover_text("Refresh").clicked() {
                    self.dir_cache.clear();
                }
            });
        });
        ui.separator();
        let mut open = None;
        egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| self.dir_ui(ui, &root, &mut open));
        if let Some(p) = open {
            self.open_path(p);
        }
    }

    fn dir_ui(&mut self, ui: &mut egui::Ui, dir: &Path, open: &mut Option<PathBuf>) {
        let entries = self
            .dir_cache
            .entry(dir.to_path_buf())
            .or_insert_with(|| {
                let mut v: Vec<(String, PathBuf, bool)> = std::fs::read_dir(dir)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path(), e.file_type().is_ok_and(|t| t.is_dir())))
                    .collect();
                v.sort_by_key(|(n, _, d)| (!d, n.to_lowercase()));
                v
            })
            .clone();
        let current = self.tabs[self.cur()].path.clone();
        for (name, path, is_dir) in entries {
            if is_dir {
                egui::CollapsingHeader::new(&name).id_salt(&path).show(ui, |ui| self.dir_ui(ui, &path, open));
            } else if ui.selectable_label(current.as_ref() == Some(&path), &name).clicked() {
                *open = Some(path);
            }
        }
    }

    fn goto_window(&mut self, ctx: &egui::Context) {
        let focus = self.focus_find;
        let mut go = None;
        if let Some(q) = &mut self.goto {
            egui::Window::new("goto").title_bar(false).resizable(false).anchor(Align2::CENTER_TOP, [0.0, 60.0]).show(ctx, |ui| {
                let r = ui.add(TextEdit::singleline(q).hint_text("Go to line").desired_width(180.0));
                if focus {
                    r.request_focus();
                }
                if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    go = Some(q.trim().parse::<usize>().ok());
                }
            });
        }
        if let Some(n) = go {
            if let Some(n) = n {
                self.ed().goto_line(n);
            }
            self.goto = None;
        }
    }

    fn panes(&mut self, ui: &mut egui::Ui) {
        let fg = self.s.fg;
        let hl = if self.find_open && !self.query.text.is_empty() {
            if self.highlight.as_ref().is_none_or(|(q, _)| q.text != self.query.text || q.case != self.query.case || q.word != self.query.word || q.regex != self.query.regex) {
                self.highlight = self.query.build().ok().map(|re| (self.query.clone(), re));
            }
            self.highlight.as_ref().map(|(_, re)| re.clone())
        } else {
            None
        };
        let view = View { font: FontId::new(self.s.size, FontFamily::Name("user".into())), fg, gutter: self.s.gutter, spacing: self.s.spacing, highlight: hl.as_ref() };
        let n = self.panes.len();
        let rects = pane_rects(ui.max_rect(), n);
        for (p, r) in rects.into_iter().enumerate() {
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r.shrink(if n > 1 { 1.0 } else { 0.0 })).id_salt(("pane", p)));
            let tab = self.panes[p];
            if n > 1 {
                let (head, _) = child.allocate_exact_size(vec2(child.available_width(), 22.0), Sense::hover());
                let active = p == self.active;
                let t = &self.tabs[tab];
                child.painter().text(head.left_center() + vec2(10.0, 0.0), Align2::LEFT_CENTER, format!("{}{}", t.name(), if t.modified { " •" } else { "" }), FontId::proportional(12.0), if active { fg } else { fg.gamma_multiply(0.5) });
                if active {
                    child.painter().rect_filled(Rect::from_min_max(pos2(head.left(), head.bottom() - 2.0), head.right_bottom()), 0.0, fg.gamma_multiply(0.6));
                }
            }
            let resp = self.tabs[tab].draw(&mut child, &view, p == self.active);
            // `clicked` too: a fast click can press and release within one frame.
            if (resp.is_pointer_button_down_on() || resp.clicked()) && self.active != p {
                self.active = p;
            }
            if n > 1 {
                ui.painter().rect_stroke(r, 0.0, (1.0, fg.gamma_multiply(0.12)), egui::StrokeKind::Inside);
            }
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let fg = self.s.fg;
        ui.horizontal_centered(|ui| {
            let dim = |t: String| RichText::new(t).size(12.0).color(fg.gamma_multiply(0.55));
            let e = &self.tabs[self.cur()];
            let left = if self.status.is_empty() { e.path.as_ref().map_or(e.name(), |p| p.display().to_string()) } else { self.status.clone() };
            ui.add(egui::Label::new(dim(left)).truncate());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (line, col) = e.line_col();
                ui.label(dim(format!(
                    "Ln {line}, Col {col}   ·   {} lines   ·   {}   ·   {}   ·   {} tabs",
                    e.doc.line_count(),
                    human_size(e.doc.len()),
                    if e.doc.crlf { "CRLF" } else { "LF" },
                    self.tabs.len()
                )));
            });
        });
    }

    fn settings(&mut self, ctx: &egui::Context) {
        let mut open = self.settings_open;
        egui::Window::new("Appearance").open(&mut open).collapsible(false).resizable(false).anchor(Align2::RIGHT_TOP, [-16.0, 70.0]).show(ctx, |ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            ui.horizontal_wrapped(|ui| {
                for (name, bg, fg) in THEMES {
                    let (bg, fg) = (Color32::from_hex(bg).unwrap(), Color32::from_hex(fg).unwrap());
                    let b = egui::Button::new(RichText::new(*name).color(fg).size(12.0)).fill(bg).corner_radius(6);
                    if ui.add(b).clicked() {
                        self.s.bg = bg;
                        self.s.fg = fg;
                    }
                }
            });
            ui.separator();
            egui::Grid::new("colors").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                ui.label("Background");
                ui.color_edit_button_srgba(&mut self.s.bg);
                ui.end_row();
                ui.label("Text");
                ui.color_edit_button_srgba(&mut self.s.fg);
                ui.end_row();
                ui.label("Size");
                ui.add(egui::Slider::new(&mut self.s.size, 6.0..=96.0).step_by(0.5));
                ui.end_row();
                ui.label("Line spacing");
                ui.add(egui::Slider::new(&mut self.s.spacing, 1.0..=2.5).step_by(0.05));
                ui.end_row();
                ui.label("Line numbers");
                ui.checkbox(&mut self.s.gutter, "");
                ui.end_row();
            });
            ui.separator();
            ui.label(format!("Font — {}", self.s.font));
            if self.fonts.is_empty() {
                self.fonts = all_fonts();
            }
            ui.horizontal(|ui| {
                ui.add(TextEdit::singleline(&mut self.font_filter).hint_text("Search fonts…").desired_width(180.0));
                if ui.small_button("Fonts folder").on_hover_text("Drop .ttf / .otf files here, then Refresh").clicked() {
                    let dir = user_fonts_dir();
                    let _ = std::fs::create_dir_all(&dir);
                    let _ = std::process::Command::new("explorer").arg(dir).spawn();
                }
                if ui.small_button("Refresh").clicked() {
                    self.fonts.clear();
                }
            });
            let filter = self.font_filter.to_lowercase();
            let mut pick = None;
            egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                let mut group = "";
                for (g, name, path) in self.fonts.iter().filter(|f| f.1.to_lowercase().contains(&filter)) {
                    if *g != group {
                        group = g;
                        ui.add_space(4.0);
                        ui.label(RichText::new(*g).size(11.0).color(self.s.fg.gamma_multiply(0.45)));
                    }
                    let mut text = RichText::new(name);
                    if path.starts_with("bundled:") {
                        text = text.family(FontFamily::Name(name.as_str().into())).size(16.0);
                    }
                    if ui.selectable_label(*name == self.s.font, text).clicked() {
                        pick = Some((name.clone(), path.clone()));
                    }
                }
            });
            if let Some((name, path)) = pick {
                if !apply_font(ctx, &path) {
                    self.status = format!("Can't read font \"{name}\", using built-in");
                }
                self.s.font = name;
                self.s.font_path = path;
            }
        });
        self.settings_open = open;
    }

    fn sync_visuals(&mut self, ctx: &egui::Context) {
        let (bg, fg) = (self.s.bg, self.s.fg);
        if self.visuals_for == (bg, fg) {
            return;
        }
        self.visuals_for = (bg, fg);
        let dark = (0.299 * bg.r() as f32 + 0.587 * bg.g() as f32 + 0.114 * bg.b() as f32) < 128.0;
        let lift = |t: f32| {
            let to = if dark { 255.0 } else { 0.0 };
            let m = |c: u8| (c as f32 + (to - c as f32) * t) as u8;
            Color32::from_rgb(m(bg.r()), m(bg.g()), m(bg.b()))
        };
        let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
        v.panel_fill = bg;
        v.window_fill = lift(0.06);
        v.extreme_bg_color = lift(0.10);
        v.faint_bg_color = lift(0.04);
        v.override_text_color = Some(fg);
        v.window_stroke = egui::Stroke::new(1.0, lift(0.15));
        v.selection.bg_fill = fg.gamma_multiply(0.3);
        v.widgets.inactive.weak_bg_fill = lift(0.07);
        v.widgets.hovered.weak_bg_fill = lift(0.13);
        v.widgets.active.weak_bg_fill = lift(0.18);
        ctx.set_visuals(v);
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.sync_visuals(&ctx);
        for i in 0..self.tabs.len() {
            if let Some(p) = self.tabs[i].poll() {
                self.add_recent(&p);
            }
            if let Some(m) = self.tabs[i].msg.take() {
                self.status = m;
            }
        }

        if ctx.input(|i| i.viewport().close_requested()) {
            self.stop_search();
            for i in 0..self.tabs.len() {
                if self.tabs[i].modified {
                    self.show_tab(i);
                    if !self.tabs[i].confirm_discard() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                        break;
                    }
                }
            }
        }
        for p in ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect::<Vec<_>>()) {
            if p.is_dir() { self.set_folder(p) } else { self.open_path(p) }
        }
        self.shortcuts(&ctx);

        let title = { let e = &self.tabs[self.cur()]; format!("{}{} — te", if e.modified { "• " } else { "" }, e.name()) };
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }

        let bg = self.s.bg;
        let bar = |m: i8| Frame::NONE.fill(bg).inner_margin(egui::Margin::symmetric(m, 3));
        egui::Panel::top("menu").frame(bar(8)).show(ui, |ui| self.menu_bar(ui));
        egui::Panel::top("tabs").frame(bar(8)).show(ui, |ui| self.tab_bar(ui));
        if self.find_open {
            egui::Panel::top("find").frame(bar(10)).show(ui, |ui| self.find_bar(ui));
        }
        egui::Panel::bottom("status").exact_size(24.0).frame(bar(12)).show(ui, |ui| self.status_bar(ui));
        if self.results_open {
            egui::Panel::bottom("results").resizable(true).default_size(220.0).frame(bar(10)).show(ui, |ui| self.results(ui));
        }
        if self.tree_open {
            egui::Panel::left("tree").resizable(true).default_size(240.0).frame(bar(8)).show(ui, |ui| self.tree(ui));
        }
        egui::CentralPanel::no_frame().frame(Frame::NONE.fill(bg)).show(ui, |ui| self.panes(ui));
        self.goto_window(&ctx);
        if self.settings_open {
            self.settings(&ctx);
        }
        self.focus_find = false;

        let cfg = self.s.serialize();
        if cfg != self.saved_cfg {
            let _ = std::fs::write(Settings::path(), &cfg);
            self.saved_cfg = cfg;
        }
    }
}

fn main() -> eframe::Result {
    let icon = egui::IconData { rgba: logo::rgba(64, [0x9f, 0xef, 0x00]), width: 64, height: 64 };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("te")
            .with_inner_size(Vec2::new(1200.0, 760.0))
            .with_drag_and_drop(true)
            .with_icon(Arc::new(icon)),
        renderer: eframe::Renderer::Wgpu,
        // DX12 only, on the integrated GPU: a text editor shouldn't wake the discrete GPU or load every driver.
        wgpu_options: egui_wgpu::WgpuConfiguration {
            wgpu_setup: egui_wgpu::WgpuSetup::CreateNew(egui_wgpu::WgpuSetupCreateNew {
                instance_descriptor: egui_wgpu::wgpu::InstanceDescriptor {
                    backends: egui_wgpu::wgpu::Backends::DX12,
                    ..egui_wgpu::WgpuSetupCreateNew::without_display_handle().instance_descriptor
                },
                power_preference: egui_wgpu::wgpu::PowerPreference::LowPower,
                ..egui_wgpu::WgpuSetupCreateNew::without_display_handle()
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    eframe::run_native("te", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}
