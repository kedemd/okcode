//! One generic tree-sitter extractor, data per language (lib/analysis/treesitter.js).
//!
//! Offsets: JS reports UTF-16 code-unit offsets; tree-sitter here works on UTF-8 bytes. All
//! internal work is in bytes, and every offset that leaves this module is converted with `Src::u`.
//! The custom predicates in the .scm files (#strip!, #select-adjacent!, #set-adjacent!) are
//! unknown to web-tree-sitter and therefore no-ops in the JS; they are left unevaluated here too.
use regex::Regex;
use serde_json::{json, Map, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use tree_sitter::{Language, Node, Parser, Query, QueryCursor, StreamingIterator};

// ── per-language data ──────────────────────────────────────────────────────

#[derive(Clone, Copy)]
struct Comment {
    line: Option<&'static str>,
    open: Option<&'static str>,
    close: Option<&'static str>,
}
const SLASH: Comment = Comment { line: Some("//"), open: None, close: None };
const C_STYLE: Comment = Comment { line: Some("//"), open: Some("/*"), close: Some("*/") };
const HASH: Comment = Comment { line: Some("#"), open: None, close: None };

#[derive(Default, Clone, Copy)]
struct Decorators {
    wrapper: &'static [&'static str],
    sibling: &'static [&'static str],
    leading: &'static [&'static str],
}

struct Cfg {
    id: &'static str,
    exts: &'static [&'static str],
    tsx_exts: &'static [&'static str],
    query_version: u32,
    import_query_version: u32,
    comment: Comment,
    widen: Option<(&'static [&'static str], bool)>,
    decorators: Option<Decorators>,
}

static LANGUAGES: &[Cfg] = &[
    Cfg {
        id: "typescript",
        exts: &[".ts", ".mts", ".cts"],
        tsx_exts: &[".tsx"],
        query_version: 6,
        import_query_version: 1,
        comment: C_STYLE,
        widen: Some((&["lexical_declaration", "variable_declaration", "export_statement"], true)),
        decorators: Some(Decorators { wrapper: &[], sibling: &["decorator"], leading: &[] }),
    },
    Cfg {
        id: "python",
        exts: &[".py"],
        tsx_exts: &[],
        query_version: 4,
        import_query_version: 1,
        comment: HASH,
        widen: None,
        decorators: Some(Decorators { wrapper: &["decorated_definition"], sibling: &[], leading: &[] }),
    },
    Cfg { id: "go", exts: &[".go"], tsx_exts: &[], query_version: 4, import_query_version: 1, comment: SLASH, widen: None, decorators: None },
    Cfg {
        id: "rust",
        exts: &[".rs"],
        tsx_exts: &[],
        query_version: 6,
        import_query_version: 1,
        comment: SLASH,
        widen: None,
        decorators: Some(Decorators { wrapper: &[], sibling: &["attribute_item"], leading: &[] }),
    },
    Cfg {
        id: "php",
        exts: &[".php", ".phtml"],
        tsx_exts: &[],
        query_version: 4,
        import_query_version: 1,
        comment: C_STYLE,
        widen: None,
        decorators: Some(Decorators { wrapper: &[], sibling: &[], leading: &["attribute_list"] }),
    },
    Cfg {
        id: "java",
        exts: &[".java"],
        tsx_exts: &[],
        query_version: 2,
        import_query_version: 1,
        comment: C_STYLE,
        widen: None,
        decorators: Some(Decorators { wrapper: &[], sibling: &[], leading: &["modifiers"] }),
    },
    Cfg {
        id: "csharp",
        exts: &[".cs"],
        tsx_exts: &[],
        query_version: 3,
        import_query_version: 1,
        comment: C_STYLE,
        widen: None,
        decorators: Some(Decorators { wrapper: &[], sibling: &[], leading: &["attribute_list"] }),
    },
    Cfg {
        id: "c",
        exts: &[".c"],
        tsx_exts: &[],
        query_version: 5,
        import_query_version: 1,
        comment: C_STYLE,
        widen: Some((&["function_definition"], false)),
        decorators: None,
    },
    Cfg {
        id: "cpp",
        exts: &[".cpp", ".cc", ".cxx", ".h", ".hpp", ".hh", ".hxx", ".ipp"],
        tsx_exts: &[],
        query_version: 6,
        import_query_version: 1,
        comment: C_STYLE,
        widen: Some((&["function_definition"], false)),
        decorators: Some(Decorators { wrapper: &["template_declaration"], sibling: &[], leading: &["attribute_declaration"] }),
    },
];

fn cfg_of(id: &str) -> Option<&'static Cfg> {
    LANGUAGES.iter().find(|c| c.id == id)
}

fn text_of<'a>(n: Node, src: &'a [u8]) -> std::borrow::Cow<'a, str> {
    String::from_utf8_lossy(&src[n.start_byte()..n.end_byte()])
}

fn has_ancestor_type(node: Node, ty: &str, max_up: usize) -> bool {
    let mut n = node.parent();
    let mut i = 0;
    while let Some(x) = n {
        if i >= max_up {
            break;
        }
        if x.kind() == ty {
            return true;
        }
        n = x.parent();
        i += 1;
    }
    false
}

fn is_exported(id: &str, def: Node, name: &str, parent: Option<&str>, src: &[u8]) -> bool {
    let kids = || (0..def.child_count()).filter_map(move |i| def.child(i));
    match id {
        "typescript" => def.kind() == "export_statement" || has_ancestor_type(def, "export_statement", 2),
        "python" => parent.is_none() && !name.is_empty() && !name.starts_with('_'),
        "go" => name.chars().next().map_or(false, |c| c.is_ascii_uppercase()),
        "rust" => kids().any(|c| c.kind() == "visibility_modifier"),
        "php" => {
            if parent.is_none() {
                return true;
            }
            for c in kids() {
                if c.kind() == "visibility_modifier" {
                    return text_of(c, src) == "public";
                }
            }
            true
        }
        "java" => {
            for c in kids() {
                if c.kind() == "modifiers" {
                    return (0..c.child_count()).filter_map(|j| c.child(j)).any(|m| m.kind() == "public");
                }
            }
            false
        }
        "csharp" => {
            for c in kids() {
                if c.kind() != "modifier" {
                    continue;
                }
                if (0..c.child_count()).filter_map(|j| c.child(j)).any(|m| m.kind() == "public") {
                    return true;
                }
            }
            false
        }
        "c" | "cpp" => {
            if parent.is_some() {
                return false;
            }
            for c in kids() {
                if c.kind() == "storage_class_specifier" && text_of(c, src) == "static" {
                    return false;
                }
            }
            true
        }
        _ => false,
    }
}

// ── manifest / versions ────────────────────────────────────────────────────

static MANIFEST: &str = include_str!("../queries/manifest.json");

fn manifest_version(id: &str) -> String {
    let m: Value = serde_json::from_str(MANIFEST).unwrap_or(Value::Null);
    let Some(man) = m.get(id) else { return "0".into() };
    let ver = man["version"].as_str().unwrap_or("");
    match man.get("extra") {
        Some(x) if !x.is_null() => format!("{}+{}@{}", ver, x["package"].as_str().unwrap_or(""), x["version"].as_str().unwrap_or("")),
        _ => ver.to_string(),
    }
}

/// `currentVersionFor` (the runtime is always present here, so no `:none` suffix).
pub fn current_version_for(path: &str) -> Option<String> {
    let id = lang_id_for_path(path)?;
    let cfg = cfg_of(id)?;
    Some(format!("{}:q{}.{}", manifest_version(id), cfg.query_version, cfg.import_query_version))
}

fn ext_of(p: &str) -> String {
    // /\.[^./\\]+$/ lowercased
    let bytes = p.as_bytes();
    let mut i = bytes.len();
    while i > 0 {
        let c = bytes[i - 1];
        if c == b'.' {
            return if i == bytes.len() { String::new() } else { p[i - 1..].to_lowercase() };
        }
        if c == b'/' || c == b'\\' {
            break;
        }
        i -= 1;
    }
    String::new()
}

pub fn lang_id_for_path(p: &str) -> Option<&'static str> {
    let e = ext_of(p);
    LANGUAGES.iter().find(|c| c.exts.contains(&e.as_str()) || c.tsx_exts.contains(&e.as_str())).map(|c| c.id)
}

/// parse.js#langOf restricted to the tree-sitter extensions (the only ones this crate handles).
pub fn lang_of(path: &str) -> &'static str {
    match lang_id_for_path(path) {
        Some(id) => id,
        None => "other",
    }
}

// ── grammar cache ──────────────────────────────────────────────────────────

struct Grammar {
    parser: RefCell<Parser>,
    query: Query,
    import_query: Option<Query>,
}

thread_local! {
    static GRAMMARS: RefCell<HashMap<String, Rc<Grammar>>> = RefCell::new(HashMap::new());
}

fn language_for(id: &str, tsx: bool) -> Language {
    match (id, tsx) {
        ("typescript", false) => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        ("typescript", true) => tree_sitter_typescript::LANGUAGE_TSX.into(),
        ("python", _) => tree_sitter_python::LANGUAGE.into(),
        ("go", _) => tree_sitter_go::LANGUAGE.into(),
        ("rust", _) => tree_sitter_rust::LANGUAGE.into(),
        ("php", _) => tree_sitter_php::LANGUAGE_PHP.into(),
        ("java", _) => tree_sitter_java::LANGUAGE.into(),
        ("csharp", _) => tree_sitter_c_sharp::LANGUAGE.into(),
        ("c", _) => tree_sitter_c::LANGUAGE.into(),
        _ => tree_sitter_cpp::LANGUAGE.into(),
    }
}

// upstream tags.scm + (typescript) tags.upstream-js.scm + tags.local.scm, in that order.
fn query_text_for(id: &str) -> String {
    let (up, extra, local): (&str, &str, &str) = match id {
        "typescript" => (
            include_str!("../queries/typescript/tags.scm"),
            include_str!("../queries/typescript/tags.upstream-js.scm"),
            include_str!("../queries/typescript/tags.local.scm"),
        ),
        "python" => (include_str!("../queries/python/tags.scm"), "", include_str!("../queries/python/tags.local.scm")),
        "go" => (include_str!("../queries/go/tags.scm"), "", include_str!("../queries/go/tags.local.scm")),
        "rust" => (include_str!("../queries/rust/tags.scm"), "", include_str!("../queries/rust/tags.local.scm")),
        "php" => (include_str!("../queries/php/tags.scm"), "", include_str!("../queries/php/tags.local.scm")),
        "java" => (include_str!("../queries/java/tags.scm"), "", include_str!("../queries/java/tags.local.scm")),
        "csharp" => (include_str!("../queries/csharp/tags.scm"), "", include_str!("../queries/csharp/tags.local.scm")),
        "c" => (include_str!("../queries/c/tags.scm"), "", include_str!("../queries/c/tags.local.scm")),
        _ => (include_str!("../queries/cpp/tags.scm"), "", include_str!("../queries/cpp/tags.local.scm")),
    };
    format!("{up}\n{extra}\n{local}")
}

fn import_query_text_for(id: &str) -> &'static str {
    match id {
        "typescript" => include_str!("../queries/typescript/imports.scm"),
        "python" => include_str!("../queries/python/imports.scm"),
        "go" => include_str!("../queries/go/imports.scm"),
        "rust" => include_str!("../queries/rust/imports.scm"),
        "php" => include_str!("../queries/php/imports.scm"),
        "java" => include_str!("../queries/java/imports.scm"),
        "csharp" => include_str!("../queries/csharp/imports.scm"),
        "c" => include_str!("../queries/c/imports.scm"),
        _ => include_str!("../queries/cpp/imports.scm"),
    }
}

fn grammar_for(id: &str, path: &str) -> Result<Rc<Grammar>, String> {
    let cfg = cfg_of(id).ok_or_else(|| format!("{id} — searchable, no symbol extractor yet"))?;
    let tsx = cfg.tsx_exts.contains(&ext_of(path).as_str());
    let key = format!("{id}{}", if tsx { ":tsx" } else { "" });
    if let Some(g) = GRAMMARS.with(|m| m.borrow().get(&key).cloned()) {
        return Ok(g);
    }
    let lang = language_for(id, tsx);
    let mut parser = Parser::new();
    parser.set_language(&lang).map_err(|e| e.to_string())?;
    let query = Query::new(&lang, &query_text_for(id)).map_err(|e| format!("{id} tags query: {e}"))?;
    let import_query = Some(Query::new(&lang, import_query_text_for(id)).map_err(|e| format!("{id} imports query: {e}"))?);
    let g = Rc::new(Grammar { parser: RefCell::new(parser), query, import_query });
    GRAMMARS.with(|m| m.borrow_mut().insert(key, g.clone()));
    Ok(g)
}

// ── source text helpers (UTF-16 offsets for output, byte offsets inside) ───

struct Src<'a> {
    s: &'a str,
    b: &'a [u8],
    map: Option<Vec<u32>>, // byte index -> utf16 index; None when ASCII
    line_starts: Vec<usize>,
}

impl<'a> Src<'a> {
    fn new(s: &'a str) -> Self {
        let b = s.as_bytes();
        let map = if s.is_ascii() {
            None
        } else {
            let mut m = vec![0u32; b.len() + 1];
            let mut u = 0u32;
            for (i, ch) in s.char_indices() {
                for k in 0..ch.len_utf8() {
                    m[i + k] = u;
                }
                u += ch.len_utf16() as u32;
            }
            m[b.len()] = u;
            Some(m)
        };
        let mut line_starts = vec![0usize];
        for (i, &c) in b.iter().enumerate() {
            if c == b'\n' {
                line_starts.push(i + 1);
            }
        }
        Src { s, b, map, line_starts }
    }
    fn u(&self, byte: usize) -> usize {
        match &self.map {
            None => byte,
            Some(m) => m[byte.min(self.b.len())] as usize,
        }
    }
    fn line_of(&self, offset: usize) -> usize {
        let at = offset.min(self.b.len());
        // last line start <= at
        self.line_starts.partition_point(|&x| x <= at)
    }
    fn col_of(&self, offset: usize) -> usize {
        let ls = self.line_starts[self.line_of(offset) - 1];
        1 + self.u(offset) - self.u(ls)
    }
    fn slice(&self, a: usize, b: usize) -> &str {
        if a >= b || b > self.b.len() {
            return "";
        }
        std::str::from_utf8(&self.b[a..b]).unwrap_or("")
    }
}

fn is_js_ws(c: char) -> bool {
    matches!(c, '\u{9}' | '\u{a}' | '\u{b}' | '\u{c}' | '\u{d}' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}
fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_ws)
}

fn trunc_u16(s: &str, n: usize) -> String {
    let mut count = 0;
    for (i, ch) in s.char_indices() {
        let l = ch.len_utf16();
        if count + l > n {
            return s[..i].to_string();
        }
        count += l;
    }
    s.to_string()
}
fn len_u16(s: &str) -> usize {
    s.chars().map(|c| c.len_utf16()).sum()
}

// ── regexes derived from the comment style ─────────────────────────────────

struct StyleRe {
    // cleanDocText
    doc_line: Option<Regex>,   // ^<line>!?\s?
    doc_open: Option<Regex>,   // ^<open>\*?\s?
    doc_close: Option<Regex>,  // \s?<close>$
    // commentAbove
    above_line: Option<Regex>, // ^<line>!?/?\s?
    above_open: Option<Regex>, // ^<open>+\s?
    above_close: Option<Regex>, // \s?<close>+$
}

fn esc(s: &str) -> String {
    regex::escape(s)
}

impl StyleRe {
    fn new(c: &Comment) -> Self {
        let re = |s: String| Some(Regex::new(&s).unwrap());
        StyleRe {
            doc_line: c.line.and_then(|l| re(format!(r"^{}!?\s?", esc(l)))),
            doc_open: c.open.and_then(|o| re(format!(r"^{}\*?\s?", esc(o)))),
            doc_close: c.close.and_then(|x| re(format!(r"\s?{}$", esc(x)))),
            above_line: c.line.and_then(|l| re(format!(r"^{}!?/?\s?", esc(l)))),
            // JS escapes the whole string, then applies `+` to its last char.
            above_open: c.open.and_then(|o| re(format!(r"^{}+\s?", quant_last(o)))),
            above_close: c.close.and_then(|x| re(format!(r"\s?{}+$", quant_last(x)))),
        }
    }
}

// `escapeRe(s) + '+'` applies `+` to the last escaped unit only: all but the last char, then the last, then +.
fn quant_last(s: &str) -> String {
    // return the escaped string without the trailing `+` (the caller appends it)
    esc(s)
}

fn strip_quotes(text: &str) -> String {
    for q in ["\"\"\"", "'''", "\"", "'", "`"] {
        if text.starts_with(q) && text.ends_with(q) && len_u16(text) >= q.len() * 2 {
            return js_trim(&text[q.len()..text.len() - q.len()]).to_string();
        }
    }
    js_trim(text).to_string()
}

fn lead_star(t: &str) -> String {
    // /^\*\s?/
    if let Some(r) = t.strip_prefix('*') {
        let mut it = r.chars();
        match it.next() {
            Some(c) if is_js_ws(c) => it.as_str().to_string(),
            _ => r.to_string(),
        }
    } else {
        t.to_string()
    }
}

fn clean_doc_text(raws: &[String], style: &Comment, re: &StyleRe) -> String {
    let parts: Vec<String> = raws
        .iter()
        .map(|raw| {
            let s = js_trim(raw);
            if s.starts_with(['"', '\'', '`']) {
                return strip_quotes(s);
            }
            // style is always Some in the languages here
            s.split('\n')
                .map(|l| {
                    let t = js_trim(l);
                    if style.line.map_or(false, |x| t.starts_with(x)) {
                        return re.doc_line.as_ref().unwrap().replace(t, "").to_string();
                    }
                    if style.open.map_or(false, |x| t.starts_with(x)) {
                        let a = re.doc_open.as_ref().unwrap().replace(t, "").to_string();
                        return re.doc_close.as_ref().unwrap().replace(&a, "").to_string();
                    }
                    lead_star(t)
                })
                .collect::<Vec<_>>()
                .join(" ")
                .trim_matches(is_js_ws)
                .to_string()
        })
        .collect();
    trunc_u16(js_trim(&parts.join(" ")), 400)
}

fn comment_above(src: &Src, start: usize, prev_end: usize, style: &Comment, re: &StyleRe) -> String {
    let before = src.slice(prev_end, start);
    let lines: Vec<&str> = before.split('\n').map(js_trim).collect();
    let mut doc: Vec<String> = Vec::new();
    for l in lines.iter().rev() {
        let l = *l;
        if l.is_empty() {
            if !doc.is_empty() {
                break;
            }
            continue;
        }
        if style.line.map_or(false, |x| l.starts_with(x)) {
            doc.insert(0, re.above_line.as_ref().unwrap().replace(l, "").to_string());
            continue;
        }
        if let (Some(close), Some(open)) = (style.close, style.open) {
            if l.ends_with(close) || l.starts_with(open) || l.starts_with('*') {
                let a = re.above_open.as_ref().unwrap().replace(l, "").to_string();
                let b = re.above_close.as_ref().unwrap().replace(&a, "").to_string();
                doc.insert(0, lead_star(&b));
                continue;
            }
        }
        break;
    }
    trunc_u16(js_trim(&doc.join(" ")), 400)
}

// ── signature ──────────────────────────────────────────────────────────────

const SIGNATURE_MAX: usize = 300;
const BODY_TYPES: &[&str] = &["statement_block", "block", "class_body", "declaration_list", "field_declaration_list", "compound_statement"];

fn find_body_start(node: Node) -> Option<usize> {
    if let Some(b) = node.child_by_field_name("body") {
        return Some(b.start_byte());
    }
    for i in 0..node.child_count() {
        let c = node.child(i).unwrap();
        if BODY_TYPES.contains(&c.kind()) {
            return Some(c.start_byte());
        }
    }
    for i in 0..node.child_count() {
        let child = node.child(i).unwrap();
        if let Some(n) = child.child_by_field_name("body") {
            return Some(n.start_byte());
        }
        for j in 0..child.child_count() {
            let g = child.child(j).unwrap();
            if BODY_TYPES.contains(&g.kind()) {
                return Some(g.start_byte());
            }
        }
    }
    None
}

struct SigRe {
    ws: Regex,
    open: Regex,
    close: Regex,
    tail: Regex,
}

fn signature_of(src: &Src, content_start: usize, node: Node, re: &SigRe) -> String {
    let end = find_body_start(node).unwrap_or(node.end_byte());
    let raw = src.slice(content_start, end);
    // JS \s differs slightly from Rust's; collapse by JS whitespace set.
    let mut collapsed = String::with_capacity(raw.len());
    let mut in_ws = false;
    for ch in raw.chars() {
        if is_js_ws(ch) || ch == '\u{2028}' {
            if !in_ws {
                collapsed.push(' ');
            }
            in_ws = true;
        } else {
            collapsed.push(ch);
            in_ws = false;
        }
    }
    let _ = &re.ws;
    let a = re.open.replace_all(&collapsed, "$1").to_string();
    let b = re.close.replace_all(&a, "$1").to_string();
    let c = js_trim(&b).to_string();
    let d = re.tail.replace(&c, "").to_string();
    if len_u16(&d) > SIGNATURE_MAX {
        format!("{}…", trunc_u16(&d, SIGNATURE_MAX))
    } else {
        d
    }
}

// ── decorators / widening ──────────────────────────────────────────────────

fn decorated_span<'t>(node: Node<'t>, dec: &Option<Decorators>) -> (usize, usize, usize) {
    let mut start = node.start_byte();
    let mut end = node.end_byte();
    let mut content_start = node.start_byte();
    let Some(dec) = dec else { return (start, content_start, end) };
    if !dec.wrapper.is_empty() {
        if let Some(p) = node.parent() {
            if dec.wrapper.contains(&p.kind()) {
                start = p.start_byte();
                end = p.end_byte();
            }
        }
    }
    if !dec.sibling.is_empty() {
        let mut prev = node.prev_sibling();
        while let Some(p) = prev {
            if !dec.sibling.contains(&p.kind()) {
                break;
            }
            start = p.start_byte();
            prev = p.prev_sibling();
        }
    }
    if !dec.leading.is_empty() {
        let mut i = 0usize;
        let mut child = node.child(0);
        while let Some(c) = child {
            if !dec.leading.contains(&c.kind()) {
                break;
            }
            i += 1;
            child = node.child(i);
        }
        if let Some(c) = child {
            content_start = c.start_byte();
        }
    }
    (start, content_start, end)
}

fn widen_to_parent<'t>(node: Node<'t>, widen: &Option<(&'static [&'static str], bool)>) -> Node<'t> {
    let Some((types, sole)) = widen else { return node };
    let mut at = node;
    while let Some(p) = at.parent() {
        if !types.contains(&p.kind()) {
            break;
        }
        if *sole {
            let mut cur = p.walk();
            let n = p.named_children(&mut cur).filter(|c| c.kind() == at.kind()).count();
            if n > 1 {
                break;
            }
        }
        at = p;
    }
    at
}

// ── syntax errors ──────────────────────────────────────────────────────────

struct SyntaxError {
    start: usize,
    end: usize,
    is_missing: bool,
}

fn collect_syntax_errors(root: Node) -> Vec<SyntaxError> {
    let mut errors = Vec::new();
    let mut cursor = root.walk();
    // iterative pre-order walk over every node (named and anonymous)
    loop {
        let node = cursor.node();
        if node.kind() == "ERROR" || node.is_missing() {
            let end = node.end_byte();
            errors.push(SyntaxError { start: node.start_byte(), end: if end != 0 { end } else { node.start_byte() + 1 }, is_missing: node.is_missing() });
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return errors;
            }
        }
    }
}

// ── symbol extraction ──────────────────────────────────────────────────────

const KIND_PRIORITY: &[&str] = &["method", "function", "class", "interface", "enum", "type", "constant", "module"];
fn kind_rank(k: &str) -> usize {
    KIND_PRIORITY.iter().position(|x| *x == k).unwrap_or(KIND_PRIORITY.len())
}

struct Raw<'t> {
    kind: String,
    name: Option<String>,
    node: Node<'t>,
    doc: Option<String>,
    parent: Option<String>,
}

struct Def<'t> {
    name: String,
    kind: String,
    start: usize,
    content_start: usize,
    end: usize,
    explicit_parent: Option<String>,
    doc: Option<String>,
    node: Node<'t>,
    parent_def: Option<usize>,
    parent: Option<String>,
    path: Option<String>, // None until settled (JS: undefined)
}

fn truthy(o: &Option<String>) -> bool {
    o.as_ref().map_or(false, |s| !s.is_empty())
}

struct Ref {
    name: String,
    kind: String,
    line: usize,
}

fn build_symbols(cfg: &Cfg, src: &Src, root: Node, query: &Query) -> (Vec<Value>, Vec<Ref>) {
    let names = query.capture_names();
    let style = cfg.comment;
    let sre = StyleRe::new(&style);
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, root, src.b);

    let mut raw: Vec<Raw> = Vec::new();
    let mut standalone: Vec<(usize, usize, String)> = Vec::new();
    let mut refs: Vec<Ref> = Vec::new();

    while let Some(m) = matches.next() {
        let mut def: Option<(&str, Node)> = None;
        let mut name: Option<Node> = None;
        let mut parent: Option<Node> = None;
        let mut rf: Option<(&str, Node)> = None;
        let mut docs: Vec<Node> = Vec::new();
        for c in m.captures {
            let cn = names[c.index as usize];
            if cn.starts_with("definition.") {
                def = Some((cn, c.node));
            } else if cn == "name" {
                name = Some(c.node);
            } else if cn == "doc" {
                docs.push(c.node);
            } else if cn == "parent" {
                parent = Some(c.node);
            } else if cn.starts_with("reference.") {
                rf = Some((cn, c.node));
            }
        }
        let doc_texts = |docs: &[Node]| -> Vec<String> { docs.iter().map(|n| text_of(*n, src.b).into_owned()).collect() };
        if let Some((cn, node)) = def {
            raw.push(Raw {
                kind: cn["definition.".len()..].to_string(),
                name: name.map(|n| text_of(n, src.b).into_owned()),
                node,
                doc: if docs.is_empty() { None } else { Some(clean_doc_text(&doc_texts(&docs), &style, &sre)) },
                parent: parent.map(|n| text_of(n, src.b).into_owned()),
            });
        } else if let Some((cn, node)) = rf {
            refs.push(Ref {
                name: name.map(|n| text_of(n, src.b).into_owned()).unwrap_or_else(|| text_of(node, src.b).into_owned()),
                kind: cn["reference.".len()..].to_string(),
                line: src.line_of(node.start_byte()),
            });
        } else if !docs.is_empty() && name.is_none() {
            let start = docs.iter().map(|n| n.start_byte()).min().unwrap();
            let end = docs.iter().map(|n| n.end_byte()).max().unwrap();
            standalone.push((start, end, clean_doc_text(&doc_texts(&docs), &style, &sre)));
        }
    }

    // Merge same-range matches.
    let mut order: Vec<Raw> = Vec::new();
    let mut by_range: HashMap<(usize, usize), usize> = HashMap::new();
    for d in raw {
        if d.name.is_none() {
            continue;
        }
        let key = (d.node.start_byte(), d.node.end_byte());
        match by_range.get(&key) {
            None => {
                by_range.insert(key, order.len());
                order.push(d);
            }
            Some(&i) => {
                let ex = &mut order[i];
                if kind_rank(&d.kind) < kind_rank(&ex.kind) {
                    ex.kind = d.kind.clone();
                }
                if !truthy(&ex.doc) && truthy(&d.doc) {
                    ex.doc = d.doc.clone();
                }
                if !truthy(&ex.parent) && truthy(&d.parent) {
                    ex.parent = d.parent.clone();
                }
            }
        }
    }
    let mut defs: Vec<Def> = order
        .into_iter()
        .map(|d| {
            let node = widen_to_parent(d.node, &cfg.widen);
            let (start, content_start, end) = decorated_span(node, &cfg.decorators);
            Def {
                name: d.name.unwrap(),
                kind: d.kind,
                start,
                content_start,
                end,
                explicit_parent: d.parent,
                doc: d.doc,
                node,
                parent_def: None,
                parent: None,
                path: None,
            }
        })
        .collect();

    // Widening can land two captures of one definition on the same span: one symbol, best kind.
    let mut seen: HashMap<(usize, usize, String), usize> = HashMap::new();
    let mut remove = vec![false; defs.len()];
    for i in (0..defs.len()).rev() {
        let key = (defs[i].start, defs[i].end, defs[i].name.clone());
        match seen.get(&key) {
            None => {
                seen.insert(key, i);
            }
            Some(&o) => {
                let (dk, dd, dp) = (defs[i].kind.clone(), defs[i].doc.clone(), defs[i].explicit_parent.clone());
                let other = &mut defs[o];
                if kind_rank(&dk) < kind_rank(&other.kind) {
                    other.kind = dk;
                }
                if !truthy(&other.doc) && truthy(&dd) {
                    other.doc = dd;
                }
                if !truthy(&other.explicit_parent) && truthy(&dp) {
                    other.explicit_parent = dp;
                }
                remove[i] = true;
            }
        }
    }
    let mut it = remove.iter();
    defs.retain(|_| !*it.next().unwrap());

    // Nesting.
    defs.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
    let n = defs.len();
    for i in 0..n {
        if truthy(&defs[i].explicit_parent) {
            defs[i].parent = defs[i].explicit_parent.clone();
            continue;
        }
        let mut parent: Option<usize> = None;
        for j in 0..n {
            if j == i {
                continue;
            }
            if defs[j].start <= defs[i].start && defs[j].end >= defs[i].end {
                let better = match parent {
                    None => true,
                    Some(p) => (src.u(defs[j].end) - src.u(defs[j].start)) < (src.u(defs[p].end) - src.u(defs[p].start)),
                };
                if better {
                    parent = Some(j);
                }
            }
        }
        defs[i].parent_def = parent;
        defs[i].parent = None;
    }
    for i in 0..n {
        if truthy(&defs[i].explicit_parent) {
            defs[i].path = Some(format!("{}.{}", defs[i].explicit_parent.as_ref().unwrap(), defs[i].name));
        } else if let Some(p) = defs[i].parent_def {
            let pp = defs[p].path.clone();
            defs[i].parent = pp.clone();
            defs[i].path = Some(format!("{}.{}", pp.as_deref().unwrap_or("undefined"), defs[i].name));
        } else {
            defs[i].path = Some(defs[i].name.clone());
        }
    }

    // Doc fallback.
    for (s, e, text) in &standalone {
        let mut owner: Option<usize> = None;
        for (j, d) in defs.iter().enumerate() {
            if *s >= d.start && *e <= d.end {
                let better = match owner {
                    None => true,
                    Some(o) => (src.u(d.end) - src.u(d.start)) < (src.u(defs[o].end) - src.u(defs[o].start)),
                };
                if better {
                    owner = Some(j);
                }
            }
        }
        if let Some(o) = owner {
            if !truthy(&defs[o].doc) {
                defs[o].doc = Some(text.clone());
            }
        }
    }
    let mut idx: Vec<usize> = (0..n).collect();
    idx.sort_by_key(|&i| defs[i].start); // stable
    let mut prev_end = 0usize;
    for i in idx {
        if !truthy(&defs[i].doc) {
            let c = comment_above(src, defs[i].start, prev_end, &style, &sre);
            defs[i].doc = if c.is_empty() { None } else { Some(c) };
        }
        prev_end = defs[i].end;
    }

    let sig_re = SigRe {
        ws: Regex::new(r"\s+").unwrap(),
        open: Regex::new(r"([(\[]) ").unwrap(),
        close: Regex::new(r",? ([)\]])").unwrap(),
        tail: Regex::new(r"\s*[{:]\s*$").unwrap(),
    };
    let symbols = defs
        .iter()
        .map(|d| {
            let mut o = Map::new();
            o.insert("name".into(), json!(d.name));
            o.insert("kind".into(), json!(d.kind));
            o.insert("path".into(), json!(d.path.clone().unwrap_or_else(|| "undefined".into())));
            if truthy(&d.parent) {
                o.insert("parent".into(), json!(d.parent));
            }
            o.insert("start".into(), json!(src.u(d.start)));
            o.insert("end".into(), json!(src.u(d.end)));
            o.insert("lineStart".into(), json!(src.line_of(d.start)));
            o.insert("lineEnd".into(), json!(src.line_of(d.end)));
            o.insert("signature".into(), json!(signature_of(src, d.content_start, d.node, &sig_re)));
            o.insert("doc".into(), json!(d.doc.clone().unwrap_or_default()));
            if is_exported(cfg.id, d.node, &d.name, d.parent.as_deref().filter(|s| !s.is_empty()), src.b) {
                o.insert("exported".into(), json!(true));
            }
            Value::Object(o)
        })
        .collect();
    (symbols, refs)
}

// ── imports ────────────────────────────────────────────────────────────────

struct ImportGroup {
    family: String,
    start: usize,
    source: Option<String>,
    names: Vec<(String, String)>,
}

fn build_imports(src: &Src, root: Node, query: &Option<Query>) -> Vec<Value> {
    let Some(query) = query else { return vec![] };
    let cn = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, root, src.b);
    let mut groups: Vec<ImportGroup> = Vec::new();
    let mut index: HashMap<(String, usize, usize), usize> = HashMap::new();
    while let Some(m) = matches.next() {
        let mut stmt: Option<(Node, String)> = None;
        let mut source: Option<Node> = None;
        let mut local: Option<Node> = None;
        let mut imported: Option<Node> = None;
        for c in m.captures {
            let name = cn[c.index as usize];
            let (fam, role) = match name.rfind('.') {
                Some(d) => (&name[..d], &name[d + 1..]),
                None => (name, name),
            };
            match role {
                "stmt" => stmt = Some((c.node, fam.to_string())),
                "source" => source = Some(c.node),
                "name" => local = Some(c.node),
                "importedName" => imported = Some(c.node),
                _ => {}
            }
        }
        let Some((snode, family)) = stmt else { continue };
        let mut names = Vec::new();
        if let Some(l) = local {
            let lt = text_of(l, src.b).into_owned();
            names.push((lt.clone(), imported.map(|i| text_of(i, src.b).into_owned()).unwrap_or(lt)));
        } else if let Some(i) = imported {
            let t = text_of(i, src.b).into_owned();
            names.push((t.clone(), t));
        }
        let key = (family.clone(), snode.start_byte(), snode.end_byte());
        let gi = *index.entry(key).or_insert_with(|| {
            groups.push(ImportGroup { family: family.clone(), start: snode.start_byte(), source: None, names: vec![] });
            groups.len() - 1
        });
        let g = &mut groups[gi];
        if let Some(s) = source {
            if g.source.is_none() {
                g.source = Some(text_of(s, src.b).into_owned());
            }
        }
        g.names.extend(names);
    }
    groups
        .into_iter()
        .map(|g| {
            json!({
                "local": null,
                "from": g.source,
                "line": src.line_of(g.start),
                "kind": g.family,
                "reexport": g.family == "reexport",
                "names": if g.names.is_empty() { Value::Null } else { Value::Array(g.names.iter().map(|(l, i)| json!({"local": l, "imported": i})).collect()) },
            })
        })
        .collect()
}

// ── public entry points ────────────────────────────────────────────────────

/// `extractFor`: the symbols/imports/exports/references/analyzerVersion object.
pub fn extract_for(id: &str, path: &str, text: &str) -> Result<Value, String> {
    let cfg = cfg_of(id).ok_or_else(|| format!("{id} — searchable, no symbol extractor yet"))?;
    let g = grammar_for(id, path)?;
    let tree = g.parser.borrow_mut().parse(text, None).ok_or("tree-sitter parse failed")?;
    let src = Src::new(text);
    let root = tree.root_node();
    let (symbols, refs) = build_symbols(cfg, &src, root, &g.query);
    let imports = build_imports(&src, root, &g.import_query);
    let exports: Vec<Value> = symbols.iter().filter(|s| s.get("exported").is_some() && s.get("parent").is_none()).map(|s| s["name"].clone()).collect();
    let references: Vec<Value> = refs.iter().map(|r| json!({"name": r.name, "kind": r.kind, "line": r.line})).collect();
    Ok(json!({
        "symbols": symbols,
        "imports": imports,
        "exports": exports,
        "references": references,
        "analyzerVersion": format!("{}:q{}.{}", manifest_version(id), cfg.query_version, cfg.import_query_version),
    }))
}

fn diagnostics_of(text: &str, tree: &tree_sitter::Tree) -> Vec<Value> {
    let src = Src::new(text);
    collect_syntax_errors(tree.root_node())
        .into_iter()
        .map(|e| {
            json!({
                "source": "treesitter-syntax",
                "code": if e.is_missing { "MISSING" } else { "ERROR" },
                "severity": "error",
                "message": if e.is_missing { "expected syntax is missing" } else { "unexpected syntax" },
                "start": src.u(e.start),
                "end": src.u(e.end),
                "line": src.line_of(e.start),
                "column": src.col_of(e.start),
                "endLine": src.line_of(e.end),
            })
        })
        .collect()
}

/// ERROR/MISSING nodes as diagnostics in the shared shape (`syntaxDiagnostics`).
pub fn syntax_diagnostics(path: &str, text: &str) -> Vec<Value> {
    let Some(id) = lang_id_for_path(path) else { return vec![] };
    let Ok(g) = grammar_for(id, path) else { return vec![] };
    let Some(tree) = g.parser.borrow_mut().parse(text, None) else { return vec![] };
    diagnostics_of(text, &tree)
}

/// `jsxSyntaxDiagnostics`: the forced-TSX parse's ERROR/MISSING nodes for a JS-family candidate.
pub fn jsx_syntax_diagnostics(text: &str) -> Vec<Value> {
    let Ok(g) = grammar_for("typescript", "x.tsx") else { return vec![] };
    let Some(tree) = g.parser.borrow_mut().parse(text, None) else { return vec![] };
    diagnostics_of(text, &tree)
}

/// `extractJsxFallback`: extractFor over the forced-TSX grammar, plus `hasErrors`.
pub fn extract_jsx_fallback(text: &str) -> Result<Value, String> {
    let cfg = cfg_of("typescript").ok_or("typescript")?;
    let g = grammar_for("typescript", "x.tsx")?;
    let tree = g.parser.borrow_mut().parse(text, None).ok_or("tree-sitter parse failed")?;
    let src = Src::new(text);
    let root = tree.root_node();
    let has_errors = !collect_syntax_errors(root).is_empty();
    let (symbols, refs) = build_symbols(cfg, &src, root, &g.query);
    let imports = build_imports(&src, root, &g.import_query);
    let exports: Vec<Value> = symbols.iter().filter(|s| s.get("exported").is_some() && s.get("parent").is_none()).map(|s| s["name"].clone()).collect();
    let references: Vec<Value> = refs.iter().map(|r| json!({"name": r.name, "kind": r.kind, "line": r.line})).collect();
    Ok(json!({
        "symbols": symbols, "imports": imports, "exports": exports, "references": references,
        "analyzerVersion": format!("{}:q{}.{}", manifest_version("typescript"), cfg.query_version, cfg.import_query_version),
        "hasErrors": has_errors,
    }))
}

/// `jsxFallbackVersionTag` (the runtime is always present: no `:none`).
pub fn jsx_fallback_version_tag() -> String {
    let cfg = cfg_of("typescript").unwrap();
    format!("{}:q{}.{}", manifest_version("typescript"), cfg.query_version, cfg.import_query_version)
}

/// `LANGUAGES` as plain data (id -> { id, exts, tsxExts, queryVersion, importQueryVersion }).
pub fn languages_json() -> Value {
    let mut o = serde_json::Map::new();
    for c in LANGUAGES {
        o.insert(c.id.into(), json!({
            "id": c.id, "exts": c.exts, "tsxExts": c.tsx_exts,
            "queryVersion": c.query_version, "importQueryVersion": c.import_query_version,
        }));
    }
    Value::Object(o)
}

fn is_secret(path: &str) -> bool {
    let p = path.replace('\\', "/");
    thread_local! {
        static RES: Vec<Regex> = [
            r"(?i)(^|/)\.env($|\.)",
            r"(?i)(^|/)\.(npmrc|netrc|pgpass|htpasswd)$",
            r"(?i)(^|/)(id_rsa|id_dsa|id_ecdsa|id_ed25519)(\.pub)?$",
            r"(?i)(^|/)(credentials|secrets?)(\.[\w-]+)?\.(json|ya?ml|ini|txt)$",
            r"(?i)\.(pem|key|p12|pfx|keystore|jks|license)$",
        ].iter().map(|r| Regex::new(r).unwrap()).collect();
    }
    RES.with(|v| v.iter().any(|r| r.is_match(&p)))
}

/// parse.js#extract for a path whose language is handled by tree-sitter (any other path: Err).
pub fn extract(path: &str, src: Option<&str>) -> Result<Value, String> {
    let id = lang_id_for_path(path).ok_or_else(|| format!("not a tree-sitter path: {path}"))?;
    let lang = id;
    let Some(text) = src else {
        return Ok(json!({
            "path": path, "lang": lang, "lines": 0, "symbols": [], "imports": [], "exports": [],
            "parsed": false, "indexed": false,
            "reason": if is_secret(path) { "may hold credentials — hashed, never read" } else { "binary or oversized — hashed, not read" },
        }));
    };
    let lines = text.split('\n').count();
    let version = current_version_for(path);
    match extract_for(id, path, text) {
        Ok(Value::Object(mut o)) => {
            o.insert("path".into(), json!(path));
            o.insert("lang".into(), json!(lang));
            o.insert("lines".into(), json!(lines));
            o.insert("analyzerVersion".into(), json!(version));
            o.insert("parsed".into(), json!(true));
            o.insert("indexed".into(), json!(true));
            Ok(Value::Object(o))
        }
        Ok(_) => Err("unreachable".into()),
        Err(e) => Ok(json!({
            "path": path, "lang": lang, "lines": lines, "symbols": [], "imports": [], "exports": [],
            "parsed": false, "indexed": true, "analyzerVersion": version,
            "reason": trunc_u16(&e, 160),
        })),
    }
}

/// The tree-sitter branch of validate.js#validateSource (no palette: the convention-color check lives elsewhere).
pub fn validate_source(rel: &str, text: &str) -> Result<Value, String> {
    if lang_id_for_path(rel).is_none() {
        return Err(format!("not a tree-sitter path: {rel}"));
    }
    let found = syntax_diagnostics(rel, text);
    let full = len_u16(text);
    Ok(json!({
        "validators": [{
            "validator": "treesitter-syntax",
            "status": if found.is_empty() { "passed" } else { "failed" },
            "covers": [{"start": 0, "end": full}],
        }],
        "diagnostics": found,
        "coverage": "structural",
    }))
}

pub fn dispatch(name: &str, args: &[Value]) -> Result<Value, String> {
    match name {
        "extract" => {
            let path = args.first().and_then(|v| v.as_str()).ok_or("path")?;
            let src = args.get(1).and_then(|v| v.as_str());
            extract(path, src)
        }
        "validateSource" => {
            let a = args.first().ok_or("arg")?;
            let rel = a["rel"].as_str().ok_or("rel")?;
            let text = a["text"].as_str().ok_or("text")?;
            if a["palette"].as_array().map_or(false, |p| !p.is_empty()) {
                return Err("palette (convention-color) not handled in this crate".into());
            }
            validate_source(rel, text)
        }
        _ => Err(format!("unknown fn {name}")),
    }
}
