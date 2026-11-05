//! Split a document into numbered part files of at most N bytes each, for upload limits.

use crate::doc::Doc;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;

/// Decimal units: platforms that say "50 MB" may mean 50,000,000 or 52,428,800 bytes, and
/// decimal parts fit either.
pub const UNITS: &[(&str, f64)] = &[("bytes", 1.0), ("KB", 1e3), ("MB", 1e6), ("GB", 1e9)];

/// How far back from a cut to look for a line end (a longer line is cut where the limit falls).
const LOOKBACK: usize = 8 << 20;
const WRITE_CHUNK: usize = 8 << 20;

/// `big.log` part 3 of ~40 → `big_part03.log` (zero-padded so the parts sort in order).
pub fn part_name(name: &str, part: usize, estimate: usize) -> String {
    let (stem, ext) = match name.rfind('.').filter(|&i| i > 0) {
        Some(i) => (&name[..i], &name[i..]),
        None => (name, ""),
    };
    let width = (estimate.max(1).to_string().len() + 1).max(3);
    format!("{stem}_part{part:0width$}{ext}")
}

/// Parts needed if every part is full.
pub fn estimate(len: usize, max: usize) -> usize {
    len.div_ceil(max.max(1))
}

/// Writes the parts into `dir`. On error or cancel, deletes what it wrote. `done` counts bytes written.
pub fn run(doc: &Doc, dir: &Path, name: &str, max: usize, at_lines: bool, done: &AtomicUsize, cancel: &AtomicBool) -> Result<Vec<PathBuf>, String> {
    let (len, max) = (doc.len(), max.max(1));
    let est = estimate(len, max);
    let mut written: Vec<PathBuf> = Vec::new();
    let result = (|| {
        let mut start = 0;
        while start < len {
            let mut end = (start + max).min(len);
            if at_lines && end < len {
                let back = (end - start).min(LOOKBACK);
                if let Some(i) = memchr::memrchr(b'\n', &doc.bytes(end - back..end)) {
                    end = end - back + i + 1;
                }
            }
            let path = dir.join(part_name(name, written.len() + 1, est));
            written.push(path.clone());
            let mut w = BufWriter::with_capacity(1 << 20, std::fs::File::create(&path).map_err(|e| format!("can't create {}: {e}", path.display()))?);
            let mut at = start;
            while at < end {
                if cancel.load(Ordering::Relaxed) {
                    return Err("cancelled".to_string());
                }
                let to = (at + WRITE_CHUNK).min(end);
                w.write_all(&doc.bytes(at..to)).map_err(|e| format!("writing {}: {e}", path.display()))?;
                done.fetch_add(to - at, Ordering::Relaxed);
                at = to;
            }
            w.into_inner().map_err(|e| e.to_string())?.sync_all().map_err(|e| e.to_string())?;
            start = end;
        }
        Ok(())
    })();
    match result {
        Ok(()) => Ok(written),
        Err(e) => {
            // Never leave a half set behind.
            for p in &written {
                let _ = std::fs::remove_file(p);
            }
            Err(e)
        }
    }
}

pub struct Job {
    pub dir: PathBuf,
    pub done: Arc<AtomicUsize>,
    pub total: usize,
    cancel: Arc<AtomicBool>,
    handle: JoinHandle<Result<Vec<PathBuf>, String>>,
}

impl Job {
    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn join(self) -> Result<Vec<PathBuf>, String> {
        self.handle.join().unwrap_or_else(|_| Err("the split crashed".into()))
    }
}

/// Splits a snapshot of the document on a background thread.
pub fn start(doc: Doc, dir: PathBuf, name: String, max: usize, at_lines: bool) -> Job {
    let (done, cancel) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicBool::new(false)));
    let (d, c, out) = (done.clone(), cancel.clone(), dir.clone());
    let total = doc.len();
    let handle = std::thread::spawn(move || run(&doc, &out, &name, max, at_lines, &d, &c));
    Job { dir, done, total, cancel, handle }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_exactly_and_cleans_up() {
        assert_eq!(part_name("big.log", 3, 40), "big_part003.log");
        assert_eq!(part_name("big.log", 7, 1500), "big_part00007.log");
        assert_eq!(part_name("notes", 1, 2), "notes_part001");
        assert_eq!(part_name(".env", 2, 2), ".env_part002");
        assert_eq!(estimate(10, 3), 4);

        let mut doc = Doc::new();
        let text = b"alpha\nbravo charlie\ndelta\nthis line is far longer than ten bytes\necho\n";
        doc.insert(0, text);
        let dir = std::env::temp_dir().join("te_split_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (done, never) = (AtomicUsize::new(0), AtomicBool::new(false));

        for at_lines in [true, false] {
            let parts = run(&doc, &dir, "t.txt", 10, at_lines, &done, &never).unwrap();
            let bytes: Vec<Vec<u8>> = parts.iter().map(|p| std::fs::read(p).unwrap()).collect();
            // Nothing lost, nothing over the limit.
            assert_eq!(bytes.concat(), text);
            assert!(bytes.iter().all(|b| !b.is_empty() && b.len() <= 10));
            if at_lines {
                // Every part ends on a line end, except where one line alone was over the limit.
                assert_eq!(&bytes[0], b"alpha\n");
                assert_eq!(&bytes[1], b"bravo char"); // that line alone is over 10 bytes
                assert!(bytes.iter().filter(|b| !b.ends_with(b"\n")).count() <= 5);
            } else {
                assert!(bytes[..bytes.len() - 1].iter().all(|b| b.len() == 10));
            }
            for p in parts {
                std::fs::remove_file(p).unwrap();
            }
        }

        // A cancelled split leaves no files behind.
        let cancelled = run(&doc, &dir, "c.txt", 10, true, &done, &AtomicBool::new(true));
        assert_eq!(cancelled.unwrap_err(), "cancelled");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);

        // A failure (folder gone) reports the error and also leaves nothing.
        let missing = dir.join("nope");
        assert!(run(&doc, &missing, "m.txt", 10, true, &done, &never).unwrap_err().starts_with("can't create"));

        // The background job gives the same result.
        let job = start(doc.clone(), dir.clone(), "j.txt".into(), 25, true);
        while !job.is_finished() {
            std::thread::yield_now();
        }
        let total = job.total;
        let parts = job.join().unwrap();
        let joined: Vec<u8> = parts.iter().flat_map(|p| std::fs::read(p).unwrap()).collect();
        assert_eq!((joined.as_slice(), total), (&text[..], text.len()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Real-size check: TE_BENCH=big.log cargo test --release split_speed -- --ignored --nocapture
#[cfg(test)]
#[test]
#[ignore]
fn split_speed() {
    let path = PathBuf::from(std::env::var("TE_BENCH").unwrap());
    let doc = crate::doc::load(&path, &AtomicUsize::new(0)).unwrap();
    let out = path.with_extension("parts");
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).unwrap();
    let t = std::time::Instant::now();
    let parts = run(&doc, &out, "big.log", 50_000_000, true, &AtomicUsize::new(0), &AtomicBool::new(false)).unwrap();
    let took = t.elapsed();
    let sizes: Vec<u64> = parts.iter().map(|p| std::fs::metadata(p).unwrap().len()).collect();
    let ends_ok = parts.iter().all(|p| std::fs::read(p).unwrap().last() == Some(&b'\n'));
    println!("{} parts in {took:?}; largest {} bytes; total {} of {}; every part ends at a line end: {ends_ok}",
        parts.len(), sizes.iter().max().unwrap(), sizes.iter().sum::<u64>(), doc.len());
    let _ = std::fs::remove_dir_all(&out);
}
