//! Tag balance inside template literals (analysis/template-balance.js).
//! Works on UTF-16 units so every offset is the JS offset.
use crate::text::{is_js_ws, line_col_at};

const BACKTICK: u32 = 0x60;
const BACKSLASH: u32 = 0x5c;
const NONE: u32 = u32::MAX;

const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr",
];

#[derive(Debug, Clone)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub interpolations: Vec<(usize, usize)>,
}

#[derive(Debug, Clone)]
pub struct Diag {
    pub start: usize,
    pub end: usize,
    pub code: &'static str,
    pub message: String,
    pub line: usize,
    pub column: usize,
}

fn at(t: &[u16], i: usize) -> u32 {
    t.get(i).map_or(NONE, |&c| c as u32)
}
fn is_word(c: u32) -> bool {
    matches!(c, 0x30..=0x39 | 0x41..=0x5a | 0x61..=0x7a | 0x5f)
}
fn is_alpha(c: u32) -> bool {
    matches!(c, 0x41..=0x5a | 0x61..=0x7a)
}
fn index_of(t: &[u16], pat: &[u8], from: usize) -> Option<usize> {
    if pat.is_empty() || t.len() < pat.len() {
        return None;
    }
    (from..=t.len() - pat.len()).find(|&i| pat.iter().enumerate().all(|(k, &p)| t[i + k] == p as u16))
}
fn starts_with(t: &[u16], pat: &[u8], i: usize) -> bool {
    i + pat.len() <= t.len() && pat.iter().enumerate().all(|(k, &p)| t[i + k] == p as u16)
}

pub fn template_spans(text: &[u16]) -> Vec<Span> {
    let n = text.len();
    let mut spans = Vec::new();
    let mut i = 0;
    let mut quote: u32 = NONE;
    while i < n {
        let c = at(text, i);
        if quote != NONE {
            if c == BACKSLASH {
                i += 2;
                continue;
            }
            if c == quote {
                quote = NONE;
            }
            i += 1;
            continue;
        }
        if c == 0x2f && at(text, i + 1) == 0x2f {
            i = (i..n).find(|&k| text[k] == 10).unwrap_or(n);
            continue;
        }
        if c == 0x2f && at(text, i + 1) == 0x2a {
            i = match index_of(text, b"*/", i + 2) {
                Some(e) => e + 2,
                None => n,
            };
            continue;
        }
        if c == 0x27 || c == 0x22 {
            quote = c;
            i += 1;
            continue;
        }
        if c != BACKTICK {
            i += 1;
            continue;
        }
        let start = i + 1;
        let mut interpolations = Vec::new();
        let mut j = start;
        while j < n {
            let ch = at(text, j);
            if ch == BACKSLASH {
                j += 2;
                continue;
            }
            if ch == BACKTICK {
                break;
            }
            if ch == 0x24 && at(text, j + 1) == 0x7b {
                let from = j;
                j += 2;
                let mut depth = 1i32;
                let mut q = NONE;
                let mut tmpl = 0i32;
                while j < n && depth > 0 {
                    let d = at(text, j);
                    if d == BACKSLASH {
                        j += 2;
                        continue;
                    }
                    if q != NONE {
                        if d == q {
                            q = NONE;
                        }
                        j += 1;
                        continue;
                    }
                    if tmpl > 0 {
                        if d == BACKTICK {
                            tmpl -= 1;
                        }
                        j += 1;
                        continue;
                    }
                    if d == 0x27 || d == 0x22 {
                        q = d;
                        j += 1;
                        continue;
                    }
                    if d == BACKTICK {
                        tmpl += 1;
                        j += 1;
                        continue;
                    }
                    if d == 0x7b {
                        depth += 1;
                    } else if d == 0x7d {
                        depth -= 1;
                    }
                    j += 1;
                }
                interpolations.push((from, j));
                continue;
            }
            j += 1;
        }
        spans.push(Span { start, end: j.min(n), interpolations });
        i = j + 1;
    }
    // Only a body that actually contains an opening tag can be unbalanced.
    spans.into_iter().filter(|sp| sp.end > sp.start && has_open_tag(text, sp)).collect()
}

fn has_open_tag(text: &[u16], sp: &Span) -> bool {
    let mut body: Vec<u16> = Vec::new();
    let mut at_ = sp.start;
    for &(s, e) in &sp.interpolations {
        if s >= at_ && s <= text.len() {
            body.extend_from_slice(&text[at_..s]);
        }
        at_ = e;
    }
    if at_ < sp.end {
        body.extend_from_slice(&text[at_..sp.end]);
    }
    // /<[A-Za-z][\w-]*[\s/>]/
    for i in 0..body.len() {
        if body[i] != 0x3c || !is_alpha(at(&body, i + 1)) {
            continue;
        }
        let mut k = i + 2;
        while is_word(at(&body, k)) || at(&body, k) == 0x2d {
            k += 1;
        }
        let c = at(&body, k);
        if c != NONE && (is_js_ws(c) || c == 0x2f || c == 0x3e) {
            return true;
        }
    }
    false
}

struct Open {
    name: String,
    start: usize,
}

struct Raw {
    start: usize,
    end: usize,
    code: &'static str,
    message: String,
}

fn check_span(text: &[u16], span: &Span) -> Vec<Raw> {
    let mut out = Vec::new();
    let mut stack: Vec<Open> = Vec::new();
    let skip_to = |pos: usize| span.interpolations.iter().find(|x| pos >= x.0 && pos < x.1).map(|x| x.1);
    let mut i = span.start;
    while i < span.end {
        if let Some(jump) = skip_to(i) {
            i = jump;
            continue;
        }
        if text[i] != 0x3c {
            i += 1;
            continue;
        }
        if starts_with(text, b"<!--", i) {
            i = match index_of(text, b"-->", i + 4) {
                Some(e) if e <= span.end => e + 3,
                _ => span.end,
            };
            continue;
        }
        if starts_with(text, b"<!", i) {
            i = match (i..text.len()).find(|&k| text[k] == 0x3e) {
                Some(g) => g + 1,
                None => span.end,
            };
            continue;
        }
        // /^<(\/?)([A-Za-z][\w-]*)/ over text[i .. min(i+200, span.end)]
        let lim = (i + 200).min(span.end);
        let mut k = i + 1;
        let closing = k < lim && text[k] == 0x2f;
        if closing {
            k += 1;
        }
        if !(k < lim && is_alpha(text[k] as u32)) {
            i += 1;
            continue;
        }
        let name_start = k;
        k += 1;
        while k < lim && (is_word(text[k] as u32) || text[k] == 0x2d) {
            k += 1;
        }
        let name: String = text[name_start..k].iter().map(|&c| (c as u8 as char).to_ascii_lowercase()).collect();
        let mut j = k;
        let mut q = NONE;
        let mut self_closing = false;
        while j < span.end {
            let ch = text[j] as u32;
            if q != NONE {
                if ch == q {
                    q = NONE;
                }
                j += 1;
                continue;
            }
            if ch == 0x22 || ch == 0x27 {
                q = ch;
                j += 1;
                continue;
            }
            if ch == 0x3e {
                self_closing = j > 0 && text[j - 1] == 0x2f;
                break;
            }
            j += 1;
        }
        if j >= span.end {
            break;
        }
        if closing {
            match stack.last() {
                None => out.push(Raw {
                    start: i,
                    end: j + 1,
                    code: "UnexpectedClosingTag",
                    message: format!("</{name}> has no matching opening tag in this template"),
                }),
                Some(top) if top.name != name => {
                    out.push(Raw {
                        start: i,
                        end: j + 1,
                        code: "UnexpectedClosingTag",
                        message: format!(
                            "</{name}> closes <{}> \u{2014} the innermost open tag is <{}>, opened at line {}",
                            top.name,
                            top.name,
                            line_col_at(text, top.start).0
                        ),
                    });
                    match stack.iter().rposition(|t| t.name == name) {
                        Some(p) => stack.truncate(p),
                        None => {
                            stack.pop();
                        }
                    }
                }
                Some(_) => {
                    stack.pop();
                }
            }
        } else if !self_closing && !VOID.contains(&name.as_str()) {
            stack.push(Open { name, start: i });
        }
        i = j + 1;
    }
    for t in stack {
        out.push(Raw {
            start: t.start,
            end: t.start + t.name.len() + 1,
            code: "UnclosedTag",
            message: format!("<{}> is never closed in this template", t.name),
        });
    }
    out
}

/// check(text) -> diagnostics, absolute offsets, line/column attached.
pub fn check(text: &[u16]) -> Vec<Diag> {
    if !text.contains(&(BACKTICK as u16)) {
        return Vec::new();
    }
    let mut raw = Vec::new();
    for span in template_spans(text) {
        raw.extend(check_span(text, &span));
    }
    raw.sort_by_key(|d| d.start);
    raw.into_iter()
        .map(|d| {
            let (line, column) = line_col_at(text, d.start);
            Diag { start: d.start, end: d.end, code: d.code, message: d.message, line, column }
        })
        .collect()
}
