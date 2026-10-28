//! Find options and the background "search everywhere" job (current file, open tabs, or a folder).

use crate::doc::{self, Doc};
use regex::bytes::{Regex, RegexBuilder};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// Results are capped so a search for "e" in a 2 GB file can't eat the machine (~250 bytes a hit).
pub const MAX_HITS: usize = 50_000;
/// Per file too, so one huge log doesn't crowd every other file out of the results.
pub const FILE_HITS: usize = 5_000;
const PREVIEW_BEFORE: usize = 60;
const PREVIEW_AFTER: usize = 120;

#[derive(Clone, Default)]
pub struct Query {
    pub text: String,
    pub case: bool,
    pub word: bool,
    pub regex: bool,
}

impl Query {
    pub fn build(&self) -> Result<Regex, String> {
        if self.text.is_empty() {
            return Err("nothing to search for".into());
        }
        let mut p = if self.regex { self.text.clone() } else { regex::escape(&self.text) };
        if self.word {
            p = format!(r"\b(?:{p})\b");
        }
        RegexBuilder::new(&p)
            .case_insensitive(!self.case)
            .multi_line(true)
            .size_limit(64 << 20)
            .build()
            .map_err(|e| e.to_string().lines().last().unwrap_or("bad regex").trim().to_string())
    }
}

#[derive(Clone, PartialEq)]
pub enum Target {
    Tab(u64),
    File(PathBuf),
}

pub struct Hit {
    pub range: Range<usize>,
    pub line: usize,
    /// Text before the match, the match, and text after it, on the hit's line.
    pub preview: [String; 3],
}

pub struct FileHits {
    pub name: String,
    pub target: Target,
    pub hits: Vec<Hit>,
}

pub struct Search {
    pub label: String,
    pub results: Arc<Mutex<Vec<FileHits>>>,
    pub files_done: Arc<AtomicUsize>,
    pub hits: Arc<AtomicUsize>,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Search {
    pub fn running(&self) -> bool {
        self.handle.as_ref().is_some_and(|h| !h.is_finished())
    }

    pub fn truncated(&self) -> bool {
        self.hits.load(Ordering::Relaxed) >= MAX_HITS
    }

    /// Stops the worker and waits for it, which releases its file mappings (needed before a save
    /// replaces a file on Windows).
    pub fn stop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for Search {
    fn drop(&mut self) {
        self.stop();
    }
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).replace('\t', "    ")
}

fn search_doc(doc: &Doc, re: &Regex, hits: &AtomicUsize, cancel: &AtomicBool) -> Vec<Hit> {
    let mut out = Vec::new();
    doc.scan(re, 0..doc.len(), |r| {
        if out.len() >= FILE_HITS || cancel.load(Ordering::Relaxed) || hits.fetch_add(1, Ordering::Relaxed) >= MAX_HITS {
            return false;
        }
        let line = doc.line_of(r.start);
        let (ls, le) = (doc.line_start(line), doc.line_end(line));
        let a = ls.max(r.start.saturating_sub(PREVIEW_BEFORE));
        let m = r.end.min(le).max(r.start);
        let z = le.min(m + PREVIEW_AFTER);
        let pre = lossy(&doc.bytes(a..r.start));
        out.push(Hit {
            range: r.clone(),
            line,
            preview: [pre.trim_start().to_string(), lossy(&doc.bytes(r.start..m)), lossy(&doc.bytes(m..z))],
        });
        true
    });
    out
}

/// Text files only: skips anything with a NUL byte near the start.
fn looks_binary(path: &std::path::Path) -> bool {
    use std::io::Read;
    let mut buf = [0u8; 8192];
    let n = std::fs::File::open(path).and_then(|mut f| f.read(&mut buf)).unwrap_or(0);
    buf[..n].contains(&0)
}

/// Searches `tabs` (snapshots of open documents) and then every text file under `folder`,
/// streaming results into `results` as each file finishes. Files in `skip` are open as tabs, so
/// their (possibly edited) tab text was searched instead.
pub fn start(label: String, re: Regex, tabs: Vec<(u64, String, Doc)>, folder: Option<PathBuf>, skip: Vec<PathBuf>) -> Search {
    let results = Arc::new(Mutex::new(Vec::new()));
    let (files_done, hits, cancel) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)), Arc::new(AtomicBool::new(false)));
    let (r, f, h, c) = (results.clone(), files_done.clone(), hits.clone(), cancel.clone());
    let handle = std::thread::spawn(move || {
        let push = |name: String, target: Target, found: Vec<Hit>| {
            f.fetch_add(1, Ordering::Relaxed);
            if !found.is_empty() {
                r.lock().unwrap().push(FileHits { name, target, hits: found });
            }
        };
        for (id, name, doc) in tabs {
            let found = search_doc(&doc, &re, &h, &c);
            push(name, Target::Tab(id), found);
        }
        let Some(root) = folder else { return };
        // List the files first (cheap), then search them on a few threads. Each thread holds one
        // file at a time, so memory stays bounded; 4 keeps the worst case (four ~60 MB files
        // read at once) modest while still overlapping disk waits.
        let mut files = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            if c.load(Ordering::Relaxed) {
                return;
            }
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let Ok(ft) = e.file_type() else { continue };
                if ft.is_dir() {
                    if e.file_name() != ".git" {
                        stack.push(e.path());
                    }
                } else if !skip.contains(&e.path()) {
                    files.push(e.path());
                }
            }
        }
        files.sort();
        let next = AtomicUsize::new(0);
        let workers = std::thread::available_parallelism().map_or(4, |n| n.get()).min(4);
        std::thread::scope(|sc| {
            for _ in 0..workers {
                sc.spawn(|| {
                    while let Some(path) = files.get(next.fetch_add(1, Ordering::Relaxed)) {
                        if c.load(Ordering::Relaxed) || h.load(Ordering::Relaxed) >= MAX_HITS {
                            return;
                        }
                        let found = if looks_binary(path) {
                            Vec::new()
                        } else {
                            doc::load(path, &AtomicUsize::new(0)).map(|doc| search_doc(&doc, &re, &h, &c)).unwrap_or_default()
                        };
                        let name = path.strip_prefix(&root).unwrap_or(path).to_string_lossy().into_owned();
                        push(name, Target::File(path.clone()), found);
                    }
                });
            }
        });
        // Threads finish files out of order; show the results sorted by path.
        r.lock().unwrap().sort_by(|a, b| a.name.cmp(&b.name));
    });
    Search { label, results, files_done, hits, cancel, handle: Some(handle) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_and_folder_search() {
        let q = |text: &str, case, word, regex| Query { text: text.into(), case, word, regex }.build().unwrap();
        assert!(q("a.b", true, false, false).is_match(b"xa.b") && !q("a.b", true, false, false).is_match(b"axb"));
        assert!(q("Foo", false, false, false).is_match(b"FOO") && !q("Foo", true, false, false).is_match(b"FOO"));
        assert!(q("cat", true, true, false).is_match(b"a cat.") && !q("cat", true, true, false).is_match(b"concat"));
        assert!(q(r"\d{3}", true, false, true).is_match(b"x123"));
        assert!(Query { text: "(".into(), regex: true, ..Default::default() }.build().is_err());

        let dir = std::env::temp_dir().join("te_search_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), "one needle\ntwo\nthree needle here\n").unwrap();
        std::fs::write(dir.join("sub/b.log"), "\tneedle at start\n").unwrap();
        std::fs::write(dir.join("bin.dat"), b"needle\0\0binary").unwrap();
        let mut s = start("t".into(), q("needle", true, false, false), vec![], Some(dir.clone()), vec![]);
        while s.running() {
            std::thread::yield_now();
        }
        let res = s.results.lock().unwrap();
        let mut got: Vec<(String, usize, String)> =
            res.iter().flat_map(|f| f.hits.iter().map(|h| (f.name.replace('\\', "/"), h.line, h.preview.concat()))).collect();
        got.sort();
        assert_eq!(
            got,
            vec![
                ("a.txt".into(), 0, "one needle".into()),
                ("a.txt".into(), 2, "three needle here".into()),
                ("sub/b.log".into(), 0, "needle at start".into()),
            ]
        );
        drop(res);
        s.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Memory check: TE_BENCH=big.log cargo test --release memory -- --ignored --nocapture
/// (sample the process's private bytes from outside while it runs).
#[cfg(test)]
#[test]
#[ignore]
fn memory() {
    let path = PathBuf::from(std::env::var("TE_BENCH").unwrap());
    let say = |m: &str| println!("[{:?}] {m}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() % 100_000);
    let docs: Vec<Doc> = (0..5).map(|_| doc::load(&path, &AtomicUsize::new(0)).unwrap()).collect();
    say("5 docs open");
    std::thread::sleep(std::time::Duration::from_secs(2));
    let tabs = docs.iter().enumerate().map(|(i, d)| (i as u64, format!("tab{i}"), d.clone())).collect();
    let re = Query { text: "user_id".into(), case: true, ..Default::default() }.build().unwrap();
    let t = std::time::Instant::now();
    let s = start("mem".into(), re, tabs, None, vec![]);
    while s.running() {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    say(&format!("search done: {} hits in {:?}", s.results.lock().unwrap().iter().map(|f| f.hits.len()).sum::<usize>(), t.elapsed()));
    std::thread::sleep(std::time::Duration::from_secs(2));
    drop(s);
    drop(docs);
    say("all dropped");
    std::thread::sleep(std::time::Duration::from_secs(3));
}

/// Folder-search load check: TE_FOLDER=D:\some\big\tree cargo test --release folder_load -- --ignored --nocapture
#[cfg(test)]
#[test]
#[ignore]
fn folder_load() {
    let root = PathBuf::from(std::env::var("TE_FOLDER").unwrap());
    let re = Query { text: "fn main".into(), case: true, ..Default::default() }.build().unwrap();
    let t = std::time::Instant::now();
    let s = start("load".into(), re, vec![], Some(root), vec![]);
    while s.running() {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let res = s.results.lock().unwrap();
    println!(
        "folder search: {} files scanned, {} files matched, {} hits in {:?}",
        s.files_done.load(Ordering::Relaxed),
        res.len(),
        res.iter().map(|f| f.hits.len()).sum::<usize>(),
        t.elapsed()
    );
}
