//! Text tools behind the Encode / Format / Edit menus. Each one maps the selected bytes (or the
//! whole document) to new bytes, or explains why it can't.

use std::fmt::Write;

pub type Tool = fn(&[u8]) -> Result<Vec<u8>, String>;

fn text(b: &[u8]) -> Result<&str, String> {
    std::str::from_utf8(b.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(b)).map_err(|e| format!("not valid UTF-8 ({e})"))
}

fn ok(s: impl Into<String>) -> Result<Vec<u8>, String> {
    Ok(s.into().into_bytes())
}

// ---------------------------------------------------------------- encoders / decoders

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const B64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
const B32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

fn base64(b: &[u8], alpha: &[u8; 64], pad: bool) -> String {
    let mut out = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..=c.len() {
            out.push(alpha[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
        if pad {
            out.push_str(&"=="[..3 - c.len()]);
        }
    }
    out
}

/// Accepts both the standard and URL-safe alphabets, with or without padding and line breaks.
fn base64_decode(b: &[u8]) -> Result<Vec<u8>, String> {
    let (mut out, mut acc, mut bits) = (Vec::with_capacity(b.len() * 3 / 4), 0u32, 0);
    for &c in b {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b' ' | b'\t' | b'\r' | b'\n' => continue,
            _ => return Err(format!("not Base64: unexpected {:?}", c as char)),
        };
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

fn base32(b: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = String::new();
    for c in b.chunks(5) {
        let n = (0..5).fold(0u64, |n, i| n << 8 | *c.get(i).unwrap_or(&0) as u64);
        let used = (c.len() * 8).div_ceil(5);
        for i in 0..8 {
            out.push(if i < used { B32[(n >> (35 - 5 * i) & 31) as usize] as char } else { '=' });
        }
    }
    ok(out)
}

fn base32_decode(b: &[u8]) -> Result<Vec<u8>, String> {
    let (mut out, mut acc, mut bits) = (Vec::new(), 0u64, 0);
    for &c in b {
        let v = match c.to_ascii_uppercase() {
            c @ b'A'..=b'Z' => c - b'A',
            c @ b'2'..=b'7' => c - b'2' + 26,
            b'=' | b' ' | b'\t' | b'\r' | b'\n' => continue,
            _ => return Err(format!("not Base32: unexpected {:?}", c as char)),
        };
        acc = acc << 5 | v as u64;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

fn hex(b: &[u8]) -> String {
    b.iter().fold(String::with_capacity(b.len() * 2), |mut s, x| {
        let _ = write!(s, "{x:02x}");
        s
    })
}

/// Ignores whitespace, `0x` / `\x` prefixes and `:` `,` `-` separators.
fn hex_decode(b: &[u8]) -> Result<Vec<u8>, String> {
    let s = text(b)?.replace("0x", "").replace("0X", "").replace("\\x", "");
    let digits: Vec<u8> = s.bytes().filter(|c| !c.is_ascii_whitespace() && !b":,-".contains(c)).collect();
    if digits.len() % 2 == 1 {
        return Err("odd number of hex digits".into());
    }
    digits
        .chunks(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap_or("zz"), 16).map_err(|_| format!("not hex: {:?}", String::from_utf8_lossy(p))))
        .collect()
}

fn url_encode(b: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = String::with_capacity(b.len());
    for &c in b {
        if c.is_ascii_alphanumeric() || b"-_.~".contains(&c) {
            out.push(c as char);
        } else {
            let _ = write!(out, "%{c:02X}");
        }
    }
    ok(out)
}

fn url_decode(b: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() + 0 && b.len() > i + 2 => match u8::from_str_radix(&String::from_utf8_lossy(&b[i + 1..i + 3]), 16) {
                Ok(v) => {
                    out.push(v);
                    i += 3;
                    continue;
                }
                Err(_) => out.push(b'%'),
            },
            b'+' => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    Ok(out)
}

fn html_encode(b: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = String::with_capacity(b.len());
    for c in text(b)?.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    ok(out)
}

fn html_decode(b: &[u8]) -> Result<Vec<u8>, String> {
    const NAMED: &[(&str, &str)] = &[
        ("amp", "&"), ("lt", "<"), ("gt", ">"), ("quot", "\""), ("apos", "'"), ("nbsp", "\u{a0}"),
        ("copy", "©"), ("reg", "®"), ("trade", "™"), ("hellip", "…"), ("mdash", "—"), ("ndash", "–"),
        ("laquo", "«"), ("raquo", "»"), ("euro", "€"), ("pound", "£"), ("yen", "¥"), ("deg", "°"),
    ];
    let s = text(b)?;
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let entity = rest[1..].find(';').filter(|&j| j <= 10).map(|j| &rest[1..j + 1]);
        let decoded = entity.and_then(|e| {
            if let Some(h) = e.strip_prefix("#x").or_else(|| e.strip_prefix("#X")) {
                u32::from_str_radix(h, 16).ok().and_then(char::from_u32).map(String::from)
            } else if let Some(d) = e.strip_prefix('#') {
                d.parse().ok().and_then(char::from_u32).map(String::from)
            } else {
                NAMED.iter().find(|(n, _)| *n == e).map(|(_, v)| v.to_string())
            }
        });
        match (entity, decoded) {
            (Some(e), Some(d)) => {
                out.push_str(&d);
                rest = &rest[e.len() + 2..];
            }
            _ => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    ok(out)
}

/// Non-ASCII → \uXXXX (surrogate pairs above the BMP), as in JSON / JavaScript / Java.
fn unicode_escape(b: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = String::new();
    for c in text(b)?.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            for u in c.encode_utf16(&mut [0; 2]) {
                let _ = write!(out, "\\u{u:04x}");
            }
        }
    }
    ok(out)
}

fn string_escape(b: &[u8]) -> Result<Vec<u8>, String> {
    let s = serde_json::to_string(text(b)?).map_err(|e| e.to_string())?;
    ok(&s[1..s.len() - 1])
}

/// Undoes \n \t \r \" \\ \/ \' \0 \xHH \uXXXX (with surrogate pairs) and \u{...}; leaves anything else alone.
fn unescape(b: &[u8]) -> Result<Vec<u8>, String> {
    let s = text(b)?;
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    let hex_n = |it: &mut std::iter::Peekable<std::str::Chars>, n: usize| -> Option<u32> {
        let h: String = (0..n).map_while(|_| it.next_if(char::is_ascii_hexdigit)).collect();
        (h.len() == n).then(|| u32::from_str_radix(&h, 16).ok()).flatten()
    };
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some('b') => out.push('\u{8}'),
            Some('f') => out.push('\u{c}'),
            Some(c @ ('"' | '\\' | '/' | '\'')) => out.push(c),
            Some('x') => match hex_n(&mut it, 2) {
                Some(v) => out.push(v as u8 as char),
                None => out.push_str("\\x"),
            },
            Some('u') if it.peek() == Some(&'{') => {
                it.next();
                let h: String = it.by_ref().take_while(|&c| c != '}').collect();
                match u32::from_str_radix(&h, 16).ok().and_then(char::from_u32) {
                    Some(ch) => out.push(ch),
                    None => out.push_str(&format!("\\u{{{h}}}")),
                }
            }
            Some('u') => match hex_n(&mut it, 4) {
                Some(hi @ 0xD800..=0xDBFF) => {
                    let lo = (it.peek() == Some(&'\\'))
                        .then(|| {
                            let mut probe = it.clone();
                            probe.next();
                            (probe.next() == Some('u')).then(|| hex_n(&mut probe, 4)).flatten().filter(|lo| (0xDC00..=0xDFFF).contains(lo)).map(|lo| (lo, probe))
                        })
                        .flatten();
                    match lo {
                        Some((lo, probe)) => {
                            it = probe;
                            out.push(char::from_u32(0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)).unwrap_or('\u{fffd}'));
                        }
                        None => out.push('\u{fffd}'),
                    }
                }
                Some(v) => out.push(char::from_u32(v).unwrap_or('\u{fffd}')),
                None => out.push_str("\\u"),
            },
            Some(c) => {
                out.push('\\');
                out.push(c);
            }
            None => out.push('\\'),
        }
    }
    ok(out)
}

fn binary(b: &[u8]) -> Result<Vec<u8>, String> {
    ok(b.iter().map(|x| format!("{x:08b}")).collect::<Vec<_>>().join(" "))
}

fn binary_decode(b: &[u8]) -> Result<Vec<u8>, String> {
    let bits: Vec<u8> = b.iter().copied().filter(|c| !c.is_ascii_whitespace()).collect();
    if let Some(c) = bits.iter().find(|&&c| c != b'0' && c != b'1') {
        return Err(format!("not binary: unexpected {:?}", *c as char));
    }
    if bits.len() % 8 != 0 {
        return Err("bit count is not a multiple of 8".into());
    }
    Ok(bits.chunks(8).map(|g| g.iter().fold(0, |n, &c| n << 1 | (c - b'0'))).collect())
}

fn rot13(b: &[u8]) -> Result<Vec<u8>, String> {
    Ok(b.iter()
        .map(|&c| match c {
            b'a'..=b'z' => (c - b'a' + 13) % 26 + b'a',
            b'A'..=b'Z' => (c - b'A' + 13) % 26 + b'A',
            c => c,
        })
        .collect())
}

fn jwt_decode(b: &[u8]) -> Result<Vec<u8>, String> {
    let parts: Vec<&str> = text(b)?.trim().split('.').collect();
    if parts.len() < 2 {
        return Err("a JWT has the form header.payload.signature".into());
    }
    let part = |p: &str| -> Result<String, String> {
        let raw = base64_decode(p.as_bytes())?;
        Ok(String::from_utf8(json_pretty(&raw)?).unwrap_or_default())
    };
    ok(format!("// header\n{}\n\n// payload\n{}\n\n// signature (not verified)\n{}\n", part(parts[0])?, part(parts[1])?, parts.get(2).unwrap_or(&"")))
}

pub const ENCODE: &[(&str, Tool)] = &[
    ("Base64", |b| ok(base64(b, B64, true))),
    ("Base64 URL-safe", |b| ok(base64(b, B64URL, false))),
    ("Base32", base32),
    ("Hex", |b| ok(hex(b))),
    ("URL (percent)", url_encode),
    ("HTML entities", html_encode),
    ("Unicode \\uXXXX", unicode_escape),
    ("String escape (JSON/JS)", string_escape),
    ("Binary", binary),
    ("ROT13", rot13),
];

pub const DECODE: &[(&str, Tool)] = &[
    ("Base64 / Base64 URL", base64_decode),
    ("Base32", base32_decode),
    ("Hex", hex_decode),
    ("URL (percent)", url_decode),
    ("HTML entities", html_decode),
    ("Unescape \\n \\t \\uXXXX \\xHH", unescape),
    ("Binary", binary_decode),
    ("ROT13", rot13),
    ("JWT (header + payload)", jwt_decode),
];

fn crc32(b: &[u8]) -> String {
    let mut c = 0xFFFF_FFFFu32;
    for &x in b {
        c ^= x as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    format!("{:08x}", !c)
}

pub const HASH: &[(&str, fn(&[u8]) -> String)] = &[
    ("MD5", |b| hex(&<md5::Md5 as sha2::Digest>::digest(b))),
    ("SHA-1", |b| hex(&<sha1::Sha1 as sha2::Digest>::digest(b))),
    ("SHA-256", |b| hex(&<sha2::Sha256 as sha2::Digest>::digest(b))),
    ("SHA-512", |b| hex(&<sha2::Sha512 as sha2::Digest>::digest(b))),
    ("CRC32", crc32),
];

// ---------------------------------------------------------------- formatters

/// Re-indents JSON without parsing it into values, so numbers, key order and huge inputs survive.
fn json_layout(b: &[u8], indent: Option<&[u8]>) -> Result<Vec<u8>, String> {
    let b = b.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(b);
    serde_json::from_slice::<serde::de::IgnoredAny>(b).map_err(|e| format!("invalid JSON: {e}"))?;
    let mut out = Vec::with_capacity(b.len() + b.len() / 2);
    let (mut depth, mut in_str, mut esc) = (0usize, false, false);
    let newline = |out: &mut Vec<u8>, depth: usize| {
        if let Some(ind) = indent {
            out.push(b'\n');
            for _ in 0..depth {
                out.extend_from_slice(ind);
            }
        }
    };
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        i += 1;
        if in_str {
            out.push(c);
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
            continue;
        }
        match c {
            b'"' => {
                in_str = true;
                out.push(c);
            }
            b'{' | b'[' => {
                let j = i + b[i..].iter().take_while(|c| c.is_ascii_whitespace()).count();
                out.push(c);
                if b.get(j) == Some(&(c + 2)) {
                    out.push(c + 2); // `{}` / `[]` stay together
                    i = j + 1;
                } else {
                    depth += 1;
                    newline(&mut out, depth);
                }
            }
            b'}' | b']' => {
                depth -= 1;
                newline(&mut out, depth);
                out.push(c);
            }
            b',' => {
                out.push(c);
                newline(&mut out, depth);
            }
            b':' => {
                out.push(c);
                if indent.is_some() {
                    out.push(b' ');
                }
            }
            c if c.is_ascii_whitespace() => {}
            c => out.push(c),
        }
    }
    if indent.is_some() {
        out.push(b'\n');
    }
    Ok(out)
}

fn json_pretty(b: &[u8]) -> Result<Vec<u8>, String> {
    json_layout(b, Some(b"    "))
}

fn json_value(b: &[u8]) -> Result<serde_json::Value, String> {
    serde_json::from_str(text(b)?).map_err(|e| format!("invalid JSON: {e}"))
}

fn to_json(v: &serde_json::Value) -> Result<Vec<u8>, String> {
    json_pretty(&serde_json::to_vec(v).map_err(|e| e.to_string())?)
}

#[derive(Debug)]
enum Tok<'a> {
    Open(&'a str, String),
    Close(&'a str, String),
    Single(&'a str), // self-closing, void, comment, CDATA, <?...?>, <!DOCTYPE>
    Text(&'a str),
    Script(&'a str), // raw body of <script>/<style>/<pre>/<textarea>
}

const VOID: &[&str] = &["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr"];

fn tag_name(s: &str) -> String {
    s.chars().take_while(|c| !c.is_whitespace() && *c != '/' && *c != '>').collect::<String>().to_ascii_lowercase()
}

/// Tolerant tokenizer: never fails, so jammed or slightly broken markup still comes out readable.
fn markup_tokens(s: &str, html: bool) -> Vec<Tok<'_>> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'<' {
            let j = s[i..].find('<').map_or(s.len(), |k| i + k);
            if !s[i..j].trim().is_empty() {
                out.push(Tok::Text(&s[i..j]));
            }
            i = j;
            continue;
        }
        let rest = &s[i..];
        let until = |pat: &str, skip: usize| rest[skip..].find(pat).map_or(s.len(), |k| i + skip + k + pat.len());
        let j = if rest.starts_with("<!--") {
            until("-->", 4)
        } else if rest.starts_with("<![CDATA[") {
            until("]]>", 9)
        } else {
            let (mut k, mut quote) = (i + 1, 0u8);
            while k < b.len() {
                let c = b[k];
                if quote != 0 {
                    if c == quote {
                        quote = 0;
                    }
                } else if c == b'"' || c == b'\'' {
                    quote = c;
                } else if c == b'>' {
                    break;
                }
                k += 1;
            }
            (k + 1).min(b.len())
        };
        let tag = &s[i..j];
        i = j;
        if tag.starts_with("<!") || tag.starts_with("<?") {
            out.push(Tok::Single(tag));
        } else if let Some(n) = tag.strip_prefix("</") {
            out.push(Tok::Close(tag, tag_name(n)));
        } else {
            let name = tag_name(&tag[1..]);
            if tag.ends_with("/>") || (html && VOID.contains(&name.as_str())) {
                out.push(Tok::Single(tag));
            } else {
                if html && ["script", "style", "pre", "textarea"].contains(&name.as_str()) {
                    let close = format!("</{name}");
                    let k = b[i..].windows(close.len()).position(|w| w.eq_ignore_ascii_case(close.as_bytes())).map_or(s.len(), |k| i + k);
                    out.push(Tok::Open(tag, name));
                    if !s[i..k].trim().is_empty() {
                        out.push(Tok::Script(&s[i..k]));
                    }
                    i = k;
                    continue;
                }
                out.push(Tok::Open(tag, name));
            }
        }
    }
    out
}

fn markup(b: &[u8], html: bool, pretty: bool) -> Result<Vec<u8>, String> {
    let t = markup_tokens(text(b)?, html);
    let mut out = String::with_capacity(b.len() + b.len() / 4);
    let mut depth = 0usize;
    let line = |out: &mut String, depth: usize, x: &str| {
        if pretty {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&"    ".repeat(depth));
        }
        out.push_str(x);
    };
    let mut k = 0;
    while k < t.len() {
        match &t[k] {
            Tok::Open(tag, name) => {
                // <a>short text</a> and <a></a> stay on one line
                if let (Some(Tok::Text(x)), Some(Tok::Close(c, n))) = (t.get(k + 1), t.get(k + 2))
                    && n == name
                    && !x.trim().contains('\n')
                {
                    line(&mut out, depth, &format!("{tag}{}{c}", x.trim()));
                    k += 3;
                    continue;
                }
                if let Some(Tok::Close(c, n)) = t.get(k + 1)
                    && n == name
                {
                    line(&mut out, depth, &format!("{tag}{c}"));
                    k += 2;
                    continue;
                }
                line(&mut out, depth, tag);
                depth += 1;
            }
            Tok::Close(c, _) => {
                depth = depth.saturating_sub(1);
                line(&mut out, depth, c);
            }
            Tok::Single(x) => line(&mut out, depth, x),
            Tok::Text(x) if pretty => x.lines().map(str::trim).filter(|l| !l.is_empty()).for_each(|l| line(&mut out, depth, l)),
            Tok::Text(x) => out.push_str(&x.split_whitespace().collect::<Vec<_>>().join(" ")),
            Tok::Script(x) if pretty => x.lines().map(str::trim).filter(|l| !l.is_empty()).for_each(|l| line(&mut out, depth, l)),
            Tok::Script(x) => out.push_str(x), // collapsing JS/CSS whitespace could change its meaning
        }
        k += 1;
    }
    if pretty {
        out.push('\n');
    }
    ok(out)
}

/// Splits CSV/TSV, honouring quotes. The delimiter is guessed from the first line.
fn csv_rows(s: &str) -> Vec<Vec<String>> {
    let first = s.lines().next().unwrap_or("");
    let delim = [',', '\t', ';', '|'].into_iter().max_by_key(|d| first.matches(*d).count()).unwrap_or(',');
    let (mut rows, mut row, mut cell, mut quoted) = (Vec::new(), Vec::new(), String::new(), false);
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '"' if quoted && it.peek() == Some(&'"') => {
                cell.push('"');
                it.next();
            }
            '"' => quoted = !quoted,
            c if c == delim && !quoted => row.push(std::mem::take(&mut cell)),
            '\r' if !quoted => {}
            '\n' if !quoted => {
                row.push(std::mem::take(&mut cell));
                rows.push(std::mem::take(&mut row));
            }
            c => cell.push(c),
        }
    }
    if !cell.is_empty() || !row.is_empty() {
        row.push(cell);
        rows.push(row);
    }
    rows
}

fn csv_table(b: &[u8]) -> Result<Vec<u8>, String> {
    let rows = csv_rows(text(b)?);
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let width = |c: usize| rows.iter().filter_map(|r| r.get(c)).map(|s| s.chars().count()).max().unwrap_or(0);
    let widths: Vec<usize> = (0..cols).map(width).collect();
    let mut out = String::new();
    for (i, r) in rows.iter().enumerate() {
        let cells: Vec<String> = (0..cols).map(|c| format!("{:w$}", r.get(c).map_or("", |s| s.as_str()), w = widths[c])).collect();
        out.push_str(&format!("| {} |\n", cells.join(" | ")));
        if i == 0 {
            out.push_str(&format!("|{}|\n", widths.iter().map(|w| "-".repeat(w + 2)).collect::<Vec<_>>().join("|")));
        }
    }
    ok(out)
}

fn csv_to_json(b: &[u8]) -> Result<Vec<u8>, String> {
    let rows = csv_rows(text(b)?);
    let Some((head, body)) = rows.split_first() else { return Err("empty CSV".into()) };
    let objects: Vec<serde_json::Value> = body
        .iter()
        .map(|r| head.iter().enumerate().map(|(i, h)| (h.clone(), r.get(i).cloned().unwrap_or_default().into())).collect::<serde_json::Map<_, _>>().into())
        .collect();
    to_json(&serde_json::Value::Array(objects))
}

fn json_to_csv(b: &[u8]) -> Result<Vec<u8>, String> {
    let serde_json::Value::Array(items) = json_value(b)? else { return Err("expected a JSON array of objects".into()) };
    let mut head: Vec<String> = Vec::new();
    for it in &items {
        for k in it.as_object().ok_or("expected a JSON array of objects")?.keys() {
            if !head.contains(k) {
                head.push(k.clone());
            }
        }
    }
    let cell = |s: String| if s.contains([',', '"', '\n', '\r']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s };
    let mut out = head.iter().cloned().map(cell).collect::<Vec<_>>().join(",") + "\n";
    for it in &items {
        let row: Vec<String> = head
            .iter()
            .map(|k| match it.get(k) {
                None | Some(serde_json::Value::Null) => String::new(),
                Some(serde_json::Value::String(s)) => cell(s.clone()),
                Some(v) => cell(v.to_string()),
            })
            .collect();
        out += &(row.join(",") + "\n");
    }
    ok(out)
}

fn sql_format(b: &[u8]) -> Result<Vec<u8>, String> {
    let opts = sqlformat::FormatOptions { indent: sqlformat::Indent::Spaces(4), uppercase: Some(true), lines_between_queries: 1, ..Default::default() };
    ok(sqlformat::format(text(b)?, &sqlformat::QueryParams::None, &opts) + "\n")
}

pub const FORMAT: &[(&str, &[(&str, Tool)])] = &[
    ("JSON", &[
        ("Pretty print", json_pretty),
        ("Minify", |b| json_layout(b, None)),
        ("Convert to YAML", |b| serde_yaml::to_string(&json_value(b)?).map(String::into_bytes).map_err(|e| e.to_string())),
        ("Convert to TOML", |b| toml::to_string_pretty(&json_value(b)?).map(String::into_bytes).map_err(|e| format!("can't express as TOML: {e}"))),
        ("Convert to CSV", json_to_csv),
    ]),
    ("XML", &[("Pretty print", |b| markup(b, false, true)), ("Minify", |b| markup(b, false, false))]),
    ("HTML", &[("Pretty print", |b| markup(b, true, true)), ("Minify", |b| markup(b, true, false))]),
    ("YAML", &[
        ("Reformat (drops comments)", |b| {
            let v: serde_yaml::Value = serde_yaml::from_str(text(b)?).map_err(|e| format!("invalid YAML: {e}"))?;
            serde_yaml::to_string(&v).map(String::into_bytes).map_err(|e| e.to_string())
        }),
        ("Convert to JSON", |b| to_json(&serde_yaml::from_str(text(b)?).map_err(|e| format!("invalid YAML: {e}"))?)),
    ]),
    ("TOML", &[
        ("Reformat (drops comments)", |b| {
            let v: toml::Table = toml::from_str(text(b)?).map_err(|e| format!("invalid TOML: {e}"))?;
            toml::to_string_pretty(&v).map(String::into_bytes).map_err(|e| e.to_string())
        }),
        ("Convert to JSON", |b| to_json(&toml::from_str(text(b)?).map_err(|e| format!("invalid TOML: {e}"))?)),
    ]),
    ("SQL", &[("Format", sql_format)]),
    ("CSV / TSV", &[("Align as table", csv_table), ("Convert to JSON", csv_to_json)]),
];

// ---------------------------------------------------------------- line and case tools

/// Applies `f` to the lines, keeping the text's line ending style and trailing newline.
fn lines(b: &[u8], f: fn(Vec<&str>) -> Vec<String>) -> Result<Vec<u8>, String> {
    let s = text(b)?;
    let eol = if s.contains("\r\n") { "\r\n" } else { "\n" };
    let mut out = f(s.lines().collect()).join(eol);
    if s.ends_with('\n') {
        out.push_str(eol);
    }
    ok(out)
}

fn title_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut start = true;
    for c in s.chars() {
        if start {
            out.extend(c.to_uppercase());
        } else {
            out.extend(c.to_lowercase());
        }
        start = !c.is_alphanumeric() && c != '\'';
    }
    out
}

pub const CASE: &[(&str, Tool)] = &[
    ("UPPERCASE", |b| ok(text(b)?.to_uppercase())),
    ("lowercase", |b| ok(text(b)?.to_lowercase())),
    ("Title Case", |b| ok(title_case(text(b)?))),
];

pub const LINES: &[(&str, Tool)] = &[
    ("Sort ascending", |b| lines(b, |mut l| {
        l.sort_unstable();
        l.into_iter().map(String::from).collect()
    })),
    ("Sort descending", |b| lines(b, |mut l| {
        l.sort_unstable_by(|a, b| b.cmp(a));
        l.into_iter().map(String::from).collect()
    })),
    ("Remove duplicate lines", |b| lines(b, |l| {
        let mut seen = std::collections::HashSet::new();
        l.into_iter().filter(|x| seen.insert(*x)).map(String::from).collect()
    })),
    ("Remove empty lines", |b| lines(b, |l| l.into_iter().filter(|x| !x.trim().is_empty()).map(String::from).collect())),
    ("Trim trailing spaces", |b| lines(b, |l| l.into_iter().map(|x| x.trim_end().to_string()).collect())),
    ("Join lines", |b| lines(b, |l| vec![l.iter().map(|x| x.trim()).filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" ")])),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn run(t: Tool, s: &str) -> String {
        String::from_utf8(t(s.as_bytes()).unwrap()).unwrap()
    }

    #[test]
    fn tools() {
        let sample = "Hé llo/?&<>\"' 😀\n\tend";
        // every encoder round-trips through its decoder
        for (name, enc) in ENCODE {
            let dec = match *name {
                "Base64" | "Base64 URL-safe" => base64_decode as Tool,
                "Base32" => base32_decode,
                "Hex" => hex_decode,
                "URL (percent)" => url_decode,
                "HTML entities" => html_decode,
                "Unicode \\uXXXX" | "String escape (JSON/JS)" => unescape,
                "Binary" => binary_decode,
                _ => rot13,
            };
            assert_eq!(run(dec, &run(*enc, sample)), sample, "{name}");
        }
        assert_eq!(run(ENCODE[0].1, "hello"), "aGVsbG8=");
        assert_eq!(run(base32, "foobar"), "MZXW6YTBOI======");
        assert_eq!(run(html_decode, "&lt;a&gt; &#x1F600; &amp;amp &bogus;"), "<a> 😀 &amp &bogus;");
        assert_eq!(HASH[2].1(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(crc32(b"123456789"), "cbf43926");

        // JSON keeps big numbers and key order, and minify undoes pretty
        let j = r#"{"z":12345678901234567890123,"a":[1,{"s":"x,{y}\"q"}],"e":{},"l":[ ]}"#;
        let p = run(json_pretty, j);
        assert!(p.contains("12345678901234567890123") && p.find("\"z\"") < p.find("\"a\""), "{p}");
        assert_eq!(run(|b| json_layout(b, None), &p), r#"{"z":12345678901234567890123,"a":[1,{"s":"x,{y}\"q"}],"e":{},"l":[]}"#);
        assert!(json_pretty(b"{\"a\":}").is_err());

        let x = run(|b| markup(b, false, true), "<?xml version=\"1.0\"?><a><b x=\"1>2\">t</b><c/><d><e>1</e></d><!-- n --></a>");
        assert_eq!(x, "<?xml version=\"1.0\"?>\n<a>\n    <b x=\"1>2\">t</b>\n    <c/>\n    <d>\n        <e>1</e>\n    </d>\n    <!-- n -->\n</a>\n");
        let h = run(|b| markup(b, true, true), "<div><br><img src=a><script>if (a<b) x();</script></div>");
        assert_eq!(h, "<div>\n    <br>\n    <img src=a>\n    <script>\n        if (a<b) x();\n    </script>\n</div>\n");

        assert_eq!(run(csv_to_json, "a,b\n1,\"x,\"\"y\"\n"), "[\n    {\n        \"a\": \"1\",\n        \"b\": \"x,\\\"y\"\n    }\n]\n");
        assert_eq!(run(LINES[2].1, "b\r\na\r\nb\r\n"), "b\r\na\r\n");
    }
}
