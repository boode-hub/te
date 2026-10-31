//! One open document (a tab): its text, cursor, scroll position and undo history, plus drawing it.

use crate::doc::{self, Doc, Loading, Piece};
use eframe::egui::{self, Align2, Color32, CornerRadius, Event, FontId, Key, Modifiers, Rect, Sense, pos2, vec2};
use regex::bytes::{NoExpand, Regex};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

// ponytail: lines longer than this are drawn truncated; window the horizontal range if it matters.
pub const MAX_LINE: usize = 20_000;
const UNDO_LIMIT: usize = 1000;
/// Undo also stops growing once its snapshots hold this many pieces in total (~64 MB), so
/// thousands of scattered edits can't eat memory.
const UNDO_PIECES: usize = 2_000_000;
/// Find-next in documents bigger than this runs on a background thread (Esc cancels).
const FIND_IN_BACKGROUND: usize = 16 << 20;
/// Background finds work in slices this big, so a cancel takes effect within one slice.
const FIND_SLICE: usize = 64 << 20;
/// Whole-document tools (format, replace all, line ops) need the text in memory; cap it.
pub const TRANSFORM_LIMIT: usize = 256 << 20;

/// Every colour the user can map. `from_pair` derives a whole theme from background + text.
#[derive(Clone, Copy, PartialEq)]
pub struct Theme {
    pub bg: Color32,
    pub fg: Color32,
    pub cursor: Color32,
    pub selection: Color32,
    pub line: Color32,
    pub numbers: Color32,
    pub matches: Color32,
    pub bars: Color32,
}

impl Theme {
    pub fn from_pair(bg: Color32, fg: Color32) -> Theme {
        Theme {
            bg,
            fg,
            cursor: fg,
            selection: fg.gamma_multiply(0.22),
            line: fg.gamma_multiply(0.05),
            numbers: fg.gamma_multiply(0.3),
            matches: fg.gamma_multiply(0.45),
            bars: bg,
        }
    }

    /// (label, config key, colour) for each slot, in display order.
    pub fn slots(&mut self) -> [(&'static str, &'static str, &mut Color32); 8] {
        [
            ("Background", "bg", &mut self.bg),
            ("Text", "fg", &mut self.fg),
            ("Cursor", "cursor", &mut self.cursor),
            ("Selection", "selection", &mut self.selection),
            ("Current line", "line", &mut self.line),
            ("Line numbers", "numbers", &mut self.numbers),
            ("Search matches", "matches", &mut self.matches),
            ("Bars and panels", "bars", &mut self.bars),
        ]
    }
}

/// How to draw, shared by every pane.
pub struct View<'a> {
    pub font: FontId,
    pub theme: Theme,
    pub gutter: bool,
    pub spacing: f32,
    pub highlight: Option<&'a Regex>,
}

/// Where one pane is scrolled to. Each pane keeps its own per tab, so two panes on the same file
/// scroll independently.
#[derive(Clone, Copy, Default)]
pub struct PaneView {
    pub top: f64, // first visible line; fractional for smooth scrolling
    pub scroll_x: f32,
}

/// A background find: its thread (whose result is None if cancelled) and its cancel flag.
type Finding = (JoinHandle<Option<Option<Range<usize>>>>, Arc<AtomicBool>);

pub struct Editor {
    pub id: u64,
    pub doc: Doc,
    pub path: Option<PathBuf>,
    pub loading: Option<Loading>,
    /// Selection and scroll to apply once loading finishes.
    restore: Option<(Range<usize>, f64)>,
    pub modified: bool,
    pub cursor: usize,
    pub anchor: Option<usize>,
    goal_x: Option<f32>,
    /// Scroll position last seen in the active pane: used when saving/reloading and when the tab
    /// is first shown in another pane. Each pane scrolls on its own via `PaneView`.
    pub top: f64,
    pub scroll_x: f32,
    /// Scroll the active pane here on its next draw (Go to line, Page Up/Down, reopen).
    pending_top: Option<f64>,
    follow: bool,
    undo: Vec<(Vec<Piece>, usize)>,
    redo: Vec<(Vec<Piece>, usize)>,
    typing: bool,
    rows: usize,
    /// Message for the status bar, taken by the app each frame.
    pub msg: Option<String>,
    /// A find-next running in the background: its result (None = cancelled) and its cancel flag.
    finding: Option<Finding>,
}

/// Tabs aren't handled by egui's layout, so draw each as 4 spaces.
fn display(b: &[u8]) -> String {
    String::from_utf8_lossy(b).replace('\t', "    ")
}

/// Byte offset in `b` of display-char index `ci` (inverse of `display`).
fn byte_of_display(b: &[u8], ci: usize) -> usize {
    let s = String::from_utf8_lossy(b);
    let mut acc = 0;
    for (i, c) in s.char_indices() {
        acc += if c == '\t' { 4 } else { 1 };
        if acc > ci {
            return i.min(b.len());
        }
    }
    b.len()
}

/// One find-next: from `from` to the end (or start), then wrapped around. `None` if cancelled.
fn find_wrapping(doc: &Doc, re: &Regex, from: usize, forward: bool, cursor: usize, cancel: &AtomicBool) -> Option<Option<Range<usize>>> {
    let len = doc.len();
    let mut hit = find_sliced(doc, re, if forward { from..len } else { 0..from }, forward, cancel, FIND_SLICE)?;
    // An empty match (e.g. `^`) right at the cursor would be found again forever; step past it.
    if hit.as_ref().is_some_and(|r| r.is_empty() && r.start == cursor) {
        hit = find_sliced(doc, re, if forward { (from + 1).min(len)..len } else { 0..from.saturating_sub(1) }, forward, cancel, FIND_SLICE)?;
    }
    if hit.is_none() {
        hit = find_sliced(doc, re, if forward { 0..from } else { from..len }, forward, cancel, FIND_SLICE)?;
    }
    Some(hit)
}

/// `Doc::find_in` over `r` in FIND_SLICE pieces, checking `cancel` between them. Exact: a match
/// only has to start inside a slice, and the scan reads past the slice end to finish it.
fn find_sliced(doc: &Doc, re: &Regex, r: Range<usize>, forward: bool, cancel: &AtomicBool, slice: usize) -> Option<Option<Range<usize>>> {
    let mut slices: Vec<Range<usize>> = (r.start..r.end).step_by(slice).map(|a| a..(a + slice).min(r.end)).collect();
    if !forward {
        slices.reverse();
    }
    for s in slices {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        if let Some(m) = doc.find_in(re, s, forward) {
            return Some(Some(m));
        }
    }
    Some(None)
}

pub fn width(ctx: &egui::Context, font: &FontId, b: &[u8]) -> f32 {
    ctx.fonts_mut(|f| f.layout_no_wrap(display(b), font.clone(), Color32::WHITE).size().x)
}

impl Editor {
    pub fn new(id: u64) -> Editor {
        Editor {
            id,
            doc: Doc::new(),
            path: None,
            loading: None,
            restore: None,
            modified: false,
            cursor: 0,
            anchor: None,
            goal_x: None,
            top: 0.0,
            scroll_x: 0.0,
            pending_top: None,
            follow: false,
            undo: Vec::new(),
            redo: Vec::new(),
            typing: false,
            rows: 30,
            msg: None,
            finding: None,
        }
    }

    pub fn open(id: u64, path: PathBuf) -> Editor {
        let mut e = Editor::new(id);
        e.start_load(path, None);
        e
    }

    pub fn name(&self) -> String {
        let p = self.loading.as_ref().map(|l| &l.path).or(self.path.as_ref());
        p.and_then(|p| p.file_name()).map_or("untitled".into(), |n| n.to_string_lossy().into_owned())
    }

    /// An empty untitled tab nobody has touched; opening a file reuses it.
    pub fn is_pristine(&self) -> bool {
        self.path.is_none() && self.loading.is_none() && !self.modified && self.doc.len() == 0
    }

    fn start_load(&mut self, path: PathBuf, restore: Option<(Range<usize>, f64)>) {
        self.cancel_find(true);
        self.doc = Doc::new();
        self.undo.clear();
        self.redo.clear();
        self.modified = false;
        self.restore = restore;
        self.loading = Some(doc::open(path));
    }

    /// Finishes a background load. Returns the path when a file just opened successfully.
    pub fn poll(&mut self) -> Option<PathBuf> {
        if !self.loading.as_ref().is_some_and(|l| l.is_finished()) {
            return None;
        }
        let l = self.loading.take().unwrap();
        let path = l.path.clone();
        match l.join() {
            Ok(doc) => {
                self.doc = doc;
                self.path = Some(path.clone());
                let (sel, top) = self.restore.take().unwrap_or((0..0, 0.0));
                let len = self.doc.len();
                self.anchor = (sel.start != sel.end).then_some(sel.start.min(len));
                self.cursor = sel.end.min(len);
                self.top = top;
                self.pending_top = Some(top);
                self.follow = top == 0.0 && self.cursor > 0;
                Some(path)
            }
            Err(e) => {
                self.msg = Some(format!("Can't open {}: {e}", path.display()));
                None
            }
        }
    }

    pub fn reload(&mut self) {
        if let Some(p) = self.path.clone() {
            self.start_load(p, Some((self.cursor..self.cursor, self.top)));
        }
    }

    /// Asks before throwing away edits. Returns false if the user cancelled.
    pub fn confirm_discard(&mut self) -> bool {
        if !self.modified {
            return true;
        }
        use rfd::{MessageButtons, MessageDialogResult};
        match rfd::MessageDialog::new()
            .set_title("te")
            .set_description(format!("Save changes to {}?", self.name()))
            .set_buttons(MessageButtons::YesNoCancel)
            .show()
        {
            MessageDialogResult::Yes => self.save(false),
            MessageDialogResult::No => true,
            _ => false,
        }
    }

    fn save_dialog(&self) -> Option<PathBuf> {
        let mut d = rfd::FileDialog::new().set_file_name(self.name());
        if let Some(dir) = self.path.as_ref().and_then(|p| p.parent()) {
            d = d.set_directory(dir);
        }
        d.save_file()
    }

    /// Writes to a temp file, then swaps it in, so a failed save never damages the original.
    pub fn save(&mut self, save_as: bool) -> bool {
        if self.loading.is_some() {
            self.msg = Some("Still opening the file; save again in a moment".into());
            return false;
        }
        let path = match (&self.path, save_as) {
            (Some(p), false) => p.clone(),
            _ => match self.save_dialog() {
                Some(p) => p,
                None => return false,
            },
        };
        let mut tmp = path.clone().into_os_string();
        tmp.push(".te-tmp");
        let tmp = PathBuf::from(tmp);
        if let Err(e) = self.doc.write_to(&tmp) {
            let _ = std::fs::remove_file(&tmp);
            self.msg = Some(format!("Save failed: {e}"));
            return false;
        }
        // A background find holds a snapshot that may map this file; it must let go before the swap.
        self.cancel_find(true);
        let restore = Some((self.anchor.unwrap_or(self.cursor)..self.cursor, self.top));
        self.doc = Doc::new(); // release the mapping so Windows lets us replace the file
        match std::fs::rename(&tmp, &path) {
            Ok(()) => {
                self.start_load(path, restore);
                self.msg = Some("Saved".into());
            }
            Err(e) => {
                self.start_load(tmp.clone(), restore);
                self.msg = Some(format!("Couldn't replace file ({e}); your text is safe in {}", tmp.display()));
            }
        }
        true
    }

    /// Writes the text elsewhere and keeps editing this file.
    pub fn save_copy(&mut self) {
        if self.loading.is_some() {
            self.msg = Some("Still opening the file; try again in a moment".into());
            return;
        }
        if let Some(p) = self.save_dialog() {
            self.msg = Some(match self.doc.write_to(&p) {
                Ok(()) => format!("Copy saved to {}", p.display()),
                Err(e) => format!("Save failed: {e}"),
            });
        }
    }

    // ---------- editing ----------

    pub fn selection(&self) -> Option<Range<usize>> {
        self.anchor.filter(|&a| a != self.cursor).map(|a| a.min(self.cursor)..a.max(self.cursor))
    }

    fn begin_edit(&mut self, typing: bool) {
        self.cancel_find(false);
        if !(typing && self.typing) {
            self.undo.push((self.doc.snapshot(), self.cursor));
            let mut pieces: usize = self.undo.iter().map(|(p, _)| p.len()).sum();
            while self.undo.len() > UNDO_LIMIT || (pieces > UNDO_PIECES && self.undo.len() > 1) {
                pieces -= self.undo.remove(0).0.len();
            }
        }
        self.typing = typing;
        self.redo.clear();
        self.modified = true;
        self.goal_x = None;
        self.follow = true;
    }

    pub fn insert(&mut self, text: &[u8], typing: bool) {
        if self.loading.is_some() {
            return;
        }
        let sel = self.selection();
        self.begin_edit(typing && sel.is_none() && !text.contains(&b'\n'));
        if let Some(r) = sel {
            self.doc.delete(r.clone());
            self.cursor = r.start;
        }
        self.anchor = None;
        self.doc.insert(self.cursor, text);
        self.cursor += text.len();
    }

    pub fn delete(&mut self, r: Range<usize>) {
        if r.is_empty() || self.loading.is_some() {
            return;
        }
        self.begin_edit(false);
        self.doc.delete(r.clone());
        self.cursor = r.start;
        self.anchor = None;
    }

    pub fn undo_redo(&mut self, redo: bool) {
        self.cancel_find(false);
        let (from, to) = if redo { (&mut self.redo, &mut self.undo) } else { (&mut self.undo, &mut self.redo) };
        if let Some((pieces, cursor)) = from.pop() {
            to.push((self.doc.snapshot(), self.cursor));
            self.doc.restore(pieces);
            self.cursor = cursor.min(self.doc.len());
            self.anchor = None;
            self.typing = false;
            self.modified = true;
            self.follow = true;
        }
    }

    fn move_to(&mut self, pos: usize, shift: bool) {
        if shift {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = pos;
        self.typing = false;
        self.follow = true;
    }

    /// Selects `r` and scrolls to it (after loading, if the file is still opening).
    pub fn jump(&mut self, r: Range<usize>) {
        if self.loading.is_some() {
            self.restore = Some((r, 0.0));
            return;
        }
        let len = self.doc.len();
        self.anchor = Some(r.start.min(len));
        self.cursor = r.end.min(len);
        self.typing = false;
        self.follow = true;
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = self.doc.len();
    }

    pub fn selected_text(&self, max: usize) -> Option<String> {
        self.selection().filter(|r| r.len() <= max).map(|r| String::from_utf8_lossy(&self.doc.bytes(r)).into_owned())
    }

    pub fn copy(&mut self, ctx: &egui::Context, cut: bool) {
        if let Some(r) = self.selection() {
            if r.len() > TRANSFORM_LIMIT {
                self.msg = Some("Selection too large for the clipboard".into());
                return;
            }
            ctx.copy_text(String::from_utf8_lossy(&self.doc.bytes(r.clone())).into_owned());
            if cut {
                self.delete(r);
            }
        }
    }

    pub fn goto_line(&mut self, n: usize) {
        let n = n.clamp(1, self.doc.line_count()) - 1;
        self.move_to(self.doc.line_start(n), false);
        self.pending_top = Some((n as f64 - self.rows as f64 / 2.0).max(0.0));
    }

    pub fn duplicate_line(&mut self) {
        let line = self.doc.line_of(self.cursor);
        let s = self.doc.line_start(line);
        let e = if line + 1 < self.doc.line_count() { self.doc.line_start(line + 1) } else { self.doc.len() };
        let mut text = self.doc.bytes(s..e.min(s + TRANSFORM_LIMIT));
        if !text.ends_with(b"\n") {
            text.extend_from_slice(if self.doc.crlf { b"\r\n" } else { b"\n" });
        }
        let (c, a) = (self.cursor, self.anchor);
        self.anchor = None;
        self.cursor = s;
        self.insert(&text, false);
        self.cursor = c + text.len();
        self.anchor = a.map(|a| a + text.len());
    }

    pub fn delete_line(&mut self) {
        let line = self.doc.line_of(self.cursor);
        let r = if line + 1 < self.doc.line_count() {
            self.doc.line_start(line)..self.doc.line_start(line + 1)
        } else if line > 0 {
            self.doc.line_end(line - 1)..self.doc.len()
        } else {
            0..self.doc.len()
        };
        self.delete(r);
    }

    /// Runs `f` over the selection (or the whole document) and puts the result in its place,
    /// selected, as one undo step.
    pub fn transform(&mut self, name: &str, f: impl FnOnce(&[u8]) -> Result<Vec<u8>, String>) {
        let r = self.selection().unwrap_or(0..self.doc.len());
        self.transform_range(r, name, f);
    }

    pub fn transform_range(&mut self, r: Range<usize>, name: &str, f: impl FnOnce(&[u8]) -> Result<Vec<u8>, String>) {
        if self.loading.is_some() {
            return;
        }
        if r.len() > TRANSFORM_LIMIT {
            self.msg = Some(format!("{name}: that's over 256 MB; select a smaller part"));
            return;
        }
        let input = self.doc.bytes(r.clone());
        // A bug in a formatter library must not take the unsaved text down with it.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&input))).unwrap_or_else(|_| Err("crashed on this input (nothing was changed)".into()));
        match result {
            Ok(out) if out == input => self.msg = Some(format!("{name}: nothing to change")),
            Ok(out) => {
                self.begin_edit(false);
                self.doc.delete(r.clone());
                self.doc.insert(r.start, &out);
                self.anchor = Some(r.start);
                self.cursor = r.start + out.len();
                self.msg = Some(name.to_string());
            }
            Err(e) => self.msg = Some(format!("{name}: {e}")),
        }
    }

    /// The bytes a hash or similar read-only tool should look at.
    pub fn input(&self) -> Result<Vec<u8>, String> {
        let r = self.selection().unwrap_or(0..self.doc.len());
        if r.len() > TRANSFORM_LIMIT {
            return Err("that's over 256 MB; select a smaller part".into());
        }
        Ok(self.doc.bytes(r))
    }

    pub fn set_line_endings(&mut self, crlf: bool) {
        self.transform_range(0..self.doc.len(), if crlf { "Line endings: CRLF" } else { "Line endings: LF" }, |b| {
            let lf = regex::bytes::Regex::new("\r\n").unwrap().replace_all(b, NoExpand(b"\n")).into_owned();
            Ok(if crlf { regex::bytes::Regex::new("\n").unwrap().replace_all(&lf, NoExpand(b"\r\n")).into_owned() } else { lf })
        });
        self.doc.crlf = crlf;
    }

    // ---------- find / replace ----------

    /// Selects the next (or previous) match, wrapping around. Big documents are searched in the
    /// background; the result arrives through `poll_find`. Returns false only for a definite miss.
    pub fn find_next(&mut self, re: &Regex, forward: bool) -> bool {
        self.cancel_find(false);
        let sel = self.selection().unwrap_or(self.cursor..self.cursor);
        let from = if forward { sel.end } else { sel.start };
        if self.doc.len() < FIND_IN_BACKGROUND {
            let hit = find_wrapping(&self.doc, re, from, forward, self.cursor, &AtomicBool::new(false)).flatten();
            return self.found(hit);
        }
        let (doc, re, cursor) = (self.doc.clone(), re.clone(), self.cursor);
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let handle = std::thread::spawn(move || find_wrapping(&doc, &re, from, forward, cursor, &flag));
        self.finding = Some((handle, cancel));
        true
    }

    fn found(&mut self, hit: Option<Range<usize>>) -> bool {
        match hit {
            Some(r) => {
                self.jump(r);
                true
            }
            None => {
                self.msg = Some("No matches".into());
                false
            }
        }
    }

    pub fn is_finding(&self) -> bool {
        self.finding.is_some()
    }

    /// Stops a background find. `wait` also joins it, releasing its snapshot of the document.
    pub fn cancel_find(&mut self, wait: bool) {
        if let Some((handle, cancel)) = self.finding.take() {
            cancel.store(true, Ordering::Relaxed);
            if wait {
                let _ = handle.join();
            }
            self.msg = Some("Search cancelled".into());
        }
    }

    /// Applies a finished background find.
    pub fn poll_find(&mut self) {
        if !self.finding.as_ref().is_some_and(|(h, _)| h.is_finished()) {
            return;
        }
        let (handle, _) = self.finding.take().unwrap();
        match handle.join() {
            Ok(Some(hit)) => {
                self.found(hit);
            }
            Ok(None) => self.msg = Some("Search cancelled".into()),
            Err(_) => self.msg = Some("Search failed".into()),
        }
    }

    /// Replaces the selection if it is a match, then moves to the next match.
    pub fn replace_one(&mut self, re: &Regex, rep: &str, expand: bool) {
        if let Some(r) = self.selection() {
            let sel = self.doc.bytes(r.clone());
            if re.find(&sel).is_some_and(|m| m.start() == 0 && m.end() == sel.len()) {
                let new = if expand { re.replace(&sel, rep.as_bytes()).into_owned() } else { rep.as_bytes().to_vec() };
                self.insert(&new, false);
            }
        }
        self.find_next(re, true);
    }

    pub fn replace_all(&mut self, re: &Regex, rep: &str, expand: bool) {
        let mut count = 0;
        self.transform_range(0..self.doc.len(), "Replace all", |b| {
            count = re.find_iter(b).count();
            Ok(if expand { re.replace_all(b, rep.as_bytes()).into_owned() } else { re.replace_all(b, NoExpand(rep.as_bytes())).into_owned() })
        });
        if count > 0 {
            self.msg = Some(format!("Replaced {count}"));
        }
    }

    // ---------- layout helpers ----------

    fn line_bytes(&self, n: usize) -> (usize, Vec<u8>) {
        let s = self.doc.line_start(n);
        let e = self.doc.line_end(n).min(s + MAX_LINE);
        (s, self.doc.bytes(s..e))
    }

    fn x_of(&self, ctx: &egui::Context, font: &FontId, pos: usize) -> f32 {
        let (s, b) = self.line_bytes(self.doc.line_of(pos));
        width(ctx, font, &b[..(pos - s).min(b.len())])
    }

    fn pos_at_x(&self, ctx: &egui::Context, font: &FontId, line: usize, x: f32) -> usize {
        let (s, b) = self.line_bytes(line.min(self.doc.line_count() - 1));
        let g = ctx.fonts_mut(|f| f.layout_no_wrap(display(&b), font.clone(), Color32::WHITE));
        s + byte_of_display(&b, g.cursor_from_pos(vec2(x, 0.0)).index.0)
    }

    pub fn line_col(&self) -> (usize, usize) {
        let line = self.doc.line_of(self.cursor);
        let ls = self.doc.line_start(line);
        let col = self.doc.bytes(ls..self.cursor.min(ls + MAX_LINE)).iter().filter(|&&c| c & 0xC0 != 0x80).count();
        (line + 1, col + 1)
    }

    // ---------- input ----------

    fn keys(&mut self, ctx: &egui::Context, font: &FontId) {
        for ev in ctx.input(|i| i.events.clone()) {
            match ev {
                Event::Text(t) => self.insert(t.as_bytes(), true),
                Event::Paste(t) => self.insert(t.as_bytes(), false),
                Event::Copy => self.copy(ctx, false),
                Event::Cut => self.copy(ctx, true),
                Event::Key { key, pressed: true, modifiers: m, .. } => self.key(ctx, font, key, m),
                _ => {}
            }
        }
    }

    fn key(&mut self, ctx: &egui::Context, font: &FontId, key: Key, m: Modifiers) {
        let d = &self.doc;
        let line = d.line_of(self.cursor);
        let vertical = |rows: isize| (line as isize + rows).clamp(0, d.line_count() as isize - 1) as usize;
        let page = self.rows.saturating_sub(1).max(1) as isize;
        match key {
            Key::ArrowLeft => self.move_to(d.prev_char(self.cursor), m.shift),
            Key::ArrowRight => self.move_to(d.next_char(self.cursor), m.shift),
            Key::ArrowUp | Key::ArrowDown | Key::PageUp | Key::PageDown => {
                let delta = match key {
                    Key::ArrowUp => -1,
                    Key::ArrowDown => 1,
                    Key::PageUp => -page,
                    _ => page,
                };
                let target = vertical(delta);
                let x = self.goal_x.unwrap_or_else(|| self.x_of(ctx, font, self.cursor));
                let pos = self.pos_at_x(ctx, font, target, x);
                if matches!(key, Key::PageUp | Key::PageDown) {
                    self.pending_top = Some((self.top + delta as f64).max(0.0));
                }
                self.move_to(pos, m.shift);
                self.goal_x = Some(x);
                return;
            }
            Key::Home => self.move_to(if m.command { 0 } else { d.line_start(line) }, m.shift),
            Key::End => self.move_to(if m.command { d.len() } else { d.line_end(line) }, m.shift),
            Key::Enter => self.insert(if d.crlf { b"\r\n" } else { b"\n" }, false),
            Key::Tab => self.insert(b"\t", true),
            Key::Backspace => match self.selection() {
                Some(r) => self.delete(r),
                None => self.delete(d.prev_char(self.cursor)..self.cursor),
            },
            Key::Delete => match self.selection() {
                Some(r) => self.delete(r),
                None => self.delete(self.cursor..d.next_char(self.cursor)),
            },
            Key::A if m.command => self.select_all(),
            Key::Z if m.command => self.undo_redo(m.shift),
            Key::Y if m.command => self.undo_redo(true),
            Key::D if m.command => self.duplicate_line(),
            Key::K if m.command && m.shift => self.delete_line(),
            _ => return,
        }
        self.goal_x = None;
    }

    // ---------- drawing ----------

    /// Draws into the whole of `ui`, scrolled to `vs` (each pane has its own). Keyboard input is
    /// handled only when `active`.
    pub fn draw(&mut self, ui: &mut egui::Ui, v: &View, active: bool, vs: &mut PaneView) -> egui::Response {
        let ctx = ui.ctx().clone();
        let (fg, t) = (v.theme.fg, v.theme);
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        if let Some(l) = &self.loading {
            let p = if l.total == 0 { 0.0 } else { l.done.load(std::sync::atomic::Ordering::Relaxed) as f32 / l.total as f32 };
            let painter = ui.painter_at(rect);
            painter.text(rect.center() - vec2(0.0, 14.0), Align2::CENTER_CENTER, format!("Opening {}", self.name()), FontId::proportional(14.0), fg.gamma_multiply(0.7));
            let track = Rect::from_center_size(rect.center() + vec2(0.0, 10.0), vec2(240.0, 3.0));
            painter.rect_filled(track, 2.0, fg.gamma_multiply(0.12));
            painter.rect_filled(Rect::from_min_size(track.min, vec2(240.0 * p.min(1.0), 3.0)), 2.0, fg);
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
            return resp;
        }

        let font = &v.font;
        let row_h = ctx.fonts_mut(|f| f.row_height(font));
        let line_h = row_h * v.spacing;
        let pad = (line_h - row_h) / 2.0;
        let id = resp.id;

        if active && ctx.memory(|m| m.focused().is_none()) {
            resp.request_focus();
        }
        if active && resp.has_focus() {
            ctx.memory_mut(|m| {
                m.set_focus_lock_filter(id, egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: false })
            });
            self.top = vs.top; // so Page Up/Down start from this pane's position
            self.keys(&ctx, font);
        }
        if active && let Some(t) = self.pending_top.take() {
            vs.top = t;
        }

        let lines = self.doc.line_count();
        let digits = lines.to_string().len().max(3);
        let gutter_w = if v.gutter { width(&ctx, font, &b"0".repeat(digits)) + 24.0 } else { 12.0 };
        let bar_w = 10.0;
        let text_rect = Rect::from_min_max(pos2(rect.left() + gutter_w, rect.top()), pos2(rect.right() - bar_w, rect.bottom()));
        let rows = (rect.height() / line_h).floor().max(1.0) as usize;
        if active {
            self.rows = rows;
        }
        let max_top = (lines - 1) as f64;

        // Mouse
        if resp.hovered() {
            let d = ctx.input(|i| i.smooth_scroll_delta);
            vs.top -= (d.y / line_h) as f64;
            vs.scroll_x -= d.x;
        }
        if let Some(p) = resp.interact_pointer_pos() {
            let line = (vs.top + ((p.y - rect.top()) / line_h) as f64).max(0.0) as usize;
            let pos = self.pos_at_x(&ctx, font, line, p.x - text_rect.left() + vs.scroll_x);
            let (pressed, shift) = ctx.input(|i| (i.pointer.primary_pressed(), i.modifiers.shift));
            if pressed && !shift {
                self.anchor = Some(pos);
            } else if pressed {
                self.anchor.get_or_insert(self.cursor);
            }
            self.cursor = pos;
            self.goal_x = None;
            self.typing = false;
            resp.request_focus();
            // Dragging past the top or bottom edge keeps selecting.
            if p.y < rect.top() || p.y > rect.bottom() {
                vs.top += if p.y < rect.top() { -1.0 } else { 1.0 };
                ctx.request_repaint();
            }
        }

        // Keep the cursor on screen after keyboard moves (in the pane being typed in)
        if active && std::mem::take(&mut self.follow) {
            let cl = self.doc.line_of(self.cursor) as f64;
            let rows = rows as f64;
            if cl < vs.top {
                vs.top = (cl - rows / 3.0).max(0.0).min(cl);
            } else if cl > vs.top + rows - 1.0 {
                vs.top = cl - rows + 1.0;
            }
            let cx = self.x_of(&ctx, font, self.cursor);
            let w = text_rect.width();
            if cx < vs.scroll_x {
                vs.scroll_x = (cx - w * 0.25).max(0.0);
            } else if cx > vs.scroll_x + w - 20.0 {
                vs.scroll_x = cx - w * 0.75;
            }
        }

        // Scrollbar
        let track = Rect::from_min_max(pos2(rect.right() - bar_w, rect.top()), rect.right_bottom());
        let thumb_h = (rect.height() * rows as f32 / (lines + rows) as f32).clamp(28.0, rect.height());
        let bar = ui.interact(track, id.with("vbar"), Sense::click_and_drag());
        if let Some(p) = bar.interact_pointer_pos() {
            let t = ((p.y - rect.top() - thumb_h / 2.0) / (rect.height() - thumb_h)).clamp(0.0, 1.0);
            vs.top = t as f64 * max_top;
        }
        vs.top = vs.top.clamp(0.0, max_top);
        vs.scroll_x = vs.scroll_x.max(0.0);

        // Text
        let gutter = ui.painter_at(Rect::from_min_max(rect.min, pos2(text_rect.left(), rect.bottom())));
        let text = ui.painter_at(text_rect);
        let first = vs.top.floor() as usize;
        let frac = (vs.top - first as f64) as f32;
        let x0 = text_rect.left() - vs.scroll_x;
        let sel = self.selection();
        let cur_line = self.doc.line_of(self.cursor);
        let space = width(&ctx, font, b" ");
        for i in 0..=rows + 1 {
            let n = first + i;
            if n >= lines {
                break;
            }
            let y = rect.top() + (i as f32 - frac) * line_h;
            let (s, b) = self.line_bytes(n);
            let le = s + b.len();
            let span = |a: usize, z: usize| (x0 + width(&ctx, font, &b[..a]), x0 + width(&ctx, font, &b[..z]));
            if n == cur_line && active {
                let r = Rect::from_min_size(pos2(rect.left(), y), vec2(rect.width() - bar_w, line_h));
                ui.painter_at(rect).rect_filled(r, 0.0, t.line);
            }
            if let Some(re) = v.highlight {
                for m in re.find_iter(&b).take(200) {
                    let (a, z) = span(m.start(), m.end());
                    text.rect_stroke(Rect::from_min_max(pos2(a, y + 1.0), pos2(z.max(a + 2.0), y + line_h - 1.0)), 2.0, (1.0, t.matches), egui::StrokeKind::Inside);
                }
            }
            if let Some(r) = &sel
                && r.start <= le
                && r.end > s
            {
                let (a, mut z) = span(r.start.max(s) - s, r.end.min(le) - s);
                if r.end > le {
                    z += space;
                }
                text.rect_filled(Rect::from_min_max(pos2(a, y), pos2(z, y + line_h)), 2.0, t.selection);
            }
            text.galley(pos2(x0, y + pad), ctx.fonts_mut(|f| f.layout_no_wrap(display(&b), font.clone(), fg)), fg);
            if n == cur_line {
                let cx = x0 + width(&ctx, font, &b[..(self.cursor - s).min(b.len())]);
                let c = if active { t.cursor } else { t.cursor.gamma_multiply(0.35) };
                text.rect_filled(Rect::from_min_size(pos2(cx - 1.0, y), vec2(2.0, line_h)), 1.0, c);
            }
            if v.gutter {
                let c = if n == cur_line { fg.gamma_multiply(0.7) } else { t.numbers };
                gutter.text(pos2(text_rect.left() - 14.0, y + pad), Align2::RIGHT_TOP, (n + 1).to_string(), font.clone(), c);
            }
        }

        // Scrollbar thumb, drawn last so it sits on top
        if lines > 1 {
            let y = rect.top() + (vs.top / max_top) as f32 * (rect.height() - thumb_h);
            let alpha = if bar.hovered() || bar.dragged() { 0.35 } else { 0.14 };
            let thumb = Rect::from_min_size(pos2(track.left() + 2.0, y), vec2(bar_w - 4.0, thumb_h));
            ui.painter().rect_filled(thumb, CornerRadius::same(4), fg.gamma_multiply(alpha));
        }
        if active {
            (self.top, self.scroll_x) = (vs.top, vs.scroll_x);
        }
        resp.union(bar)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::Query;

    fn text(e: &Editor) -> String {
        String::from_utf8(e.doc.bytes(0..e.doc.len())).unwrap()
    }

    fn re(text: &str, regex: bool) -> Regex {
        Query { text: text.into(), case: true, word: false, regex }.build().unwrap()
    }

    #[test]
    fn edit_find_replace_tools_undo_save() {
        let mut e = Editor::new(0);
        e.doc.crlf = false;
        e.insert(b"alpha beta\ngamma alpha\nalpha", false);

        // find wraps around, and an empty regex match doesn't get stuck
        e.cursor = 0;
        e.anchor = None;
        assert!(e.find_next(&re("alpha", false), true));
        assert_eq!(e.selection(), Some(0..5));
        assert!(e.find_next(&re("alpha", false), true));
        assert_eq!(e.selection(), Some(17..22));
        assert!(e.find_next(&re("^", true), true));
        let first = e.cursor;
        assert!(e.find_next(&re("^", true), true));
        assert_ne!(e.cursor, first);

        // replace one (only when the selection is a match), regex groups, replace all
        e.jump(0..5);
        e.replace_one(&re(r"(al)pha", true), "[$1]", true);
        assert!(text(&e).starts_with("[al] beta"));
        e.replace_all(&re("alpha", false), "$X", false);
        assert_eq!(text(&e), "[al] beta\ngamma $X\n$X");
        assert_eq!(e.msg.take().as_deref(), Some("Replaced 2"));

        // tools run on the selection only, and each is one undo step
        e.jump(0..4);
        e.transform("b64", |b| Ok(b.iter().rev().copied().collect()));
        assert_eq!(text(&e), "]la[ beta\ngamma $X\n$X");
        e.undo_redo(false);
        assert_eq!(text(&e), "[al] beta\ngamma $X\n$X");
        e.undo_redo(true);
        assert_eq!(text(&e), "]la[ beta\ngamma $X\n$X");
        e.transform("fail", |_| Err("nope".into()));
        assert_eq!(e.msg.take().as_deref(), Some("fail: nope"));

        // line tools
        e.anchor = None;
        e.cursor = 12; // inside "gamma"
        e.duplicate_line();
        assert_eq!(text(&e), "]la[ beta\ngamma $X\ngamma $X\n$X");
        e.delete_line();
        e.delete_line();
        assert_eq!(text(&e), "]la[ beta\ngamma $X"); // the second delete took the last line
        e.set_line_endings(true);
        assert_eq!(text(&e), "]la[ beta\r\ngamma $X");
        assert!(e.doc.crlf);

        // save goes through a temp file, reopens, and keeps the selection
        let path = std::env::temp_dir().join("te_editor_test.txt");
        std::fs::write(&path, "old").unwrap();
        e.path = Some(path.clone());
        e.jump(0..4);
        assert!(e.save(false));
        while e.poll().is_none() {
            std::thread::yield_now();
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "]la[ beta\r\ngamma $X");
        assert!(!e.modified && !path.with_extension("txt.te-tmp").exists());
        assert_eq!(e.selection(), Some(0..4));
        assert_eq!(text(&e), "]la[ beta\r\ngamma $X");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn edge_cases_and_failed_saves() {
        // Every operation on an empty document is a harmless no-op.
        let mut e = Editor::new(1);
        e.doc.crlf = false;
        e.delete_line();
        e.duplicate_line(); // duplicating the one empty line adds a newline
        assert_eq!(e.doc.len(), 1);
        e.undo_redo(false);
        e.undo_redo(true);
        e.undo_redo(false);
        e.undo_redo(false); // nothing left to undo
        assert!(!e.find_next(&re("x", false), true));
        assert!(!e.find_next(&re("x", false), false));
        e.transform("t", |b| Ok(b.to_vec()));
        e.replace_all(&re("x", false), "y", false);
        e.set_line_endings(false);
        e.delete(0..0);
        assert_eq!(e.doc.len(), 0);

        // A tool that panics leaves the text alone and reports it.
        e.insert(b"keep me", false);
        e.select_all();
        e.transform("boom", |_| panic!("library bug"));
        assert_eq!(text(&e), "keep me");
        assert!(e.msg.take().unwrap().contains("crashed"));

        let dir = std::env::temp_dir().join("te_save_tests");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Saving while the file is (re)opening is refused, so the empty placeholder can't overwrite it.
        let file = dir.join("opening.txt");
        std::fs::write(&file, "original").unwrap();
        let mut opening = Editor::open(2, file.clone());
        assert!(!opening.save(false));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "original");

        // Saving into a folder that doesn't exist fails cleanly and leaves no temp file behind.
        e.path = Some(dir.join("missing").join("x.txt"));
        assert!(!e.save(false));
        assert!(e.msg.take().unwrap().starts_with("Save failed"));
        assert_eq!(text(&e), "keep me");

        // A read-only target can't be replaced: the original stays, the text is kept in the temp file.
        let ro = dir.join("readonly.txt");
        std::fs::write(&ro, "old").unwrap();
        let mut perm = std::fs::metadata(&ro).unwrap().permissions();
        perm.set_readonly(true);
        std::fs::set_permissions(&ro, perm.clone()).unwrap();
        e.path = Some(ro.clone());
        e.save(false);
        while e.poll().is_none() && e.loading.is_some() {
            std::thread::yield_now();
        }
        assert_eq!(std::fs::read_to_string(&ro).unwrap(), "old");
        assert_eq!(std::fs::read_to_string(dir.join("readonly.txt.te-tmp")).unwrap(), "keep me");
        assert_eq!(text(&e), "keep me");
        #[allow(clippy::permissions_set_readonly_false)] // Windows: clears the read-only attribute
        perm.set_readonly(false);
        std::fs::set_permissions(&ro, perm).unwrap();
        drop(e);
        drop(opening);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn undo_memory_is_bounded() {
        let mut e = Editor::new(3);
        e.insert(&b"line of text\n".repeat(20_000), false);
        // Thousands of scattered single edits: each adds pieces, and each is its own undo step.
        for i in 0..3000usize {
            e.anchor = None;
            e.cursor = (i * 7919) % e.doc.len();
            e.insert(b"x\n", false);
        }
        let held: usize = e.undo.iter().map(|(p, _)| p.len()).sum();
        let last = e.undo.last().map_or(0, |(p, _)| p.len());
        assert!(held <= UNDO_PIECES + last, "undo holds {held} pieces");
        assert!(e.undo.len() <= UNDO_LIMIT);
        // Undo still works after trimming.
        let before = e.doc.len();
        e.undo_redo(false);
        assert_eq!(e.doc.len(), before - 2);
    }

    #[test]
    fn background_find_and_cancel() {
        // Tiny slices give the same answers as one big search, including matches across slice edges.
        let mut small = Editor::new(5);
        small.insert(b"abcabcXYZabcXYZab", false);
        let xyz = re("cXYZa", false);
        let never = AtomicBool::new(false);
        for (r, fwd) in [(0..17, true), (0..17, false), (3..12, true), (3..12, false), (9..10, true)] {
            assert_eq!(find_sliced(&small.doc, &xyz, r.clone(), fwd, &never, 2).flatten(), small.doc.find_in(&xyz, r, fwd));
        }

        // A document over the threshold is searched in the background.
        let mut e = Editor::new(4);
        e.doc.crlf = false;
        e.insert(&b"lorem ipsum
".repeat(2_000_000), false);
        let at = e.doc.len();
        e.insert(b"NEEDLE", false);
        let wait = |e: &mut Editor| {
            while e.is_finding() {
                e.poll_find();
                std::thread::yield_now();
            }
        };
        for forward in [true, false] {
            (e.cursor, e.anchor) = (0, None);
            assert!(e.find_next(&re("NEEDLE", false), forward));
            assert!(e.is_finding());
            wait(&mut e);
            assert_eq!(e.selection(), Some(at..at + 6));
        }

        // Esc (cancel) stops it and says so; typing cancels a running find too.
        assert!(e.find_next(&re("MISSING", false), true));
        e.cancel_find(true);
        assert!(!e.is_finding());
        assert_eq!(e.msg.take().as_deref(), Some("Search cancelled"));
        e.find_next(&re("MISSING", false), true);
        e.insert(b"x", false);
        assert!(!e.is_finding());
        // A miss over the whole document reports it.
        e.find_next(&re("MISSING", false), true);
        wait(&mut e);
        assert_eq!(e.msg.take().as_deref(), Some("No matches"));
    }
}
