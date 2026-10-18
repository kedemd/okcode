//! Symbol-aware chunker and text outline (lib/analysis/chunk.js).
//! Offsets in `Chunk`/boundaries are UTF-16 code units, as in JS.
use regex::Regex;
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::sync::OnceLock;

pub const STRATEGY: &str = "okcode-symbols";
pub const PREPARER: &str = "okcode-content";
pub const MARK: &str = "//@ ";
pub const MIN_EMBED_CHARS: usize = 120;
pub const CHUNK: usize = 1400;
pub const CHUNK_MAX: usize = 2600;
pub const REGION_LINES: usize = 80;
pub const LOG_LINES: usize = 200;

#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub text: String,
    pub start: usize,
    pub end: usize,
}

// JS `\s` / String.prototype.trim whitespace (differs from Rust: includes U+FEFF, excludes U+0085).
fn is_js_space(c: char) -> bool {
    c != '\u{85}' && (c.is_whitespace() || c == '\u{FEFF}')
}
fn blank(s: &str) -> bool {
    s.chars().all(is_js_space)
}
fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_space)
}
fn is_line_term(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

const WS: &str = r"[[\s\x{FEFF}]--\x{85}]";

fn re(cell: &'static OnceLock<Regex>, pat: impl FnOnce() -> String) -> &'static Regex {
    cell.get_or_init(|| Regex::new(&pat()).unwrap())
}

/// byte offset -> UTF-16 offset
struct Units {
    map: Option<Vec<u32>>,
}
impl Units {
    fn new(text: &str) -> Self {
        if text.is_ascii() {
            return Units { map: None };
        }
        let mut map = vec![0u32; text.len() + 1];
        let mut u = 0u32;
        for (b, c) in text.char_indices() {
            for k in 0..c.len_utf8() {
                map[b + k] = u;
            }
            u += c.len_utf16() as u32;
        }
        map[text.len()] = u;
        Units { map: Some(map) }
    }
    fn at(&self, byte: usize) -> usize {
        match &self.map {
            None => byte,
            Some(m) => m[byte] as usize,
        }
    }
}

fn u16_len(s: &str) -> usize {
    if s.is_ascii() {
        s.len()
    } else {
        s.encode_utf16().count()
    }
}

fn ext_of(rel: &str) -> String {
    let l = rel.to_lowercase();
    match l.rfind('.') {
        Some(i) => {
            let rest = &l[i + 1..];
            if rest.is_empty() || rest.contains(['/', '\\', '.']) {
                String::new()
            } else {
                l[i..].to_string()
            }
        }
        None => String::new(),
    }
}

// ── boundaries ──────────────────────────────────────────────────────────────

fn parse_estree(text: &str, st: oxc_span::SourceType) -> Option<Value> {
    let alloc = oxc_allocator::Allocator::default();
    let ret = oxc_parser::Parser::new(&alloc, text, st).parse();
    if ret.diagnostics.has_errors() {
        return None;
    }
    let json = ret.program.to_estree_json(false, false);
    serde_json::from_str::<Value>(&json).ok()
}

/// Offsets where a new top-level thing begins; nested ones only for oversized nodes.
pub fn boundaries(text: &str) -> Option<Vec<usize>> {
    let mut ast = None;
    for st in [oxc_span::SourceType::script(), oxc_span::SourceType::mjs()] {
        if let Some(v) = parse_estree(text, st) {
            ast = Some(v);
            break;
        }
    }
    let ast = ast?;
    let units = Units::new(text);
    let conv = |v: &Value| v.as_u64().map(|n| units.at(n as usize));
    let mut out: BTreeSet<usize> = BTreeSet::new();
    out.insert(0);
    if let Some(body) = ast.get("body").and_then(|b| b.as_array()) {
        for node in body {
            let (Some(s), Some(e)) = (conv(&node["start"]), conv(&node["end"])) else { continue };
            out.insert(s);
            if e - s <= CHUNK_MAX {
                continue;
            }
            walk_inner(node, &mut out, 0, &conv);
        }
    }
    out.insert(u16_len(text));
    Some(out.into_iter().collect())
}

fn walk_inner(node: &Value, out: &mut BTreeSet<usize>, depth: u32, conv: &dyn Fn(&Value) -> Option<usize>) {
    if depth > 2 {
        return;
    }
    let Some(obj) = node.as_object() else { return };
    let is_fn = |v: &Value| v.get("type").and_then(|t| t.as_str()).map_or(false, |t| t.contains("Function"));
    for v in obj.values() {
        let kids: Vec<&Value> = match v {
            Value::Array(a) => a.iter().collect(),
            Value::Object(_) => vec![v],
            _ => vec![],
        };
        for kid in kids {
            let Some(ty) = kid.get("type").and_then(|t| t.as_str()) else { continue };
            let truthy = |k: &str| kid.get(k).map_or(false, |x| !x.is_null());
            let named = (ty == "FunctionDeclaration" && truthy("id"))
                || (ty == "Property" && kid.get("value").map_or(false, |x| is_fn(x)))
                || ty == "MethodDefinition"
                || (ty == "VariableDeclarator" && kid.get("init").map_or(false, |x| is_fn(x)));
            if named {
                if let Some(s) = conv(&kid["start"]) {
                    out.insert(s);
                }
            }
            walk_inner(kid, out, depth + named as u32, conv);
        }
    }
}

pub fn paragraph_boundaries(text: &str) -> Vec<usize> {
    static R: OnceLock<Regex> = OnceLock::new();
    let r = re(&R, || format!(r"\n{WS}*\n"));
    let u = Units::new(text);
    let mut out = vec![0];
    for m in r.find_iter(text) {
        out.push(u.at(m.end()));
    }
    out.push(u16_len(text));
    out
}

pub fn markdown_boundaries(text: &str) -> Vec<usize> {
    let mut set: BTreeSet<usize> = paragraph_boundaries(text).into_iter().collect();
    // /^#{1,6} .*$/gm : line starts follow any JS line terminator
    let mut u = 0usize;
    let mut at_start = true;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if at_start && c == '#' {
            // count leading #
            let rest: String = std::iter::once(c).chain(chars.clone().take_while(|x| *x == '#')).collect();
            let n = rest.chars().count();
            let after = chars.clone().nth(n - 1);
            if (1..=6).contains(&n) && after == Some(' ') {
                set.insert(u);
            }
        }
        at_start = is_line_term(c);
        u += c.len_utf16();
    }
    set.into_iter().collect()
}

pub fn html_boundaries(text: &str) -> Vec<usize> {
    static R1: OnceLock<Regex> = OnceLock::new();
    static R2: OnceLock<Regex> = OnceLock::new();
    let r1 = re(&R1, || r"\n[ \t]*<[a-zA-Z][^>]*>".to_string());
    let r2 = re(&R2, || r"(?is)<script\b[^>]*>(.*?)</script>".to_string());
    let u = Units::new(text);
    let mut out: BTreeSet<usize> = BTreeSet::new();
    out.insert(0);
    out.insert(u16_len(text));
    for m in r1.find_iter(text) {
        out.insert(u.at(m.start()) + 1);
    }
    for c in r2.captures_iter(text) {
        let m = c.get(0).unwrap();
        let inner = c.get(1).unwrap();
        let open_end = u.at(m.start() + m.as_str().find('>').unwrap() + 1);
        out.insert(u.at(m.start()));
        out.insert(open_end);
        out.insert(u.at(m.end()));
        if let Some(b) = boundaries(inner.as_str()) {
            for x in b {
                out.insert(open_end + x);
            }
        }
    }
    out.into_iter().collect()
}

pub fn json_boundaries(text: &str) -> Vec<usize> {
    let mut out: BTreeSet<usize> = BTreeSet::new();
    out.insert(0);
    out.insert(u16_len(text));
    let (mut depth, mut in_string, mut escaped) = (0i64, false, false);
    let mut i = 0usize;
    for ch in text.chars() {
        let w = ch.len_utf16();
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            i += w;
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' | '[' => {
                depth += 1;
                if depth <= 2 {
                    out.insert(i + 1);
                }
            }
            '}' | ']' => depth -= 1,
            ',' if depth <= 2 => {
                out.insert(i + 1);
            }
            _ => {}
        }
        i += w;
    }
    out.into_iter().collect()
}

pub fn boundaries_for(rel: &str, text: &str) -> Option<Vec<usize>> {
    let ext = ext_of(rel);
    match ext.as_str() {
        ".js" | ".mjs" | ".cjs" | ".jsx" => boundaries(text),
        ".html" | ".htm" | ".ok" | ".svg" | ".xml" => Some(html_boundaries(text)),
        ".json" | ".jsonc" => Some(json_boundaries(text)),
        ".md" | ".markdown" => Some(markdown_boundaries(text)),
        _ => None,
    }
}

fn utf16_str(t: &[u16], a: usize, b: usize) -> String {
    String::from_utf16_lossy(&t[a..b])
}

/// Greedily merge adjacent segments up to CHUNK, never exceeding CHUNK_MAX; hard-split an
/// indivisible oversized segment (at the last line break before the cap when there is one).
pub fn from_boundaries(text: &[u16], marks: &[usize]) -> Vec<Chunk> {
    let mut marks: Vec<usize> = marks.to_vec();
    let mut chunks = Vec::new();
    let mut i = 0;
    while i + 1 < marks.len() {
        let start = marks[i];
        let mut j = i + 1;
        while j < marks.len() - 1 && marks[j].saturating_sub(start) < CHUNK {
            j += 1;
        }
        let mut end = marks[j];
        if end.saturating_sub(start) > CHUNK_MAX && j > i + 1 {
            j = i + 1;
            end = marks[j];
        }
        if end.saturating_sub(start) > CHUNK_MAX {
            // `- 1`: the cut lands AFTER the newline.
            let from = (start + CHUNK_MAX - 1).min(text.len().saturating_sub(1));
            let nl = text.get(..=from).and_then(|s| s.iter().rposition(|&c| c == 10));
            let cut = match nl {
                Some(n) if n > start + CHUNK / 2 => n + 1,
                _ => start + CHUNK_MAX,
            };
            chunks.push(Chunk { text: utf16_str(text, start, cut), start, end: cut });
            marks.drain(..i);
            marks[0] = cut;
            i = 0;
            continue;
        }
        let slice = utf16_str(text, start, end);
        if !blank(&slice) {
            chunks.push(Chunk { text: slice, start, end });
        }
        i = j;
    }
    chunks
}

pub fn chunk_code(text: &str, rel: Option<&str>) -> Vec<Chunk> {
    if text.is_empty() {
        return vec![];
    }
    let marks = rel
        .and_then(|r| boundaries_for(r, text))
        .or_else(|| boundaries(text))
        .unwrap_or_else(|| paragraph_boundaries(text));
    let t: Vec<u16> = text.encode_utf16().collect();
    from_boundaries(&t, &marks)
}

// ── regions ─────────────────────────────────────────────────────────────────

const CODE_EXT: &[&str] =
    &[".js", ".mjs", ".cjs", ".jsx", ".ts", ".tsx", ".css", ".scss", ".sh", ".bash", ".ps1", ".sql"];

fn title_of(s: &str) -> String {
    let mut o = String::new();
    let mut w = false;
    for c in s.chars() {
        if is_js_space(c) {
            w = true;
        } else {
            if w {
                o.push(' ');
                w = false;
            }
            o.push(c);
        }
    }
    // a leading run of whitespace collapsed to nothing above (o was empty); trailing dropped
    let mut n = 0;
    let mut r = String::new();
    for c in js_trim(&o).chars() {
        n += c.len_utf16();
        if n > 100 {
            break;
        }
        r.push(c);
    }
    r
}

struct Lines<'a> {
    lines: Vec<&'a str>,
    last: usize,
}
impl<'a> Lines<'a> {
    fn new(text: &'a str) -> Self {
        let lines: Vec<&str> = text.split('\n').collect();
        let mut last = lines.len();
        while last > 1 && blank(lines[last - 1]) {
            last -= 1;
        }
        Lines { lines, last }
    }
    fn line(&self, n: usize) -> &str {
        self.lines.get(n - 1).copied().unwrap_or("")
    }
    fn tight(&self, a: usize, mut b: usize) -> usize {
        while b > a && blank(self.line(b)) {
            b -= 1;
        }
        b
    }
    fn first_line(&self, a: usize, b: usize) -> &str {
        for i in a..=b {
            if !blank(self.line(i)) {
                return self.line(i);
            }
        }
        ""
    }
}

struct Head {
    line: usize,
    level: usize,
    title: String,
}

fn heading_regions(l: &Lines, heads: &[Head]) -> Option<Vec<Value>> {
    if heads.is_empty() {
        return None;
    }
    let mut out = vec![];
    if heads[0].line > 1 {
        let mut a = 1;
        while a < heads[0].line && blank(l.line(a)) {
            a += 1;
        }
        if a < heads[0].line {
            let b = l.tight(a, heads[0].line - 1);
            out.push(json!({"kind":"section","title":title_of(l.line(a)),"lineStart":a,"lineEnd":b}));
        }
    }
    for (i, h) in heads.iter().enumerate() {
        let next = heads[i + 1..].iter().find(|o| o.level <= h.level);
        let b = l.tight(h.line, next.map_or(l.last, |n| n.line - 1));
        out.push(json!({"kind":"heading","title":h.title,"level":h.level,"lineStart":h.line,"lineEnd":b}));
    }
    Some(out)
}

fn markdown_heads(l: &Lines) -> Vec<Head> {
    let mut heads = vec![];
    let mut fence: Option<char> = None;
    for (i, line) in l.lines.iter().enumerate() {
        let cs: Vec<char> = line.chars().collect();
        let mut p = 0;
        while p < cs.len() && p < 3 && is_js_space(cs[p]) {
            p += 1;
        }
        if p < cs.len() && (cs[p] == '`' || cs[p] == '~') {
            let c = cs[p];
            let run = cs[p..].iter().take_while(|x| **x == c).count();
            if run >= 3 {
                match fence {
                    None => fence = Some(c),
                    Some(f) if f == c => fence = None,
                    _ => {}
                }
                continue;
            }
        }
        if fence.is_some() {
            continue;
        }
        // /^\s{0,3}(#{1,6})\s+(.*?)\s*#*\s*$/
        let n = cs[p..].iter().take_while(|x| **x == '#').count();
        if !(1..=6).contains(&n) {
            continue;
        }
        let mut s = p + n;
        if s >= cs.len() || !is_js_space(cs[s]) {
            continue;
        }
        while s < cs.len() && is_js_space(cs[s]) {
            s += 1;
        }
        // smallest q such that cs[q..] is ws* #* ws*
        let mut q = cs.len();
        while q > s && is_js_space(cs[q - 1]) {
            q -= 1;
        }
        while q > s && cs[q - 1] == '#' {
            q -= 1;
        }
        while q > s && is_js_space(cs[q - 1]) {
            q -= 1;
        }
        if q == s || cs[s..q].iter().any(|c| is_line_term(*c)) {
            continue;
        }
        let t: String = cs[s..q].iter().collect();
        heads.push(Head { line: i + 1, level: n, title: title_of(&t) });
    }
    heads
}

fn html_heads(l: &Lines) -> Vec<Head> {
    static R: OnceLock<Regex> = OnceLock::new();
    static TAG: OnceLock<Regex> = OnceLock::new();
    let r = re(&R, || r"(?i)<h([1-6])(?-u:\b)[^>]*>".to_string());
    let tag = re(&TAG, || r"<[^>]*>".to_string());
    let mut heads = vec![];
    for (i, line) in l.lines.iter().enumerate() {
        let mut pos = 0;
        while let Some(m) = r.find_at(line, pos) {
            let level = line.as_bytes()[m.start() + 2] - b'0';
            let rest = &line[m.end()..];
            let close = format!("</h{level}>");
            let lower = rest.to_ascii_lowercase();
            let first_term = rest.char_indices().find(|(_, c)| is_line_term(*c)).map(|(b, _)| b);
            let found = lower.find(&close);
            let body = match (found, first_term) {
                (Some(f), t) if t.map_or(true, |t| f <= t) => Some(&rest[..f]),
                (_, None) => Some(rest),
                _ => None,
            };
            if let Some(body) = body {
                let title = title_of(&tag.replace_all(body, ""));
                if !title.is_empty() {
                    heads.push(Head { line: i + 1, level: level as usize, title });
                }
                break;
            }
            pos = m.start() + 1;
            while !line.is_char_boundary(pos) {
                pos += 1;
            }
        }
    }
    heads
}

fn json_key(text: &[char], i: usize) -> Option<String> {
    // text.slice(i, i + 400) then /^"((?:[^"\\]|\\.)*)"/
    let mut units = 0;
    let mut k = i;
    let mut win = vec![];
    while k < text.len() {
        units += text[k].len_utf16();
        if units > 400 {
            break;
        }
        win.push(text[k]);
        k += 1;
    }
    if win.first() != Some(&'"') {
        return None;
    }
    let mut key = String::new();
    let mut p = 1;
    while p < win.len() {
        match win[p] {
            '"' => return Some(key),
            '\\' => {
                let n = *win.get(p + 1)?;
                if is_line_term(n) {
                    return None;
                }
                key.push('\\');
                key.push(n);
                p += 2;
            }
            c => {
                key.push(c);
                p += 1;
            }
        }
    }
    None
}

fn json_regions(text: &str, l: &Lines, max_lines: usize) -> Option<Vec<Value>> {
    let chars: Vec<char> = text.chars().collect();
    let mut starts: Vec<(usize, Option<String>)> = vec![];
    let (mut depth, mut in_string, mut escaped) = (0i64, false, false);
    let mut top: Option<char> = None;
    let mut line = 1usize;
    let mut expect = false;
    for (i, &ch) in chars.iter().enumerate() {
        if ch == '\n' {
            line += 1;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if is_js_space(ch) {
            continue;
        }
        if expect && depth == 1 && ch != '}' && ch != ']' {
            let key = if top == Some('{') && ch == '"' { json_key(&chars, i) } else { None };
            starts.push((line, key));
        }
        expect = false;
        if ch == '"' {
            in_string = true;
        } else if ch == '{' || ch == '[' {
            depth += 1;
            if depth == 1 {
                top = Some(ch);
                expect = true;
            }
        } else if ch == '}' || ch == ']' {
            depth -= 1;
        } else if ch == ',' && depth == 1 {
            expect = true;
        }
    }
    let top = top?;
    if starts.len() < 2 || starts.last().unwrap().0 == starts[0].0 {
        return None;
    }
    struct Span {
        key: Option<String>,
        index: usize,
        ls: usize,
        le: usize,
    }
    let spans: Vec<Span> = starts
        .iter()
        .enumerate()
        .map(|(i, (ln, key))| {
            let end = if i + 1 < starts.len() {
                (*ln).max(starts[i + 1].0 - 1)
            } else {
                (*ln).max(l.last - 1)
            };
            Span { key: key.clone(), index: i, ls: *ln, le: l.tight(*ln, end) }
        })
        .collect();
    if top == '{' {
        return Some(
            spans
                .iter()
                .map(|s| {
                    let t = title_of(&s.key.clone().unwrap_or_else(|| format!("[{}]", s.index)));
                    json!({"kind":"member","title":t,"lineStart":s.ls,"lineEnd":s.le})
                })
                .collect(),
        );
    }
    // (from, to, lineStart, lineEnd)
    let mut out: Vec<(usize, usize, usize, usize)> = vec![];
    for s in &spans {
        match out.last_mut() {
            Some(prior) if s.le + 1 <= max_lines + prior.2 => {
                prior.3 = s.le;
                prior.1 = s.index;
            }
            _ => out.push((s.index, s.index, s.ls, s.le)),
        }
    }
    Some(
        out.into_iter()
            .map(|(f, t, a, b)| {
                let title = if f == t { format!("[{f}]") } else { format!("[{f}]\u{2013}[{t}]") };
                json!({"kind":"member","title":title,"lineStart":a,"lineEnd":b})
            })
            .collect(),
    )
}

fn page_regions(l: &Lines) -> Option<Vec<Value>> {
    let mut out: Vec<Value> = vec![];
    let mut a = 1usize;
    let nb = |s: &str| !blank(&s.replace('\u{c}', ""));
    let push = |out: &mut Vec<Value>, a: usize, b: usize| {
        let mut s = a;
        while s < b && !nb(l.line(s)) {
            s += 1;
        }
        let b = l.tight(s, b);
        if (s..=b).any(|n| nb(l.line(n))) {
            let title = format!("page {}", out.len() + 1);
            out.push(json!({"kind":"page","title":title,"lineStart":s,"lineEnd":b}));
        }
    };
    for i in 2..=l.last {
        if !l.line(i).contains('\u{c}') {
            continue;
        }
        push(&mut out, a, i - 1);
        a = i;
    }
    push(&mut out, a, l.last);
    if out.len() > 1 {
        Some(out)
    } else {
        None
    }
}

fn paragraph_regions(l: &Lines, max_lines: usize, log_lines: usize) -> Vec<Value> {
    let mut blocks = vec![];
    let mut a: Option<usize> = None;
    for i in 1..=l.last + 1 {
        let bl = i > l.last || blank(l.line(i));
        if !bl && a.is_none() {
            a = Some(i);
        }
        if bl {
            if let Some(s) = a.take() {
                blocks.push((s, i - 1));
            }
        }
    }
    let mut out = vec![];
    let mut run: Option<(usize, usize)> = None;
    let close = |out: &mut Vec<Value>, run: &mut Option<(usize, usize)>| {
        if let Some((s, e)) = run.take() {
            out.push(json!({"kind":"paragraph","title":title_of(l.first_line(s, e)),"lineStart":s,"lineEnd":e}));
        }
    };
    for (s, e) in blocks {
        if e - s + 1 > max_lines {
            close(&mut out, &mut run);
            let mut x = s;
            while x <= e {
                let y = e.min(x + log_lines - 1);
                out.push(json!({"kind":"lines","title":title_of(l.first_line(x, y)),"lineStart":x,"lineEnd":y}));
                x += log_lines;
            }
            continue;
        }
        match run.as_mut() {
            Some(r) if e - r.0 + 1 <= max_lines => r.1 = e,
            _ => {
                close(&mut out, &mut run);
                run = Some((s, e));
            }
        }
    }
    close(&mut out, &mut run);
    out
}

/// None is JS `null` (code files: their outline is their symbols).
pub fn regions(rel: &str, text: &str, max_lines: i64, log_lines: i64) -> Option<Value> {
    let ext = ext_of(rel);
    if CODE_EXT.contains(&ext.as_str()) {
        return None;
    }
    if blank(text) {
        return Some(json!([]));
    }
    let l = Lines::new(text);
    let max = max_lines.max(1) as usize;
    let e = ext.as_str();
    if matches!(e, ".md" | ".markdown") {
        if let Some(r) = heading_regions(&l, &markdown_heads(&l)) {
            return Some(Value::Array(r));
        }
    }
    if matches!(e, ".html" | ".htm" | ".xml" | ".svg" | ".ok") {
        if let Some(r) = heading_regions(&l, &html_heads(&l)) {
            return Some(Value::Array(r));
        }
    }
    if matches!(e, ".json" | ".jsonc") {
        if let Some(r) = json_regions(text, &l, max) {
            return Some(Value::Array(r));
        }
    }
    if text.contains('\u{c}') {
        if let Some(r) = page_regions(&l) {
            return Some(Value::Array(r));
        }
    }
    Some(Value::Array(paragraph_regions(&l, max, log_lines.max(1) as usize)))
}

/// The heading region a title names | {ambiguous: [...]} | null.
pub fn section_of(rel: &str, text: &str, title: &str) -> Value {
    let all: Vec<Value> = match regions(rel, text, REGION_LINES as i64, LOG_LINES as i64) {
        Some(Value::Array(a)) => a.into_iter().filter(|r| r["kind"] == "heading").collect(),
        _ => vec![],
    };
    if all.is_empty() {
        return Value::Null;
    }
    // .replace(/^\s*#+\s*/, '')
    let t = title.trim_start_matches(is_js_space);
    let stripped = if t.starts_with('#') { t.trim_start_matches('#').trim_start_matches(is_js_space) } else { title };
    let want = title_of(stripped);
    let by = |f: &dyn Fn(&str) -> bool| -> Vec<&Value> { all.iter().filter(|r| f(r["title"].as_str().unwrap_or(""))).collect() };
    let mut hits = by(&|t| t == want);
    if hits.is_empty() {
        let w = want.to_lowercase();
        hits = by(&|t| t.to_lowercase() == w);
    }
    match hits.len() {
        0 => Value::Null,
        1 => hits[0].clone(),
        _ => {
            let mut m = Map::new();
            m.insert("ambiguous".into(), Value::Array(hits.into_iter().cloned().collect()));
            Value::Object(m)
        }
    }
}

// ── the embedding text ──────────────────────────────────────────────────────

/// What okdb embeds for one file: the path stamped on the front, then the text.
pub fn prepare_text(rel: &str, text: Option<&str>, indexed: bool) -> String {
    match text {
        Some(t) if indexed && u16_len(t) >= MIN_EMBED_CHARS => format!("{MARK}{rel}\n{t}"),
        _ => String::new(),
    }
}

pub fn mark_length(rel: &str) -> usize {
    MARK.len() + u16_len(rel) + 1
}
