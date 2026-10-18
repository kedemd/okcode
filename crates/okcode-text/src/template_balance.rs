//! Template balance (lib/analysis/template-balance.js). Works on UTF-16 units so offsets match JS.
use crate::text::{from_u16, line_col_at};

const BACKTICK: u16 = b'`' as u16;
const BACKSLASH: u16 = b'\\' as u16;

pub const VOID: [&str; 14] =
    ["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr"];

#[derive(Debug, Clone)]
pub struct Interp {
    pub start: usize,
    pub end: usize,
}
#[derive(Debug, Clone)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub interpolations: Vec<Interp>,
}
#[derive(Debug, Clone)]
pub struct Diag {
    pub start: usize,
    pub end: usize,
    pub code: &'static str,
    pub name: String,
    pub message: String,
    pub line: usize,
    pub column: usize,
}

fn ch(c: char) -> u16 {
    c as u16
}
fn is_alpha(c: u16) -> bool {
    (c >= 65 && c <= 90) || (c >= 97 && c <= 122)
}
fn is_name(c: u16) -> bool {
    is_alpha(c) || (c >= 48 && c <= 57) || c == 95 || c == 45
}
fn is_js_ws(c: u16) -> bool {
    matches!(c, 9..=13 | 32 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
}
fn starts_with(t: &[u16], pat: &str, at: usize) -> bool {
    let p: Vec<u16> = pat.encode_utf16().collect();
    t.len() >= at + p.len() && t[at..at + p.len()] == p[..]
}
fn index_of(t: &[u16], pat: &str, from: usize) -> Option<usize> {
    let p: Vec<u16> = pat.encode_utf16().collect();
    if from > t.len() {
        return None;
    }
    (from..t.len().saturating_sub(p.len().saturating_sub(1))).find(|&i| t[i..i + p.len()] == p[..])
}

pub fn template_spans(text: &[u16]) -> Vec<Span> {
    let n = text.len();
    let at = |i: usize| text.get(i).copied();
    let mut spans = Vec::new();
    let mut i = 0;
    let mut quote: Option<u16> = None;
    while i < n {
        let c = text[i];
        if let Some(q) = quote {
            if c == BACKSLASH {
                i += 2;
                continue;
            }
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if c == ch('/') && at(i + 1) == Some(ch('/')) {
            i = index_of(text, "\n", i).unwrap_or(n);
            continue;
        }
        if c == ch('/') && at(i + 1) == Some(ch('*')) {
            i = match index_of(text, "*/", i + 2) {
                Some(e) => e + 2,
                None => n,
            };
            continue;
        }
        if c == ch('\'') || c == ch('"') {
            quote = Some(c);
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
            let c2 = text[j];
            if c2 == BACKSLASH {
                j += 2;
                continue;
            }
            if c2 == BACKTICK {
                break;
            }
            if c2 == ch('$') && at(j + 1) == Some(ch('{')) {
                let from = j;
                j += 2;
                let mut depth = 1;
                let mut q: Option<u16> = None;
                let mut tmpl = 0;
                while j < n && depth > 0 {
                    let d = text[j];
                    if d == BACKSLASH {
                        j += 2;
                        continue;
                    }
                    if let Some(qq) = q {
                        if d == qq {
                            q = None;
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
                    if d == ch('\'') || d == ch('"') {
                        q = Some(d);
                        j += 1;
                        continue;
                    }
                    if d == BACKTICK {
                        tmpl += 1;
                        j += 1;
                        continue;
                    }
                    if d == ch('{') {
                        depth += 1;
                    } else if d == ch('}') {
                        depth -= 1;
                    }
                    j += 1;
                }
                interpolations.push(Interp { start: from, end: j });
                continue;
            }
            j += 1;
        }
        spans.push(Span { start, end: j.min(n), interpolations });
        i = j + 1;
    }
    spans
        .into_iter()
        .filter(|sp| {
            if sp.end <= sp.start {
                return false;
            }
            let mut body: Vec<u16> = Vec::new();
            let mut a = sp.start;
            for it in &sp.interpolations {
                body.extend_from_slice(&text[a.min(n)..it.start.min(n)]);
                a = it.end;
            }
            if a < sp.end {
                body.extend_from_slice(&text[a..sp.end]);
            }
            // /<[A-Za-z][\w-]*[\s/>]/
            (0..body.len()).any(|k| {
                if body[k] != ch('<') || !body.get(k + 1).map_or(false, |&c| is_alpha(c)) {
                    return false;
                }
                let mut m = k + 2;
                while m < body.len() && is_name(body[m]) {
                    m += 1;
                }
                body.get(m).map_or(false, |&c| is_js_ws(c) || c == ch('/') || c == ch('>'))
            })
        })
        .collect()
}

struct Open {
    name: String,
    start: usize,
}

fn check_span(text: &[u16], span: &Span) -> Vec<(usize, usize, &'static str, String, String)> {
    let mut out = Vec::new();
    let mut stack: Vec<Open> = Vec::new();
    let mut i = span.start;
    while i < span.end {
        if let Some(it) = span.interpolations.iter().find(|x| i >= x.start && i < x.end) {
            i = it.end;
            continue;
        }
        if text[i] != ch('<') {
            i += 1;
            continue;
        }
        if starts_with(text, "<!--", i) {
            i = match index_of(text, "-->", i + 4) {
                Some(e) if e <= span.end => e + 3,
                _ => span.end,
            };
            continue;
        }
        if starts_with(text, "<!", i) {
            i = match index_of(text, ">", i) {
                Some(g) => g + 1,
                None => span.end,
            };
            continue;
        }
        // ^<(\/?)([A-Za-z][\w-]*) over text[i .. min(i+200, span.end)]
        let lim = (i + 200).min(span.end);
        let mut p = i + 1;
        let closing = p < lim && text[p] == ch('/');
        if closing {
            p += 1;
        }
        if !(p < lim && is_alpha(text[p])) {
            i += 1;
            continue;
        }
        let ns = p;
        p += 1;
        while p < lim && is_name(text[p]) {
            p += 1;
        }
        let name = from_u16(&text[ns..p]).to_ascii_lowercase();
        let mut j = p;
        let mut q: Option<u16> = None;
        let mut self_closing = false;
        while j < span.end {
            let c = text[j];
            if let Some(qq) = q {
                if c == qq {
                    q = None;
                }
                j += 1;
                continue;
            }
            if c == ch('"') || c == ch('\'') {
                q = Some(c);
                j += 1;
                continue;
            }
            if c == ch('>') {
                self_closing = text[j - 1] == ch('/');
                break;
            }
            j += 1;
        }
        if j >= span.end {
            break;
        }
        if closing {
            match stack.last() {
                None => out.push((
                    i,
                    j + 1,
                    "UnexpectedClosingTag",
                    name.clone(),
                    format!("</{name}> has no matching opening tag in this template"),
                )),
                Some(top) if top.name != name => {
                    out.push((
                        i,
                        j + 1,
                        "UnexpectedClosingTag",
                        name.clone(),
                        format!(
                            "</{name}> closes <{}> — the innermost open tag is <{}>, opened at line {}",
                            top.name,
                            top.name,
                            line_col_at(text, top.start as i64).0
                        ),
                    ));
                    match stack.iter().rposition(|t| t.name == name) {
                        Some(k) => stack.truncate(k),
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
        out.push((
            t.start,
            t.start + t.name.encode_utf16().count() + 1,
            "UnclosedTag",
            t.name.clone(),
            format!("<{}> is never closed in this template", t.name),
        ));
    }
    out
}

pub fn check(text: &[u16]) -> Vec<Diag> {
    if !text.contains(&BACKTICK) {
        return vec![];
    }
    let mut all = Vec::new();
    for span in template_spans(text) {
        all.extend(check_span(text, &span));
    }
    all.sort_by_key(|d| d.0);
    all.into_iter()
        .map(|(start, end, code, name, message)| {
            let (line, column) = line_col_at(text, start as i64);
            Diag { start, end, code, name, message, line, column }
        })
        .collect()
}
