//! Document model. The file is memory-mapped, never read into RAM. Edits live in a piece table
//! on top of the mapping. A sparse newline index (one count per 64 KiB chunk) means any line
//! lookup scans at most 64 KiB, no matter how big the file is.

use memchr::memchr_iter;
use memmap2::Mmap;
use regex::bytes::Regex;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::JoinHandle;

const CHUNK: usize = 64 * 1024;
const BLOCK: usize = 8 << 20;
const OVERLAP: usize = 64 << 10;
const CONTEXT: usize = 256;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Src {
    Orig,
    Add,
}

#[derive(Clone, Copy, Debug)]
pub struct Piece {
    src: Src,
    start: usize,
    len: usize,
    nl: usize,
}

/// Files smaller than this are read into memory and the file is closed at once, so te never
/// locks them. Bigger ones are memory-mapped: Windows then blocks other programs from
/// truncating or replacing that file until its tab is closed (appending still works).
const READ_LIMIT: u64 = 64 << 20;

/// The file's original bytes.
enum Orig {
    Mem(Vec<u8>),
    Map(Mmap),
}

impl std::ops::Deref for Orig {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Orig::Mem(v) => v,
            Orig::Map(m) => m,
        }
    }
}

/// Cloning is cheap: the original bytes, index and typed-text buffer are shared (the buffer is
/// copied only if it is typed into while a clone is alive). That is how a background search
/// gets its own snapshot.
#[derive(Clone)]
pub struct Doc {
    orig: Option<Arc<Orig>>,
    /// index[i] = number of '\n' in orig before byte i*CHUNK; last entry = total.
    index: Arc<Vec<usize>>,
    add: Arc<Vec<u8>>,
    pieces: Vec<Piece>,
    pub crlf: bool,
}

pub struct Loading {
    pub path: PathBuf,
    pub done: Arc<AtomicUsize>,
    pub total: usize,
    handle: JoinHandle<io::Result<Doc>>,
}

impl Loading {
    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }
    pub fn join(self) -> io::Result<Doc> {
        self.handle.join().unwrap_or_else(|_| Err(io::Error::other("loader panicked")))
    }
}

fn count_nl(b: &[u8]) -> usize {
    // Plain filter-count auto-vectorizes and beats memchr on dense newlines.
    b.iter().filter(|&&c| c == b'\n').count()
}

/// Opens in the background; the UI polls `Loading`.
pub fn open(path: PathBuf) -> Loading {
    let total = std::fs::metadata(&path).map(|m| m.len() as usize).unwrap_or(0);
    let done = Arc::new(AtomicUsize::new(0));
    let (p, d) = (path.clone(), done.clone());
    let handle = std::thread::spawn(move || load(&p, &d));
    Loading { path, done, total, handle }
}

/// Reads or maps the file and builds the newline index. `done` counts bytes indexed, for progress bars.
pub fn load(path: &Path, done: &AtomicUsize) -> io::Result<Doc> {
    use std::io::Read;
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(Doc::new());
    }
    let map = if len < READ_LIMIT {
        let mut v = Vec::with_capacity(len as usize);
        file.read_to_end(&mut v)?;
        Orig::Mem(v)
    } else {
        // SAFETY: the mapping locks the file against truncation on Windows, so it can't shrink under us.
        Orig::Map(unsafe { Mmap::map(&file)? })
    };
    drop(file);
    // Count newlines per chunk on all cores (page faults dominate, so threads help), then
    // prefix-sum. Each thread takes at least 4 MiB, so small files use one.
    let mut counts = vec![0; map.len().div_ceil(CHUNK)];
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(counts.len().div_ceil(64));
    let per = counts.len().div_ceil(threads);
    std::thread::scope(|s| {
        for (t, out) in counts.chunks_mut(per).enumerate() {
            let map = &map;
            s.spawn(move || {
                for (j, c) in out.iter_mut().enumerate() {
                    let st = (t * per + j) * CHUNK;
                    *c = count_nl(&map[st..(st + CHUNK).min(map.len())]);
                    if j % 256 == 255 {
                        done.fetch_add(256 * CHUNK, Ordering::Relaxed);
                    }
                }
            });
        }
    });
    let mut index = Vec::with_capacity(counts.len() + 1);
    index.push(0);
    for c in counts {
        index.push(index.last().unwrap() + c);
    }
    Ok(Doc::from_map(map, index))
}

impl Doc {
    pub fn new() -> Doc {
        Doc { orig: None, index: Arc::new(vec![0]), add: Arc::default(), pieces: Vec::new(), crlf: cfg!(windows) }
    }

    fn from_map(map: Orig, index: Vec<usize>) -> Doc {
        let crlf = memchr::memmem::find(&map[..map.len().min(CHUNK)], b"\r\n").is_some();
        let pieces = vec![Piece { src: Src::Orig, start: 0, len: map.len(), nl: *index.last().unwrap() }];
        Doc { orig: Some(Arc::new(map)), index: Arc::new(index), add: Arc::default(), pieces, crlf }
    }

    fn orig(&self) -> &[u8] {
        self.orig.as_deref().map_or(&[], |m| &m[..])
    }

    /// True if the file is memory-mapped (and so locked against being replaced while open).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_mapped(&self) -> bool {
        matches!(self.orig.as_deref(), Some(Orig::Map(_)))
    }

    fn slice(&self, p: &Piece) -> &[u8] {
        let buf = if p.src == Src::Orig { self.orig() } else { &self.add };
        &buf[p.start..p.start + p.len]
    }

    pub fn len(&self) -> usize {
        self.pieces.iter().map(|p| p.len).sum()
    }

    pub fn line_count(&self) -> usize {
        self.pieces.iter().map(|p| p.nl).sum::<usize>() + 1
    }

    /// Newlines in orig before byte `off`.
    fn orig_nl_before(&self, off: usize) -> usize {
        let c = off / CHUNK;
        self.index[c] + count_nl(&self.orig()[c * CHUNK..off])
    }

    /// Byte offset of the k-th (1-based) newline in orig.
    fn orig_nth_nl(&self, k: usize) -> usize {
        let c = self.index.partition_point(|&x| x < k) - 1;
        let start = c * CHUNK;
        let end = (start + CHUNK).min(self.orig().len());
        start + memchr_iter(b'\n', &self.orig()[start..end]).nth(k - self.index[c] - 1).unwrap()
    }

    fn nl_in(&self, src: Src, start: usize, len: usize) -> usize {
        match src {
            Src::Orig => self.orig_nl_before(start + len) - self.orig_nl_before(start),
            Src::Add => count_nl(&self.add[start..start + len]),
        }
    }

    pub fn bytes(&self, r: Range<usize>) -> Vec<u8> {
        let mut out = Vec::with_capacity(r.len());
        let mut pos = 0;
        for p in &self.pieces {
            let (ps, pe) = (pos, pos + p.len);
            pos = pe;
            if pe <= r.start {
                continue;
            }
            if ps >= r.end {
                break;
            }
            out.extend_from_slice(&self.slice(p)[r.start.max(ps) - ps..r.end.min(pe) - ps]);
        }
        out
    }

    pub fn line_start(&self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        let (mut rem, mut pos) = (n, 0);
        for p in &self.pieces {
            if p.nl >= rem {
                let k = match p.src {
                    Src::Orig => self.orig_nth_nl(self.orig_nl_before(p.start) + rem) - p.start,
                    Src::Add => memchr_iter(b'\n', self.slice(p)).nth(rem - 1).unwrap(),
                };
                return pos + k + 1;
            }
            rem -= p.nl;
            pos += p.len;
        }
        pos
    }

    /// End of line n, before its "\n" or "\r\n".
    pub fn line_end(&self, n: usize) -> usize {
        if n + 1 >= self.line_count() {
            return self.len();
        }
        let e = self.line_start(n + 1) - 1;
        if e > self.line_start(n) && self.bytes(e - 1..e) == b"\r" { e - 1 } else { e }
    }

    pub fn line_of(&self, off: usize) -> usize {
        let (mut line, mut pos) = (0, 0);
        for p in &self.pieces {
            if off < pos + p.len {
                return line + self.nl_in(p.src, p.start, off - pos);
            }
            line += p.nl;
            pos += p.len;
        }
        line
    }

    pub fn next_char(&self, at: usize) -> usize {
        let b = self.bytes(at..(at + 4).min(self.len()));
        let w = match b.first() {
            None => 0,
            Some(_) if b.starts_with(b"\r\n") => 2,
            Some(0xC0..=0xDF) => 2,
            Some(0xE0..=0xEF) => 3,
            Some(0xF0..=0xF7) => 4,
            Some(_) => 1,
        };
        (at + w).min(self.len())
    }

    pub fn prev_char(&self, at: usize) -> usize {
        if at == 0 {
            return 0;
        }
        let s = at.saturating_sub(4);
        let b = self.bytes(s..at);
        if b.ends_with(b"\r\n") {
            return at - 2;
        }
        let mut i = b.len() - 1;
        while i > 0 && b[i] & 0xC0 == 0x80 {
            i -= 1;
        }
        s + i
    }

    /// Make sure a piece boundary exists at `at`; returns the index of the piece starting there.
    fn split(&mut self, at: usize) -> usize {
        let mut pos = 0;
        for i in 0..self.pieces.len() {
            let p = self.pieces[i];
            if pos == at {
                return i;
            }
            if at < pos + p.len {
                let k = at - pos;
                let nl = self.nl_in(p.src, p.start, k);
                self.pieces[i] = Piece { len: k, nl, ..p };
                self.pieces.insert(i + 1, Piece { start: p.start + k, len: p.len - k, nl: p.nl - nl, ..p });
                return i + 1;
            }
            pos += p.len;
        }
        self.pieces.len()
    }

    pub fn insert(&mut self, at: usize, text: &[u8]) {
        if text.is_empty() {
            return;
        }
        let i = self.split(at);
        let add = Arc::make_mut(&mut self.add);
        let start = add.len();
        add.extend_from_slice(text);
        let nl = count_nl(text);
        // Typing appends to the add buffer contiguously; grow the last piece instead of adding one.
        if i > 0 {
            let q = &mut self.pieces[i - 1];
            if q.src == Src::Add && q.start + q.len == start {
                q.len += text.len();
                q.nl += nl;
                return;
            }
        }
        self.pieces.insert(i, Piece { src: Src::Add, start, len: text.len(), nl });
    }

    pub fn delete(&mut self, r: Range<usize>) {
        if r.is_empty() {
            return;
        }
        let i = self.split(r.start);
        let j = self.split(r.end);
        self.pieces.drain(i..j);
    }

    pub fn snapshot(&self) -> Vec<Piece> {
        self.pieces.clone()
    }

    pub fn restore(&mut self, pieces: Vec<Piece>) {
        self.pieces = pieces;
    }

    /// Calls `f` for each regex match at or after `from`, in order, until it returns false.
    /// Reads 8 MiB blocks, so memory stays flat on any file size. Each block also sees a little
    /// text before it, so `^`, `\b` and look-behind behave as if the whole file were searched.
    // ponytail: one match longer than OVERLAP straddling a block edge is cut short; raise OVERLAP if it bites.
    pub fn scan(&self, re: &Regex, from: usize, mut f: impl FnMut(Range<usize>) -> bool) {
        let len = self.len();
        let (mut s, mut next) = (from.min(len), from.min(len));
        loop {
            let e = (s + BLOCK).min(len);
            let b0 = s.saturating_sub(CONTEXT);
            let buf = self.bytes(b0..(e + OVERLAP).min(len));
            let mut at = next.max(s) - b0;
            while at <= buf.len() {
                let Some(m) = re.find_at(&buf, at) else { break };
                let (ms, me) = (b0 + m.start(), b0 + m.end());
                if ms >= e && e < len {
                    break; // the next block owns it
                }
                if !f(ms..me) {
                    return;
                }
                next = if me > ms { me } else { me + 1 };
                at = next - b0;
            }
            if e >= len {
                return;
            }
            s = e;
        }
    }

    /// First match at or after `from` (forward) or the last one starting before `from` (backward).
    pub fn find(&self, re: &Regex, from: usize, forward: bool) -> Option<Range<usize>> {
        if forward {
            let mut hit = None;
            self.scan(re, from, |r| {
                hit = Some(r);
                false
            });
            return hit;
        }
        let len = self.len();
        let mut e = from.min(len);
        while e > 0 {
            let s = e.saturating_sub(BLOCK);
            let b0 = s.saturating_sub(CONTEXT);
            let buf = self.bytes(b0..(e + OVERLAP).min(len));
            let (mut last, mut at) = (None, s - b0);
            while at <= buf.len() {
                let Some(m) = re.find_at(&buf, at) else { break };
                if b0 + m.start() >= e {
                    break;
                }
                last = Some(b0 + m.start()..b0 + m.end());
                at = if m.end() > m.start() { m.end() } else { m.end() + 1 };
            }
            if last.is_some() {
                return last;
            }
            e = s;
        }
        None
    }

    pub fn write_to(&self, path: &Path) -> io::Result<()> {
        let mut w = BufWriter::with_capacity(1 << 20, File::create(path)?);
        for p in &self.pieces {
            w.write_all(self.slice(p))?;
        }
        w.into_inner()?.sync_all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memchr::memmem;

    #[test]
    fn edits_match_model() {
        let mut model = Vec::new();
        let mut seed = 12345u64;
        let mut rnd = |m: usize| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as usize % m.max(1)
        };
        // > 8 MiB so block edges are exercised by scan/find.
        for i in 0..300_000 {
            model.extend(std::iter::repeat_n(b'a' + (i % 26) as u8, rnd(if i % 500 == 0 { 3000 } else { 40 })));
            model.extend_from_slice(if i % 3 == 0 { b"\r\n" } else { b"\n" });
        }
        let path = std::env::temp_dir().join("te_test.txt");
        std::fs::write(&path, &model).unwrap();
        let model_len_before_edits = model.len();
        let mut doc = open(path.clone()).join().unwrap();
        let re = Regex::new("zzQ").unwrap();

        let check = |doc: &Doc, model: &[u8], rnd: &mut dyn FnMut(usize) -> usize| {
            assert_eq!(doc.len(), model.len());
            assert_eq!(doc.bytes(0..model.len()), model);
            let starts: Vec<usize> = std::iter::once(0).chain(memchr_iter(b'\n', model).map(|i| i + 1)).collect();
            assert_eq!(doc.line_count(), starts.len());
            for _ in 0..50 {
                let n = rnd(starts.len());
                assert_eq!(doc.line_start(n), starts[n]);
                let off = rnd(model.len());
                assert_eq!(doc.line_of(off), starts.partition_point(|&s| s <= off) - 1);
            }
            let from = rnd(model.len());
            assert_eq!(doc.find(&re, from, true).map(|r| r.start), memmem::find(&model[from..], b"zzQ").map(|i| i + from));
            assert_eq!(doc.find(&re, from, false).map(|r| r.start), memmem::rfind(&model[..(from + 2).min(model.len())], b"zzQ"));
        };
        check(&doc, &model, &mut rnd);
        for round in 0..200 {
            let at = rnd(model.len());
            if rnd(2) == 0 {
                let text: &[u8] = [&b"zzQ"[..], b"\n", b"hello\nworld", b"x"][rnd(4)];
                doc.insert(at, text);
                model.splice(at..at, text.iter().copied());
            } else {
                let end = (at + rnd(5000)).min(model.len());
                doc.delete(at..end);
                model.drain(at..end);
            }
            check(&doc, &model, &mut rnd);
            if round % 50 == 0 {
                let mut all = Vec::new();
                doc.scan(&re, 0, |r| {
                    all.push(r.start);
                    true
                });
                assert_eq!(all, memmem::find_iter(&model, b"zzQ").collect::<Vec<_>>());
            }
        }

        // Anchors and word boundaries see the text before a block edge.
        let line = Regex::new(r"(?m)^hello").unwrap();
        let mut n = 0;
        doc.scan(&line, 0, |_| {
            n += 1;
            true
        });
        assert_eq!(n, model.split(|&c| c == b'\n').filter(|l| l.starts_with(b"hello")).count());

        // Empty matches advance instead of looping forever.
        let small = load(&path, &AtomicUsize::new(0)).unwrap();
        let mut empties = 0;
        small.scan(&Regex::new("").unwrap(), small.len() - 3, |_| {
            empties += 1;
            true
        });
        assert_eq!(empties, 4);

        // Small files are read, not mapped, so other programs can still replace them while open.
        assert!(!small.is_mapped() && !doc.is_mapped());
        std::fs::write(&path, b"replaced while open").unwrap();
        let tmp = path.with_extension("new");
        std::fs::write(&tmp, b"renamed over").unwrap();
        std::fs::rename(&tmp, &path).unwrap();
        assert_eq!(small.len(), model_len_before_edits);

        // A snapshot keeps its text when the original is typed into afterwards.
        let mut a = Doc::new();
        a.insert(0, b"abc");
        let b = a.clone();
        a.insert(3, b"d");
        assert_eq!((a.bytes(0..a.len()), b.bytes(0..b.len())), (b"abcd".to_vec(), b"abc".to_vec()));
        let _ = std::fs::remove_file(&path);
    }

    /// TE_BENCH=path cargo test --release bench -- --ignored --nocapture
    #[test]
    #[ignore]
    fn bench() {
        let t = std::time::Instant::now();
        let doc = open(std::env::var("TE_BENCH").unwrap().into()).join().unwrap();
        println!("indexed {} bytes, {} lines in {:?}", doc.len(), doc.line_count(), t.elapsed());
        let t = std::time::Instant::now();
        let mid = doc.line_start(doc.line_count() / 2);
        println!("line_start(mid)={mid} in {:?}", t.elapsed());
        let t = std::time::Instant::now();
        let last = doc.line_of(doc.len());
        for n in last.saturating_sub(40)..=last {
            let s = doc.line_start(n);
            let _ = doc.bytes(s..doc.line_end(n));
        }
        println!("render last 40 lines in {:?}", t.elapsed());
        for pat in ["user_id=4821 sit INFO lorem", r"(?i)error\s+\{", r"\b2026-10-27T21:00:00Z\b"] {
            let re = Regex::new(pat).unwrap();
            let t = std::time::Instant::now();
            let mut n = 0usize;
            doc.scan(&re, 0, |_| {
                n += 1;
                true
            });
            println!("scan {pat:?}: {n} matches in {:?}", t.elapsed());
        }
    }
}
