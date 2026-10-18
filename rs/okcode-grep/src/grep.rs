//! Pure half of grep and glob (port of src/grep.js). Columns are UTF-16 based, as in JS.
use fancy_regex::Regex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::LazyLock;

static UPPER_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\p{Lu}").unwrap());
static WORD_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\p{L}\p{N}_]+").unwrap());

// ── JS regex source → fancy-regex ───────────────────────────────────────

fn hex(chars: &[char], i: usize, n: usize) -> Option<u32> {
    if i + n > chars.len() {
        return None;
    }
    let s: String = chars[i..i + n].iter().collect();
    if s.chars().all(|c| c.is_ascii_hexdigit()) {
        u32::from_str_radix(&s, 16).ok()
    } else {
        None
    }
}

fn is_quant(chars: &[char], i: usize) -> bool {
    // {n}, {n,}, {n,m} starting at chars[i]=='{'
    let mut j = i + 1;
    let d0 = j;
    while j < chars.len() && chars[j].is_ascii_digit() {
        j += 1;
    }
    if j == d0 {
        return false;
    }
    if j < chars.len() && chars[j] == ',' {
        j += 1;
        while j < chars.len() && chars[j].is_ascii_digit() {
            j += 1;
        }
    }
    j < chars.len() && chars[j] == '}'
}

/// Translate JS RegExp syntax to something fancy-regex accepts, where it is cheap.
fn translate(src: &str, ignore_case: bool) -> String {
    let c: Vec<char> = src.chars().collect();
    let mut out = String::new();
    if ignore_case {
        out.push_str("(?i)");
    }
    let mut i = 0;
    let mut in_class = false;
    while i < c.len() {
        let ch = c[i];
        if ch == '\\' && i + 1 < c.len() {
            let e = c[i + 1];
            i += 2;
            match e {
                'd' => out.push_str(if in_class { "0-9" } else { "[0-9]" }),
                'w' => out.push_str(if in_class { "0-9A-Za-z_" } else { "[0-9A-Za-z_]" }),
                'D' if !in_class => out.push_str("[^0-9]"),
                'W' if !in_class => out.push_str("[^0-9A-Za-z_]"),
                'b' if in_class => out.push_str("\\x08"),
                'b' => out.push_str("(?:(?<=[0-9A-Za-z_])(?![0-9A-Za-z_])|(?<![0-9A-Za-z_])(?=[0-9A-Za-z_]))"),
                'B' if !in_class => out.push_str("(?:(?<=[0-9A-Za-z_])(?=[0-9A-Za-z_])|(?<![0-9A-Za-z_])(?![0-9A-Za-z_]))"),
                'u' => {
                    if i < c.len() && c[i] == '{' {
                        if let Some(end) = c[i..].iter().position(|&x| x == '}') {
                            let h: String = c[i + 1..i + end].iter().collect();
                            out.push_str(&format!("\\x{{{h}}}"));
                            i += end + 1;
                            continue;
                        }
                    }
                    if let Some(mut v) = hex(&c, i, 4) {
                        i += 4;
                        if (0xD800..0xDC00).contains(&v) && i + 1 < c.len() && c[i] == '\\' && c[i + 1] == 'u' {
                            if let Some(lo) = hex(&c, i + 2, 4) {
                                if (0xDC00..0xE000).contains(&lo) {
                                    v = 0x10000 + ((v - 0xD800) << 10) + (lo - 0xDC00);
                                    i += 6;
                                }
                            }
                        }
                        if char::from_u32(v).is_some() {
                            out.push_str(&format!("\\x{{{v:X}}}"));
                        } else {
                            out.push_str("\\x{FFFD}");
                        }
                    } else {
                        out.push('u');
                    }
                }
                'c' if i < c.len() && c[i].is_ascii_alphabetic() => {
                    out.push_str(&format!("\\x{{{:X}}}", (c[i] as u32) % 32));
                    i += 1;
                }
                '0' if !(i < c.len() && c[i].is_ascii_digit()) => out.push_str("\\x00"),
                'p' | 'P' if i < c.len() && c[i] == '{' && c[i..].contains(&'}') => {
                    let end = c[i..].iter().position(|&x| x == '}').unwrap();
                    out.push('\\');
                    out.push(e);
                    out.extend(&c[i..=i + end]);
                    i += end + 1;
                }
                'n' | 'r' | 't' | 'f' | 'v' | 'x' | 'p' | 'P' | 's' | 'S' | 'D' | 'W' | 'B' | 'k' => {
                    out.push('\\');
                    out.push(e);
                }
                '1'..='9' if !in_class => {
                    out.push('\\');
                    out.push(e);
                }
                _ if e.is_ascii_alphanumeric() => out.push(e),
                _ => {
                    out.push('\\');
                    out.push(e);
                }
            }
            continue;
        }
        if in_class {
            match ch {
                ']' => {
                    in_class = false;
                    out.push(']');
                }
                '[' => out.push_str("\\["),
                '&' => out.push_str("\\&"),
                '~' => out.push_str("\\~"),
                '-' if i + 1 < c.len() && c[i + 1] == '-' => out.push_str("\\-"),
                _ => out.push(ch),
            }
            i += 1;
            continue;
        }
        match ch {
            '[' => {
                if c.get(i + 1) == Some(&'^') && c.get(i + 2) == Some(&']') {
                    out.push_str("[\\s\\S]");
                    i += 3;
                } else if c.get(i + 1) == Some(&']') {
                    out.push_str("(?!)");
                    i += 2;
                } else {
                    in_class = true;
                    out.push('[');
                    i += 1;
                    if c.get(i) == Some(&'^') {
                        out.push('^');
                        i += 1;
                    }
                    // a leading ']' can't occur here (handled above)
                }
            }
            '.' => {
                out.push_str("[^\\n\\r\\u{2028}\\u{2029}]".replace("\\u{", "\\x{").as_str());
                i += 1;
            }
            '{' if !is_quant(&c, i) => {
                out.push_str("\\{");
                i += 1;
            }
            '}' => {
                // a closing brace of a quantifier was consumed with its '{'? no: copy through
                out.push('}');
                i += 1;
            }
            _ => {
                out.push(ch);
                i += 1;
            }
        }
    }
    out
}

/// V8's structural errors, found before translation: balance of groups/classes and dangling quantifiers.
fn precheck(src: &str) -> Option<&'static str> {
    let c: Vec<char> = src.chars().collect();
    let (mut depth, mut in_class, mut atom, mut lazy_ok) = (0usize, false, false, false);
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        if ch == '\\' {
            let e = c.get(i + 1).copied();
            i += 2;
            if !in_class {
                atom = !matches!(e, Some('b') | Some('B'));
                lazy_ok = false;
            }
            continue;
        }
        if in_class {
            if ch == ']' {
                in_class = false;
                atom = true;
            }
            i += 1;
            continue;
        }
        let quant = match ch {
            '*' | '+' => true,
            '?' if lazy_ok => {
                lazy_ok = false;
                i += 1;
                continue;
            }
            '?' => true,
            '{' => is_quant(&c, i),
            _ => false,
        };
        if quant {
            if !atom {
                return Some("Nothing to repeat");
            }
            atom = false;
            lazy_ok = true;
            if ch == '{' {
                while c[i] != '}' {
                    i += 1;
                }
            }
            i += 1;
            continue;
        }
        lazy_ok = false;
        match ch {
            '[' => in_class = true,
            '(' => {
                depth += 1;
                atom = false;
                i += 1;
                if c.get(i) == Some(&'?') {
                    i += 1;
                    match (c.get(i), c.get(i + 1)) {
                        (Some('<'), Some('=')) | (Some('<'), Some('!')) => i += 2,
                        (Some('<'), _) => {
                            while i < c.len() && c[i] != '>' {
                                i += 1;
                            }
                            i += 1;
                        }
                        _ => i += 1,
                    }
                }
                continue;
            }
            ')' => {
                if depth == 0 {
                    return Some("Unmatched ')'");
                }
                depth -= 1;
                atom = true;
            }
            '|' => atom = false,
            '^' | '$' => atom = false,
            _ => atom = true,
        }
        i += 1;
    }
    if in_class {
        Some("Unterminated character class")
    } else if depth > 0 {
        Some("Unterminated group")
    } else {
        None
    }
}

fn reason(msg: &str) -> String {
    let m = msg.to_lowercase();
    if m.contains("unclosed group") || m.contains("unclosed parenthesis") || m.contains("opening parenthesis") {
        "Unterminated group".into()
    } else if m.contains("unclosed character class") || m.contains("unclosed class") {
        "Unterminated character class".into()
    } else if m.contains("unopened group") || m.contains("unmatched closing") || m.contains("unopened parenthesis") {
        "Unmatched ')'".into()
    } else if m.contains("repetition") || m.contains("target of repeat") || m.contains("nothing to repeat") {
        "Nothing to repeat".into()
    } else {
        msg.to_string()
    }
}

fn compile_js(src: &str, ignore_case: bool) -> Result<Regex, String> {
    if let Some(m) = precheck(src) {
        return Err(m.into());
    }
    Regex::new(&translate(src, ignore_case)).map_err(|e| reason(&e.to_string()))
}

// ── globs ───────────────────────────────────────────────────────────────

fn escape_re(c: char) -> String {
    if "\\^$.|?*+()[]{}/-".contains(c) {
        format!("\\{c}")
    } else {
        c.to_string()
    }
}

fn glob_body(glob: &[char]) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    let mut i = 0;
    while i < glob.len() {
        let c = glob[i];
        if c == '*' {
            if glob.get(i + 1) == Some(&'*') {
                let at_start = i == 0 || glob[i - 1] == '/';
                let next = glob.get(i + 2).copied();
                i += 1;
                if at_start && next == Some('/') {
                    out.push_str("(?:[^/]*/)*");
                    i += 1;
                } else if at_start && next.is_none() {
                    out.push_str(".*");
                } else {
                    out.push_str("[^/]*");
                }
            } else {
                out.push_str("[^/]*");
            }
        } else if c == '?' {
            out.push_str("[^/]");
        } else if c == '[' {
            let end = if i + 2 <= glob.len() { glob[i + 2..].iter().position(|&x| x == ']').map(|p| p + i + 2) } else { None };
            match end {
                None => out.push_str("\\["),
                Some(end) => {
                    let mut cls: String = glob[i + 1..end].iter().collect();
                    if cls.starts_with('!') {
                        cls = format!("^{}", &cls[1..]);
                    }
                    out.push('[');
                    out.push_str(&cls.replace('\\', "\\\\"));
                    out.push(']');
                    i = end;
                }
            }
        } else if c == '{' {
            depth += 1;
            out.push_str("(?:");
        } else if c == '}' && depth > 0 {
            depth -= 1;
            out.push(')');
        } else if c == ',' && depth > 0 {
            out.push('|');
        } else if c == '\\' && i + 1 < glob.len() {
            i += 1;
            out.push_str(&escape_re(glob[i]));
        } else {
            out.push_str(&escape_re(c));
        }
        i += 1;
    }
    while depth > 0 {
        depth -= 1;
        out.push(')');
    }
    out
}

/// RegExp.prototype.source escaping: a '/' outside a class becomes '\/'.
fn js_source(s: &str) -> String {
    let mut out = String::new();
    let mut in_class = false;
    let mut esc = false;
    for c in s.chars() {
        if esc {
            esc = false;
            out.push(c);
            continue;
        }
        match c {
            '\\' => {
                esc = true;
                out.push(c);
            }
            '[' => {
                in_class = true;
                out.push(c);
            }
            ']' => {
                in_class = false;
                out.push(c);
            }
            '/' if !in_class => out.push_str("\\/"),
            _ => out.push(c),
        }
    }
    out
}

/// The regex SOURCE (JS spelling) of a glob.
pub fn glob_to_regexp_source(glob: &str) -> String {
    let mut g = glob.trim().replace('\\', "/");
    if let Some(r) = g.strip_prefix("./") {
        g = r.to_string();
    }
    g = g.trim_start_matches('/').to_string();
    let trailing_dir = g.ends_with('/');
    g = g.trim_end_matches('/').to_string();
    if g.is_empty() {
        return "^.*$".into();
    }
    let anchored = g.contains('/');
    let head = if anchored { "^" } else { "^(?:.*/)?" };
    let chars: Vec<char> = g.chars().collect();
    js_source(&format!("{head}{}{}$", glob_body(&chars), if trailing_dir { "(?=/)" } else { "" }))
}

pub struct GlobRe {
    pub source: String,
    re: Regex,
}

impl GlobRe {
    pub fn is_match(&self, s: &str) -> bool {
        self.re.is_match(s).unwrap_or(false)
    }
}

pub fn glob_to_regexp(glob: &str) -> Result<GlobRe, String> {
    let source = glob_to_regexp_source(glob);
    let re = compile_js(&source, false).map_err(|e| format!("Invalid regular expression: /{source}/: {e}"))?;
    Ok(GlobRe { source, re })
}

fn prefixes_of(rel: &str) -> Vec<&str> {
    let mut out = vec![];
    for (i, b) in rel.bytes().enumerate() {
        if b == b'/' {
            out.push(&rel[..i]);
        }
    }
    out.push(rel);
    out
}

pub struct PathMatcher {
    roots: Vec<String>,
    any_root: bool,
    inc: Vec<GlobRe>,
    exc: Vec<GlobRe>,
}

impl PathMatcher {
    fn hits(re: &GlobRe, rel: &str) -> bool {
        prefixes_of(rel).iter().any(|p| re.is_match(p))
    }
    pub fn matches(&self, rel: &str) -> bool {
        if !self.roots.is_empty()
            && !self.any_root
            && !self.roots.iter().any(|r| rel == r || (rel.starts_with(r.as_str()) && rel[r.len()..].starts_with('/')))
        {
            return false;
        }
        if !self.inc.is_empty() && !self.inc.iter().any(|re| Self::hits(re, rel)) {
            return false;
        }
        if self.exc.iter().any(|re| Self::hits(re, rel)) {
            return false;
        }
        true
    }
}

/// None when nothing filters.
pub fn path_matcher(glob: &[String], paths: &[String]) -> Result<Option<PathMatcher>, String> {
    let mut inc = vec![];
    let mut exc = vec![];
    for raw in glob {
        let g = raw.trim();
        if g.is_empty() {
            continue;
        }
        if let Some(rest) = g.strip_prefix('!') {
            exc.push(glob_to_regexp(rest.trim_end_matches('/'))?);
        } else {
            inc.push(glob_to_regexp(g.trim_end_matches('/'))?);
        }
    }
    let mut roots: Vec<String> = vec![];
    for p in paths {
        let mut s = p.replace('\\', "/");
        if s == "." {
            s.clear();
        } else if let Some(r) = s.strip_prefix("./") {
            s = r.to_string();
        }
        let s = s.trim_start_matches('/').trim_end_matches('/').to_string();
        if !roots.contains(&s) {
            roots.push(s);
        }
    }
    let any_root = roots.iter().any(|r| r.is_empty());
    if inc.is_empty() && exc.is_empty() && (roots.is_empty() || any_root) {
        return Ok(None);
    }
    Ok(Some(PathMatcher { roots, any_root, inc, exc }))
}

// ── the line matcher ────────────────────────────────────────────────────

fn has_meta(p: &str) -> bool {
    p.chars().any(|c| "\\^$.|?*+()[]{}".contains(c))
}

fn escape_literal(s: &str) -> String {
    s.chars().map(escape_re).collect()
}

pub enum CaseMode {
    Sensitive(bool),
    Smart,
}

fn resolve_case(pattern: &str, cs: &CaseMode) -> bool {
    match cs {
        CaseMode::Smart => UPPER_RE.is_match(pattern).unwrap_or(false),
        CaseMode::Sensitive(b) => *b,
    }
}

pub struct Matcher {
    pub literal: Option<String>,
    pub ignore_case: bool,
    pub regex: bool,
    pub source: String,
    re: Regex,
}

fn utf16_col(s: &str, byte: usize) -> usize {
    s[..byte].encode_utf16().count()
}

fn find_ascii_ci(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()].eq_ignore_ascii_case(needle))
}

/// Err carries the GREP_BAD_PATTERN message.
pub fn compile_matcher(pattern: &str, regex: bool, cs: &CaseMode) -> Result<Matcher, String> {
    if pattern.is_empty() {
        return Err("an empty pattern matches every line — give the text to look for".into());
    }
    if pattern.contains('\n') {
        return Err("a pattern matches within ONE line — search for one line of it".into());
    }
    let ignore_case = !resolve_case(pattern, cs);
    let literal = if !regex || !has_meta(pattern) { Some(pattern.to_string()) } else { None };
    build(literal, ignore_case, pattern)
}

/// Rebuild a matcher from its public fields (what a golden/caller passes around).
pub fn matcher_from_parts(literal: Option<String>, ignore_case: bool, source: &str) -> Result<Matcher, String> {
    build(literal, ignore_case, source)
}

fn build(literal: Option<String>, ignore_case: bool, p: &str) -> Result<Matcher, String> {
    let re = match &literal {
        Some(l) => Regex::new(&format!("{}{}", if ignore_case { "(?i)" } else { "" }, escape_literal(l)))
            .map_err(|e| e.to_string())?,
        None => compile_js(p, ignore_case).map_err(|e| {
            format!("invalid regular expression: Invalid regular expression: /{p}/{}: {e}", if ignore_case { "i" } else { "" })
        })?,
    };
    Ok(Matcher { regex: literal.is_none(), literal, ignore_case, source: p.to_string(), re })
}

impl Matcher {
    /// Column (0-based, UTF-16) of the first match, or -1.
    pub fn test(&self, line: &str) -> i64 {
        if let Some(l) = &self.literal {
            if !self.ignore_case {
                return line.find(l.as_str()).map_or(-1, |b| utf16_col(line, b) as i64);
            }
            if l.is_ascii() {
                return find_ascii_ci(line.as_bytes(), l.as_bytes()).map_or(-1, |b| utf16_col(line, b) as i64);
            }
        }
        match self.re.find(line) {
            Ok(Some(m)) => utf16_col(line, m.start()) as i64,
            _ => -1,
        }
    }

    /// Every non-empty matched string of a line, left to right (rg -o).
    pub fn all(&self, line: &str) -> Vec<String> {
        let mut out = vec![];
        for m in self.re.find_iter(line).flatten() {
            if !m.as_str().is_empty() {
                out.push(m.as_str().to_string());
            }
        }
        out
    }
}

const LINE_CAP: usize = 300;

/// A line as shown: a minified 20 kB line is cut to a window around the match.
pub fn clip(line: &str, col: usize) -> String {
    let u: Vec<u16> = line.encode_utf16().collect();
    if u.len() <= LINE_CAP {
        return line.to_string();
    }
    let from = (col as i64 - 80).min(u.len() as i64 - LINE_CAP as i64).max(0) as usize;
    let end = (from + LINE_CAP).min(u.len());
    format!("{}{}…", if from > 0 { "…" } else { "" }, String::from_utf16_lossy(&u[from..end]))
}

pub struct MatchOpts {
    pub before: usize,
    pub after: usize,
    pub max_per_file: usize,
    pub skip: usize,
}

impl Default for MatchOpts {
    fn default() -> Self {
        MatchOpts { before: 0, after: 0, max_per_file: usize::MAX, skip: 0 }
    }
}

pub struct Match {
    pub line: usize,
    pub col: usize,
    pub text: String,
    pub before: Vec<String>,
    pub after: Vec<String>,
    pub n: usize,
}

fn split_lines(text: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.len() > 1 && lines[lines.len() - 1].is_empty() {
        lines.pop();
    }
    lines
}

fn strip(l: &str) -> &str {
    l.strip_suffix('\r').unwrap_or(l)
}

/// Every matching line of one text, with context: (matches, count of matching lines).
pub fn match_text(text: &str, matcher: &Matcher, o: &MatchOpts) -> (Vec<Match>, usize) {
    let lines = split_lines(text);
    let mut out = vec![];
    let mut count = 0;
    for i in 0..lines.len() {
        let line = strip(lines[i]);
        let col = matcher.test(line);
        if col < 0 {
            continue;
        }
        let n = count;
        count += 1;
        if n < o.skip || out.len() >= o.max_per_file {
            continue;
        }
        let b = (i.saturating_sub(o.before)..i).map(|j| clip(strip(lines[j]), 0)).collect();
        let hi = (lines.len() - 1).min(i.saturating_add(o.after));
        let a = (i + 1..=hi).map(|j| clip(strip(lines[j]), 0)).collect();
        out.push(Match { line: i + 1, col: col as usize + 1, text: clip(line, col as usize), before: b, after: a, n });
    }
    (out, count)
}

#[derive(Clone, Debug, Default)]
pub struct Tally {
    pub count: usize,
    pub lines: usize,
    pub files: usize,
    pub first: Option<String>,
    pub last_file: Option<String>,
}

/// rg -o | sort | uniq -c over one text; returns the number of matching lines.
pub fn tally_text(text: &str, matcher: &Matcher, tally: &mut Vec<(String, Tally)>, file: Option<&str>) -> usize {
    let mut index: HashMap<String, usize> = tally.iter().enumerate().map(|(i, (k, _))| (k.clone(), i)).collect();
    let mut count = 0;
    for raw in split_lines(text) {
        let raw = strip(raw);
        if matcher.test(raw) < 0 {
            continue;
        }
        let found = matcher.all(raw);
        if found.is_empty() {
            continue;
        }
        count += 1;
        let mut seen: Vec<&str> = vec![];
        for m in &found {
            let idx = *index.entry(m.clone()).or_insert_with(|| {
                tally.push((m.clone(), Tally { first: file.map(str::to_string), ..Default::default() }));
                tally.len() - 1
            });
            let t = &mut tally[idx].1;
            t.count += 1;
            if !seen.contains(&m.as_str()) {
                seen.push(m);
                t.lines += 1;
            }
            if t.last_file.as_deref() != file {
                t.last_file = file.map(str::to_string);
                t.files += 1;
            }
        }
    }
    count
}

// ── index pre-filter rules ──────────────────────────────────────────────

/// Terms every file containing `literal` must have (see the JS comment in src/grep.js).
pub fn index_terms(literal: &str, stopwords: &[String], min_len: usize, max_len: usize) -> Vec<String> {
    if literal.is_empty() {
        return vec![];
    }
    let stop: Vec<String> = stopwords.iter().map(|s| s.to_lowercase()).collect();
    let mut out: Vec<String> = vec![];
    for m in WORD_RE.find_iter(literal).flatten() {
        let run = m.as_str();
        let left_bounded = m.start() > 0;
        let right_bounded = m.end() < literal.len();
        if !left_bounded || !run.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            continue;
        }
        let lower = run.to_lowercase();
        let t: String = lower.chars().take(max_len).collect();
        if t.len() < min_len || stop.contains(&t) {
            continue;
        }
        if !right_bounded && stop.iter().any(|s| *s != t && s.starts_with(&t)) {
            continue;
        }
        if !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

fn js_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "null".into(),
        Value::Array(a) => a.iter().map(|x| if x.is_null() { String::new() } else { js_string(x) }).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
        other => other.to_string(),
    }
}

/// A list argument: an array, or one string with commas (split outside braces). None when empty.
pub fn split_list(v: &Value) -> Option<Vec<String>> {
    let items: Vec<String> = match v {
        Value::Null => return None,
        Value::String(s) if s.is_empty() => return None,
        Value::Bool(true) => return None,
        Value::Array(a) => a.iter().map(js_string).collect(),
        other => vec![js_string(other)],
    };
    let mut out = vec![];
    for s in items {
        let mut depth = 0;
        let mut cur = String::new();
        for c in s.chars() {
            if c == '{' {
                depth += 1;
            } else if c == '}' && depth > 0 {
                depth -= 1;
            }
            if c == ',' && depth == 0 {
                out.push(std::mem::take(&mut cur));
            } else {
                cur.push(c);
            }
        }
        out.push(cur);
    }
    let clean: Vec<String> = out.into_iter().map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
    if clean.is_empty() {
        None
    } else {
        Some(clean)
    }
}

// ── golden adapter ──────────────────────────────────────────────────────

fn str_list(v: Option<&Value>) -> Vec<String> {
    match v {
        None | Some(Value::Null) => vec![],
        Some(Value::Array(a)) => a.iter().map(js_string).collect(),
        Some(o) => vec![js_string(o)],
    }
}

fn glob_list(v: Option<&Value>) -> Vec<String> {
    match v {
        None | Some(Value::Null) => vec![],
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect(),
        Some(Value::String(s)) => vec![s.clone()],
        _ => vec![],
    }
}

fn path_list(v: Option<&Value>) -> Vec<String> {
    match v {
        None | Some(Value::Null) => vec![],
        Some(Value::Array(a)) => a.iter().map(|x| if x.is_null() { String::new() } else { js_string(x) }).collect(),
        Some(o) => vec![js_string(o)],
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Number(n) => n.as_f64().map_or(true, |f| f != 0.0),
        _ => true,
    }
}

fn matcher_arg(v: &Value) -> Result<Matcher, String> {
    let literal = v.get("literal").and_then(|x| x.as_str()).map(str::to_string);
    let ic = v.get("ignoreCase").map_or(false, truthy);
    let src = v.get("source").and_then(|x| x.as_str()).unwrap_or("");
    matcher_from_parts(literal, ic, src)
}

fn num(v: Option<&Value>) -> Option<usize> {
    v.and_then(|x| x.as_f64()).map(|f| if f < 0.0 { 0 } else { f as usize })
}

pub fn golden_call(name: &str, args: &[Value]) -> Result<Value, String> {
    let name = name.strip_prefix("grep.").unwrap_or(name);
    let null = Value::Null;
    let arg = |i: usize| args.get(i).unwrap_or(&null);
    match name {
        "globToRegExp" => {
            let g = glob_to_regexp(&js_string(arg(0)))?;
            Ok(json!({ "$re": g.source, "flags": "" }))
        }
        "pathMatcher" => {
            let o = arg(0);
            let m = path_matcher(&glob_list(o.get("glob")), &path_list(o.get("paths")))?;
            Ok(if m.is_some() { json!("[function]") } else { Value::Null })
        }
        "compileMatcher" => {
            let pat = if arg(0).is_null() { String::new() } else { js_string(arg(0)) };
            let o = arg(1);
            let regex = o.get("regex").map_or(false, truthy);
            let cs = match o.get("caseSensitive") {
                Some(Value::String(s)) if s == "smart" => CaseMode::Smart,
                Some(v) => CaseMode::Sensitive(truthy(v)),
                None => CaseMode::Sensitive(false),
            };
            let m = compile_matcher(&pat, regex, &cs)?;
            Ok(json!({ "literal": m.literal, "ignoreCase": m.ignore_case, "regex": m.regex, "source": m.source }))
        }
        "matchText" => {
            let m = matcher_arg(arg(1))?;
            let o = arg(2);
            let ctx = num(o.get("context")).unwrap_or(0);
            let opts = MatchOpts {
                // JS default applies to undefined only: an explicit null means 0 lines.
                before: if o.get("before").map_or(false, Value::is_null) { 0 } else { num(o.get("before")).unwrap_or(ctx) },
                after: if o.get("after").map_or(false, Value::is_null) { 0 } else { num(o.get("after")).unwrap_or(ctx) },
                max_per_file: num(o.get("maxPerFile")).unwrap_or(usize::MAX),
                skip: num(o.get("skip")).unwrap_or(0),
            };
            let (ms, count) = match_text(&js_string(arg(0)), &m, &opts);
            let arr: Vec<Value> = ms
                .into_iter()
                .map(|x| json!({ "line": x.line, "col": x.col, "text": x.text, "before": x.before, "after": x.after, "n": x.n }))
                .collect();
            Ok(json!({ "matches": arr, "count": count }))
        }
        "tallyText" => {
            let m = matcher_arg(arg(1))?;
            let mut tally: Vec<(String, Tally)> = vec![];
            if let Some(Value::Array(entries)) = arg(2).get("$map") {
                for e in entries {
                    if let (Some(k), Some(t)) = (e.get(0).and_then(|k| k.as_str()), e.get(1)) {
                        tally.push((
                            k.to_string(),
                            Tally {
                                count: num(t.get("count")).unwrap_or(0),
                                lines: num(t.get("lines")).unwrap_or(0),
                                files: num(t.get("files")).unwrap_or(0),
                                first: t.get("first").and_then(|x| x.as_str()).map(str::to_string),
                                last_file: t.get("lastFile").and_then(|x| x.as_str()).map(str::to_string),
                            },
                        ));
                    }
                }
            }
            let file = arg(3).as_str();
            Ok(json!(tally_text(&js_string(arg(0)), &m, &mut tally, file)))
        }
        "indexTerms" => {
            let o = arg(1);
            let stop = str_list(o.get("stopwords"));
            let min = num(o.get("minLen")).unwrap_or(2);
            let max = num(o.get("maxLen")).unwrap_or(64);
            Ok(match arg(0) {
                Value::String(s) => json!(index_terms(s, &stop, min, max)),
                _ => json!([]),
            })
        }
        "splitList" => Ok(match split_list(arg(0)) {
            Some(v) => json!(v),
            None => Value::Null,
        }),
        _ => Err(format!("unknown fn {name}")),
    }
}


