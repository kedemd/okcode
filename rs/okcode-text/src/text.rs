//! Port of src/analysis/text.js. JS strings are UTF-16, so every offset-bearing
//! function works on `&[u16]` to keep offsets identical to the JS ones.
use serde_json::{json, Value};

pub fn u16s(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}
pub fn from_u16(s: &[u16]) -> String {
    String::from_utf16_lossy(s)
}

/// Returns (text, lossless): lossless means re-encoding the decoded text reproduces `buf`.
pub fn decode(buf: &[u8]) -> (String, bool) {
    let text = String::from_utf8_lossy(buf).into_owned();
    let lossless = text.as_bytes() == buf;
    (text, lossless)
}

pub fn dominant_eol(src: &str) -> &'static str {
    let b = src.as_bytes();
    let crlf = b.windows(2).filter(|w| w == b"\r\n").count();
    let total_nl = b.iter().filter(|&&c| c == b'\n').count();
    let lf = total_nl - crlf;
    if crlf > lf {
        "\r\n"
    } else {
        "\n"
    }
}

pub fn normalise_eol(text: &str, eol: &str) -> String {
    let unified = text.replace("\r\n", "\n").replace('\r', "\n");
    if eol == "\r\n" {
        unified.replace('\n', "\r\n")
    } else {
        unified
    }
}

pub fn line_offsets(src: &[u16]) -> Vec<usize> {
    let mut o = vec![0];
    for (i, &c) in src.iter().enumerate() {
        if c == 10 {
            o.push(i + 1);
        }
    }
    o
}

pub fn line_range_to_offsets(src: &[u16], from: i64, to: i64) -> (usize, usize) {
    let offsets = line_offsets(src);
    let total = offsets.len() as i64;
    let f = 1.max(from.min(total));
    let t = f.max(to.min(total));
    let start = offsets[(f - 1) as usize];
    let mut end = if t < total { offsets[t as usize] } else { src.len() };
    if t < total {
        if end > 0 && src.get(end - 1) == Some(&10) {
            end -= 1;
        }
        if end > 0 && src.get(end - 1) == Some(&13) {
            end -= 1;
        }
    }
    (start, end)
}

pub fn line_col_at(src: &[u16], offset: i64) -> (usize, usize) {
    let (mut line, mut col) = (1, 1);
    let stop = offset.min(src.len() as i64).max(0) as usize;
    for &c in &src[..stop] {
        if c == 10 {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// `column` of None/0 behaves like JS `column || 1`.
pub fn offset_at_line_col(src: &[u16], line: i64, column: Option<i64>) -> usize {
    let mut idx = 0usize;
    let mut ln = 1i64;
    while ln < line && idx < src.len() {
        if src[idx] == 10 {
            ln += 1;
        }
        idx += 1;
    }
    let c = match column {
        None | Some(0) => 1,
        Some(c) => c,
    };
    src.len().min(idx + (c - 1).max(0) as usize)
}

pub fn line_start_of(src: &[u16], offset: i64) -> usize {
    let mut i = offset.max(0).min(src.len() as i64) as usize;
    while i > 0 && src[i - 1] != 10 {
        i -= 1;
    }
    i
}

pub fn line_end_of(src: &[u16], offset: i64) -> usize {
    let mut i = offset.max(0).min(src.len() as i64) as usize;
    while i < src.len() && src[i] != 10 {
        i += 1;
    }
    i
}

pub struct Widened {
    pub start: usize,
    pub end: usize,
    pub line_start: usize,
    pub line_end: usize,
}

pub fn widen_to_context(src: &[u16], start: i64, end: i64, context: i64) -> Widened {
    let mut lo = line_start_of(src, start);
    let mut n = 0;
    while n < context && lo > 0 {
        lo = line_start_of(src, lo as i64 - 1);
        n += 1;
    }
    let mut hi = line_end_of(src, (end - 1).max(start));
    n = 0;
    while n < context && hi < src.len() {
        hi = line_end_of(src, hi as i64 + 1);
        n += 1;
    }
    Widened {
        start: lo,
        end: hi,
        line_start: line_col_at(src, lo as i64).0,
        line_end: line_col_at(src, (hi as i64 - 1).max(lo as i64)).0,
    }
}

pub struct Edit {
    pub start: i64,
    pub end: i64,
    pub body: Vec<u16>,
    pub target: Option<String>,
}

pub fn hunks_for(before: &[u16], after: &[u16], edits: &[Edit], context: i64) -> Vec<Value> {
    let mut sorted: Vec<&Edit> = edits.iter().collect();
    sorted.sort_by_key(|e| e.start);
    let mut shift = 0i64;
    sorted
        .iter()
        .map(|e| {
            let (bs, be) = (e.start, e.end);
            let as_ = bs + shift;
            let ae = as_ + e.body.len() as i64;
            shift += e.body.len() as i64 - (be - bs);
            let b = widen_to_context(before, bs, be, context);
            let a = widen_to_context(after, as_, ae, context);
            let lines = |src: &[u16], w: &Widened, p: &str| -> Vec<String> {
                from_u16(&src[w.start..w.end]).split('\n').map(|l| format!("{p} {l}")).collect()
            };
            let removed = lines(before, &b, "-");
            let added = lines(after, &a, "+");
            let tgt = e.target.as_deref().filter(|t| !t.is_empty());
            let head = format!(
                "@@ {}lines {}-{} -> {}-{} @@",
                tgt.map(|t| format!("{t} ")).unwrap_or_default(),
                b.line_start,
                b.line_end,
                a.line_start,
                a.line_end
            );
            let mut all = vec![head];
            all.extend(removed);
            all.extend(added);
            json!({
                "target": tgt,
                "before": {"start": bs, "end": be, "lineStart": b.line_start, "lineEnd": b.line_end},
                "after": {"start": as_, "end": ae, "lineStart": a.line_start, "lineEnd": a.line_end},
                "text": all.join("\n"),
            })
        })
        .collect()
}
