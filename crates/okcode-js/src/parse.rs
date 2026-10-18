//! Symbol extraction (analysis/parse.js). oxc replaces acorn; the symbol rules are the JS ones.
use crate::text::{is_js_ws, js_trim, slice_units};
use oxc_allocator::Allocator;
use oxc_ast::ast::*;
use oxc_ast_visit::{walk, Visit};
use oxc_parser::{ParseOptions, Parser, ParserReturn};
use oxc_span::{GetSpan, SourceType, Span};
use serde_json::{json, Map, Value};

pub const TEXT_MAX: u64 = 512 * 1024;
const MAX_DEPTH: usize = 3;
// The analyzer versions stored rows are checked against; JSX_TAG matches okcode-treesitter's jsxFallbackVersionTag.
pub const JS_ANALYZER_VERSION: &str = "js:3";
const JSX_TAG: &str = "0.23.2+tree-sitter-javascript@0.23.1:q6.1";

const TEXT: &[&str] = &[
    ".md", ".markdown", ".txt", ".json", ".jsonc", ".yml", ".yaml", ".html", ".htm", ".css", ".scss", ".xml", ".svg",
    ".csv", ".ini", ".toml", ".env", ".sh", ".bash", ".ps1", ".sql", ".gitignore", ".editorconfig", ".ok",
];

fn is_js_ext(e: &str) -> bool {
    matches!(e, ".js" | ".mjs" | ".cjs" | ".jsx")
}

pub fn ext_of(path: &str) -> String {
    match path.char_indices().rev().find(|&(_, c)| c == '.' || c == '/' || c == '\\') {
        Some((i, '.')) if i + 1 < path.len() => path[i..].to_lowercase(),
        _ => String::new(),
    }
}

fn lang_for_ext(e: &str) -> Option<&'static str> {
    Some(match e {
        ".ts" | ".tsx" | ".mts" | ".cts" => "typescript",
        ".py" => "python",
        ".go" => "go",
        ".rs" => "rust",
        ".php" | ".phtml" => "php",
        ".java" => "java",
        ".cs" => "csharp",
        ".c" => "c",
        ".h" | ".cpp" | ".cc" | ".cxx" | ".hpp" | ".hh" | ".hxx" | ".ipp" => "cpp",
        ".json" | ".jsonc" => "json",
        ".md" | ".markdown" => "markdown",
        ".txt" => "text",
        ".html" | ".htm" | ".ok" => "html",
        ".css" | ".scss" => "css",
        ".yml" | ".yaml" => "yaml",
        ".sh" | ".bash" => "shell",
        ".ps1" => "powershell",
        ".sql" => "sql",
        ".xml" => "xml",
        ".svg" => "svg",
        ".csv" => "csv",
        ".toml" => "toml",
        ".ini" => "ini",
        _ => return None,
    })
}

pub fn lang_of(path: &str) -> &'static str {
    let e = ext_of(path);
    if is_js_ext(&e) {
        return "javascript";
    }
    lang_for_ext(&e).unwrap_or("other")
}

/// Files whose CONTENTS must never enter the index.
pub fn is_secret(path: &str) -> bool {
    use regex::Regex;
    use std::sync::OnceLock;
    static RES: OnceLock<Vec<Regex>> = OnceLock::new();
    let res = RES.get_or_init(|| {
        [
            r"(?i)(^|/)\.env($|\.)",
            r"(?i)(^|/)\.(npmrc|netrc|pgpass|htpasswd)$",
            r"(?i)(^|/)(id_rsa|id_dsa|id_ecdsa|id_ed25519)(\.pub)?$",
            r"(?i)(^|/)(credentials|secrets?)(\.[A-Za-z0-9_-]+)?\.(json|ya?ml|ini|txt)$",
            r"(?i)\.(pem|key|p12|pfx|keystore|jks|license)$",
        ]
        .iter()
        .map(|r| Regex::new(r).unwrap())
        .collect()
    });
    let p = path.replace('\\', "/");
    res.iter().any(|re| re.is_match(&p))
}

/// Always a boolean (the workspace compares it with `!==` against a stored flag).
pub fn is_textual(path: &str, size: Option<u64>) -> bool {
    if is_secret(path) {
        return false;
    }
    let e = ext_of(path);
    (is_js_ext(&e) || TEXT.contains(&e.as_str()) || lang_for_ext(&e).is_some()) && size.map_or(true, |s| s == 0 || s <= TEXT_MAX)
}

// ── source geometry ─────────────────────────────────────────────────────────

/// Byte offsets (oxc) -> UTF-16 offsets and acorn-style line numbers.
pub struct Geo<'s> {
    pub s: &'s str,
    map: Option<Vec<u32>>,
    /// byte offset of each line start; acorn breaks on \n, \r\n, \r, U+2028, U+2029
    starts: Vec<usize>,
}

impl<'s> Geo<'s> {
    pub fn new(s: &'s str) -> Self {
        let map = if s.is_ascii() {
            None
        } else {
            let mut m = vec![0u32; s.len() + 1];
            let mut u = 0u32;
            for (b, c) in s.char_indices() {
                for k in 0..c.len_utf8() {
                    m[b + k] = u;
                }
                u += c.len_utf16() as u32;
            }
            m[s.len()] = u;
            Some(m)
        };
        let bytes = s.as_bytes();
        let mut starts = vec![0usize];
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'\n' => starts.push(i + 1),
                b'\r' => {
                    if bytes.get(i + 1) == Some(&b'\n') {
                        i += 1;
                    }
                    starts.push(i + 1);
                }
                0xE2 if bytes.get(i + 1) == Some(&0x80) && matches!(bytes.get(i + 2), Some(0xA8) | Some(0xA9)) => {
                    starts.push(i + 3);
                    i += 2;
                }
                _ => {}
            }
            i += 1;
        }
        Geo { s, map, starts }
    }
    pub fn u16(&self, b: usize) -> usize {
        match &self.map {
            None => b,
            Some(m) => m[b.min(self.s.len())] as usize,
        }
    }
    pub fn line(&self, b: usize) -> usize {
        self.starts.partition_point(|&st| st <= b)
    }
    /// acorn's 0-based column
    pub fn col0(&self, b: usize) -> usize {
        let l = self.line(b);
        self.u16(b) - self.u16(self.starts[l - 1])
    }
    fn text(&self, sp: Span) -> &'s str {
        &self.s[sp.start as usize..sp.end as usize]
    }
}

// ── parsing ─────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
pub enum Goal {
    Script,
    Module,
    CommonJs,
}

pub struct Fail {
    pub message: String,
    pub pos: usize, // byte offset
    pub len: usize,
}

pub fn parse<'a>(alloc: &'a Allocator, src: &'a str, goal: Goal, jsx: bool) -> ParserReturn<'a> {
    let mut st = match goal {
        Goal::Script => SourceType::script(),
        Goal::Module => SourceType::mjs(),
        Goal::CommonJs => SourceType::cjs(),
    };
    if jsx {
        st = st.with_jsx(true);
    }
    let opts = ParseOptions {
        preserve_parens: false,
        allow_return_outside_function: goal == Goal::CommonJs,
        ..ParseOptions::default()
    };
    Parser::new(alloc, src, st).with_options(opts).parse()
}

pub fn fail_of(r: &ParserReturn) -> Option<Fail> {
    if !r.fatal_error && !r.diagnostics.has_errors() {
        let sem = oxc_semantic::SemanticBuilder::new().with_check_syntax_error(true).build(&r.program);
        let d = sem.diagnostics.errors().next()?;
        let (pos, len) = d.labels.last().map(|l| (l.offset(), l.len())).unwrap_or((0, 0));
        let m = d.message.to_string();
        let m = if m.ends_with("has already been declared") { m.replace('`', "'") } else { m };
        return Some(Fail { message: m, pos: pos as usize, len: len as usize });
    }
    let d = r.diagnostics.errors().next();
    let (pos, len, message) = match d {
        Some(d) => {
            let l = d.labels.first().map(|l| (l.offset() as usize, l.len() as usize)).unwrap_or((0, 0));
            (l.0, l.1, d.message.to_string())
        }
        None => (0, 0, "parse error".to_string()),
    };
    Some(Fail { message, pos, len })
}

/// acorn-flavoured message text: "Unexpected token (line:col)".
pub fn acorn_message(g: &Geo, f: &Fail) -> String {
    let base = if f.message.starts_with("Unexpected token") || f.message.contains("end of file") || f.message.starts_with("Expected") {
        "Unexpected token".to_string()
    } else {
        f.message.clone()
    };
    let p = f.pos.min(g.s.len());
    format!("{base} ({}:{})", g.line(p), g.col0(p))
}

// ── helpers over the oxc AST ────────────────────────────────────────────────

fn collapse_ws(s: &str) -> String {
    let mut out = String::new();
    let mut in_ws = false;
    for c in s.chars() {
        if is_js_ws(c as u32) {
            in_ws = true;
        } else {
            if in_ws {
                out.push(' ');
                in_ws = false;
            }
            out.push(c);
        }
    }
    if in_ws {
        out.push(' ');
    }
    js_trim(&out).to_string()
}

fn js_num(v: f64) -> String {
    if v == 0.0 {
        return "0".into();
    }
    if !v.is_finite() {
        return if v.is_nan() { "NaN".into() } else if v > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    let neg = v < 0.0;
    let e = format!("{:e}", v.abs());
    let (mant, exp) = e.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let n = exp + 1;
    let body = if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let e = n - 1;
        let sign = if e < 0 { '-' } else { '+' };
        if k == 1 {
            format!("{digits}e{sign}{}", e.abs())
        } else {
            format!("{}.{}e{sign}{}", &digits[..1], &digits[1..], e.abs())
        }
    };
    if neg {
        format!("-{body}")
    } else {
        body
    }
}

fn num_value(v: f64) -> Value {
    if v.fract() == 0.0 && v.abs() < 9e15 {
        json!(v as i64)
    } else {
        json!(v)
    }
}

const KEY_TEXT_MAX: usize = 120;

/// The name a member is addressed by (memberKeyName).
fn key_name(g: &Geo, key: &PropertyKey, computed: bool) -> Option<String> {
    if computed {
        match key {
            PropertyKey::StringLiteral(l) => return Some(l.value.to_string()),
            PropertyKey::NumericLiteral(l) => return Some(js_num(l.value)),
            PropertyKey::BooleanLiteral(l) => return Some(l.value.to_string()),
            PropertyKey::BigIntLiteral(l) => return Some(l.raw.as_ref().map(|r| r.trim_end_matches('n').to_string()).unwrap_or_default()),
            PropertyKey::TemplateLiteral(t) if t.expressions.is_empty() && t.quasis.len() == 1 => {
                if let Some(c) = &t.quasis[0].value.cooked {
                    if !c.is_empty() {
                        return Some(c.to_string());
                    }
                }
            }
            _ => {}
        }
        let text = collapse_ws(g.text(key.span()));
        if text.is_empty() {
            return None;
        }
        let cut = slice_units(&text, KEY_TEXT_MAX);
        return Some(if cut.len() < text.len() { format!("[{cut}\u{2026}]") } else { format!("[{text}]") });
    }
    match key {
        PropertyKey::StaticIdentifier(i) => Some(i.name.to_string()).filter(|s| !s.is_empty()),
        PropertyKey::PrivateIdentifier(i) => Some(i.name.to_string()).filter(|s| !s.is_empty()),
        PropertyKey::StringLiteral(l) => Some(l.value.to_string()),
        PropertyKey::NumericLiteral(l) => Some(js_num(l.value)),
        PropertyKey::BigIntLiteral(l) => l.raw.as_ref().map(|r| r.trim_end_matches('n').to_string()),
        PropertyKey::Identifier(i) => Some(i.name.to_string()),
        _ => None,
    }
}

/// `p.key.name || p.key.value` as a JSON value.
fn key_simple(key: &PropertyKey) -> Value {
    match key {
        PropertyKey::StaticIdentifier(i) => json!(i.name.as_str()),
        PropertyKey::Identifier(i) => json!(i.name.as_str()),
        PropertyKey::StringLiteral(l) => json!(l.value.as_str()),
        PropertyKey::NumericLiteral(l) => num_value(l.value),
        _ => Value::Null,
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::String(s) => !s.is_empty(),
        Value::Number(n) => n.as_f64().map_or(true, |f| f != 0.0),
        _ => true,
    }
}

fn mod_name(n: &ModuleExportName) -> Value {
    match n {
        ModuleExportName::IdentifierName(i) => json!(i.name.as_str()),
        ModuleExportName::IdentifierReference(i) => json!(i.name.as_str()),
        ModuleExportName::StringLiteral(l) => json!(l.value.as_str()),
    }
}

#[derive(Clone, Copy)]
enum FnRef<'x, 'a> {
    F(&'x Function<'a>),
    A(&'x ArrowFunctionExpression<'a>),
}

fn fn_of<'x, 'a>(e: &'x Expression<'a>) -> Option<FnRef<'x, 'a>> {
    match e {
        Expression::FunctionExpression(f) => Some(FnRef::F(f)),
        Expression::ArrowFunctionExpression(a) => Some(FnRef::A(a)),
        _ => None,
    }
}

/// The leading comment above a declaration, prose joined (docAbove).
fn doc_above(g: &Geo, node_start: usize, prev_end: usize) -> String {
    let before = if prev_end <= node_start { &g.s[prev_end..node_start] } else { "" };
    let lines: Vec<&str> = before.split('\n').map(js_trim).collect();
    let mut doc: Vec<String> = Vec::new();
    for l in lines.iter().rev() {
        if l.is_empty() {
            if !doc.is_empty() {
                break;
            }
            continue;
        }
        if let Some(rest) = l.strip_prefix("//") {
            let mut cs = rest.chars();
            let s = match cs.clone().next() {
                Some(c) if is_js_ws(c as u32) => {
                    cs.next();
                    cs.as_str()
                }
                _ => rest,
            };
            doc.insert(0, s.to_string());
            continue;
        }
        if l.ends_with("*/") || l.starts_with('*') || l.starts_with("/*") {
            doc.insert(0, strip_block_markers(l));
            continue;
        }
        break;
    }
    slice_units(js_trim(&doc.join(" ")), 400)
}

/// l.replace(/^\/\*+\s?|\s?\*+\/$|^\*\s?/g, '')
fn strip_block_markers(l: &str) -> String {
    let c: Vec<char> = l.chars().collect();
    let n = c.len();
    let ws = |ch: char| is_js_ws(ch as u32);
    let mut out = String::new();
    let mut i = 0;
    while i < n {
        if i == 0 && c[0] == '/' && n > 1 && c[1] == '*' {
            let mut j = 1;
            while j < n && c[j] == '*' {
                j += 1;
            }
            if j < n && ws(c[j]) {
                j += 1;
            }
            i = j;
            continue;
        }
        // \s?\*+\/$
        let tail = |from: usize| -> bool {
            let mut k = from;
            while k < n && c[k] == '*' {
                k += 1;
            }
            k > from && k == n - 1 && c[k] == '/'
        };
        if (ws(c[i]) && tail(i + 1)) || tail(i) {
            break;
        }
        if i == 0 && c[0] == '*' {
            i = 1;
            if i < n && ws(c[i]) {
                i += 1;
            }
            continue;
        }
        out.push(c[i]);
        i += 1;
    }
    out
}

fn params_text(g: &Geo, p: &FormalParameters) -> String {
    let mut parts: Vec<String> = Vec::new();
    for it in &p.items {
        let sp = match &it.pattern {
            BindingPattern::AssignmentPattern(a) => a.left.span(),
            other => other.span(),
        };
        parts.push(collapse_ws(g.text(sp)));
    }
    if let Some(r) = &p.rest {
        parts.push(collapse_ws(g.text(r.rest.span)));
    }
    parts.join(", ")
}

fn signature(g: &Geo, f: FnRef, name: &str) -> String {
    let (is_async, params) = match f {
        FnRef::F(f) => (f.r#async, params_text(g, &f.params)),
        FnRef::A(a) => (a.r#async, params_text(g, &a.params)),
    };
    format!("{}{name}({params})", if is_async { "async " } else { "" })
}

fn bound_names(p: &BindingPattern, out: &mut Vec<String>) {
    match p {
        BindingPattern::BindingIdentifier(i) => out.push(i.name.to_string()),
        BindingPattern::ObjectPattern(o) => {
            for q in &o.properties {
                bound_names(&q.value, out);
            }
            if let Some(r) = &o.rest {
                bound_names(&r.argument, out);
            }
        }
        BindingPattern::ArrayPattern(a) => {
            for q in a.elements.iter().flatten() {
                bound_names(q, out);
            }
            if let Some(r) = &a.rest {
                bound_names(&r.argument, out);
            }
        }
        BindingPattern::AssignmentPattern(a) => bound_names(&a.left, out),
    }
}

fn import_names(specs: &Option<oxc_allocator::Vec<ImportDeclarationSpecifier>>) -> Value {
    let mut v = Vec::new();
    if let Some(specs) = specs {
        for s in specs.iter() {
            v.push(match s {
                ImportDeclarationSpecifier::ImportDefaultSpecifier(d) => json!({"local": d.local.name.as_str(), "imported": "default"}),
                ImportDeclarationSpecifier::ImportNamespaceSpecifier(d) => json!({"local": d.local.name.as_str(), "imported": "*"}),
                ImportDeclarationSpecifier::ImportSpecifier(d) => json!({"local": d.local.name.as_str(), "imported": mod_name(&d.imported)}),
            });
        }
    }
    Value::Array(v)
}

// ── call sites ──────────────────────────────────────────────────────────────

struct Calls<'g, 's> {
    g: &'g Geo<'s>,
    refs: Vec<Value>,
}

impl<'g, 's> Calls<'g, 's> {
    fn callee_name(e: &Expression) -> Option<String> {
        match e {
            Expression::Identifier(i) => Some(i.name.to_string()),
            Expression::StaticMemberExpression(m) => Some(m.property.name.to_string()),
            _ => None,
        }
    }
}

impl<'a, 'g, 's> Visit<'a> for Calls<'g, 's> {
    fn visit_call_expression(&mut self, it: &CallExpression<'a>) {
        if let Some(name) = Self::callee_name(&it.callee) {
            self.refs.push(json!({"name": name, "kind": "call", "line": self.g.line(it.span.start as usize)}));
        }
        walk::walk_call_expression(self, it);
    }
    fn visit_new_expression(&mut self, it: &NewExpression<'a>) {
        if let Some(name) = Self::callee_name(&it.callee) {
            self.refs.push(json!({"name": name, "kind": "call", "line": self.g.line(it.span.start as usize)}));
        }
        walk::walk_new_expression(self, it);
    }
}

// ── extractor ───────────────────────────────────────────────────────────────

struct Ex<'g, 's> {
    g: &'g Geo<'s>,
    symbols: Vec<Map<String, Value>>,
    imports: Vec<Value>,
    exports: Vec<Value>,
}

impl<'g, 's> Ex<'g, 's> {
    fn push(&mut self, name: &str, kind: &str, span: Span, parent: Option<&str>, prev_end: usize, extra: Vec<(&str, Value)>) -> String {
        let path = match parent {
            Some(p) => format!("{p}.{name}"),
            None => name.to_string(),
        };
        let (s, e) = (span.start as usize, span.end as usize);
        let mut m = Map::new();
        m.insert("name".into(), json!(name));
        m.insert("kind".into(), json!(kind));
        m.insert("path".into(), json!(path));
        if let Some(p) = parent {
            m.insert("parent".into(), json!(p));
        }
        m.insert("start".into(), json!(self.g.u16(s)));
        m.insert("end".into(), json!(self.g.u16(e)));
        m.insert("lineStart".into(), json!(self.g.line(s)));
        m.insert("lineEnd".into(), json!(self.g.line(e)));
        m.insert("doc".into(), json!(doc_above(self.g, s, prev_end)));
        for (k, v) in extra {
            m.insert(k.into(), v);
        }
        self.symbols.push(m);
        path
    }

    /// The function-valued members of an object literal.
    fn members(&mut self, obj: &Expression, parent: Option<&str>, depth: usize) {
        if let Expression::ObjectExpression(o) = obj {
            self.members_of(o, parent, depth);
        }
    }

    /// Into a function body: its inner declarations, or the object it returns.
    fn descend(&mut self, f: FnRef, parent: &str, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        let body: &FunctionBody = match f {
            FnRef::F(f) => match &f.body {
                Some(b) => b,
                None => return,
            },
            FnRef::A(a) => match &a.body {
                ArrowFunctionBody::FunctionBody(b) => b,
                other => {
                    if let Some(e) = other.as_expression() {
                        self.members(e, Some(parent), depth);
                    }
                    return;
                }
            },
        };
        let mut prev_end = body.span.start as usize;
        if let Some(d) = body.directives.last() {
            prev_end = d.span.end as usize;
        }
        for st in body.statements.iter() {
            self.statement(st, Some(parent), depth, prev_end);
            prev_end = st.span().end as usize;
        }
    }

    fn is_require<'x, 'a>(e: &'x Expression<'a>) -> Option<&'x CallExpression<'a>> {
        if let Expression::CallExpression(c) = e {
            if let Expression::Identifier(i) = &c.callee {
                if i.name == "require" {
                    return Some(c);
                }
            }
        }
        None
    }

    fn first_arg_value(c: &CallExpression) -> Value {
        match c.arguments.first() {
            Some(Argument::StringLiteral(l)) => json!(l.value.as_str()),
            Some(Argument::NumericLiteral(l)) => num_value(l.value),
            _ => Value::Null,
        }
    }

    /// `const a = …, b = () => {}` — one symbol per named declarator.
    fn variables(&mut self, decl: &VariableDeclaration, span: Span, parent: Option<&str>, depth: usize, prev_end: usize, exported: bool) {
        let decl_line = self.g.line(decl.span.start as usize);
        for d in decl.declarations.iter() {
            let req = d.init.as_ref().and_then(Self::is_require);
            if parent.is_none() && !exported {
                if let (BindingPattern::ObjectPattern(op), Some(c)) = (&d.id, req) {
                    let from = Self::first_arg_value(c);
                    let mut names = Vec::new();
                    for p in op.properties.iter() {
                        if matches!(p.value, BindingPattern::BindingIdentifier(_)) {
                            if let BindingPattern::BindingIdentifier(i) = &p.value {
                                names.push(json!({"local": i.name.as_str(), "imported": key_simple(&p.key)}));
                            }
                        }
                    }
                    if from.is_string() {
                        self.imports.push(json!({"local": null, "from": from, "line": decl_line, "names": names}));
                    }
                    continue;
                }
            }
            let BindingPattern::BindingIdentifier(id) = &d.id else { continue };
            if parent.is_none() && !exported {
                if let Some(c) = req {
                    let from = Self::first_arg_value(c);
                    if truthy(&from) {
                        self.imports.push(json!({"local": id.name.as_str(), "from": from, "line": decl_line}));
                    }
                    continue;
                }
            }
            let f = d.init.as_ref().and_then(fn_of);
            // Inside a scope only FUNCTIONS earn a name.
            if f.is_none() && parent.is_some() {
                continue;
            }
            let mut extra = Vec::new();
            if let Some(f) = f {
                extra.push(("signature", json!(signature(self.g, f, id.name.as_str()))));
            }
            if exported {
                extra.push(("exported", json!(true)));
            }
            let child = self.push(id.name.as_str(), if f.is_some() { "function" } else { "const" }, span, parent, prev_end, extra);
            if let Some(f) = f {
                self.descend(f, &child, depth + 1);
            }
        }
    }

    fn class_members(&mut self, c: &Class, child: &str, depth: usize) {
        for m in c.body.body.iter() {
            let (name, f, span) = match m {
                ClassElement::MethodDefinition(m) => (key_name(self.g, &m.key, m.computed), Some(FnRef::F(&m.value)), m.span),
                ClassElement::PropertyDefinition(p) => (key_name(self.g, &p.key, p.computed), p.value.as_ref().and_then(fn_of), p.span),
                ClassElement::AccessorProperty(p) => (key_name(self.g, &p.key, p.computed), p.value.as_ref().and_then(fn_of), p.span),
                _ => continue,
            };
            let Some(name) = name.filter(|n| !n.is_empty()) else { continue };
            if depth + 1 > MAX_DEPTH {
                continue;
            }
            // `push(…, { parent })` gives no prevEnd: the doc scan starts at 0, as in the JS.
            let extra = match f {
                Some(f) => vec![("signature", json!(signature(self.g, f, &name)))],
                None => vec![],
            };
            self.push(&name, "method", span, Some(child), 0, extra);
            if let Some(f) = f {
                self.descend(f, &format!("{child}.{name}"), depth + 2);
            }
        }
    }

    fn statement(&mut self, node: &Statement, parent: Option<&str>, depth: usize, prev_end: usize) {
        match node {
            Statement::FunctionDeclaration(f) if f.id.is_some() => {
                let name = f.id.as_ref().unwrap().name.as_str();
                let sig = signature(self.g, FnRef::F(f), name);
                let child = self.push(name, "function", f.span, parent, prev_end, vec![("signature", json!(sig))]);
                self.descend(FnRef::F(f), &child, depth + 1);
                return;
            }
            Statement::ClassDeclaration(c) if c.id.is_some() => {
                let name = c.id.as_ref().unwrap().name.as_str();
                let child = self.push(name, "class", c.span, parent, prev_end, vec![]);
                self.class_members(c, &child, depth);
                return;
            }
            Statement::VariableDeclaration(v) => {
                self.variables(v, v.span, parent, depth, prev_end, false);
                return;
            }
            _ => {}
        }
        if parent.is_some() {
            if let Statement::ReturnStatement(r) = node {
                if let Some(a) = &r.argument {
                    self.members(a, parent, depth);
                }
            }
            return;
        }
        let line = self.g.line(node.span().start as usize);
        match node {
            Statement::ImportDeclaration(i) => {
                self.imports.push(json!({
                    "local": null, "from": i.source.value.as_str(), "line": line, "esm": true,
                    "names": import_names(&i.specifiers),
                }));
            }
            Statement::ExportDeclaration(e) => {
                self.export_decl(&e.declaration, e.span, depth, prev_end);
            }
            Statement::ExportNamedDeclaration(e) => {
                for s in e.specifiers.iter() {
                    self.exports.push(mod_name(&s.exported));
                }
            }
            Statement::ExportFromDeclaration(e) => {
                for s in e.specifiers.iter() {
                    self.exports.push(mod_name(&s.exported));
                }
                let names: Vec<Value> = e
                    .specifiers
                    .iter()
                    .map(|s| {
                        let local = match &s.local {
                            ModuleExportName::StringLiteral(_) => Value::Null,
                            other => mod_name(other),
                        };
                        json!({"local": local, "imported": mod_name(&s.exported)})
                    })
                    .collect();
                self.imports.push(json!({
                    "local": null, "from": e.source.value.as_str(), "line": line, "esm": true, "reexport": true, "names": names,
                }));
            }
            Statement::ExportDefaultDeclaration(e) => self.export_default(e, depth, prev_end),
            Statement::ExportAllDeclaration(e) => {
                if let Some(x) = &e.exported {
                    self.exports.push(mod_name(x));
                }
                self.imports.push(json!({
                    "local": null, "from": e.source.value.as_str(), "line": line, "esm": true, "reexport": true, "names": null,
                }));
            }
            Statement::ExpressionStatement(es) => {
                if let Expression::AssignmentExpression(a) = &es.expression {
                    if self.g.text(a.left.span()).starts_with("module.exports") {
                        match &a.right {
                            Expression::ObjectExpression(o) => {
                                for p in o.properties.iter() {
                                    if let ObjectPropertyKind::ObjectProperty(p) = p {
                                        let k = key_simple(&p.key);
                                        if truthy(&k) {
                                            self.exports.push(k);
                                        }
                                    }
                                }
                            }
                            Expression::Identifier(i) => self.exports.push(json!(i.name.as_str())),
                            _ => {}
                        }
                        let snap = Value::Array(self.exports.clone());
                        self.push("module.exports", "exports", es.span, None, prev_end, vec![("exports", snap)]);
                    }
                }
            }
            _ => {}
        }
    }

    fn export_decl(&mut self, decl: &Declaration, span: Span, depth: usize, prev_end: usize) {
        match decl {
            Declaration::VariableDeclaration(v) => {
                self.variables(v, span, None, depth, prev_end, true);
                for d in v.declarations.iter() {
                    let mut names = Vec::new();
                    bound_names(&d.id, &mut names);
                    self.exports.extend(names.into_iter().map(Value::from));
                }
            }
            Declaration::FunctionDeclaration(f) if f.id.is_some() => {
                let name = f.id.as_ref().unwrap().name.as_str();
                let sig = signature(self.g, FnRef::F(f), name);
                let child = self.push(name, "function", span, None, prev_end, vec![("exported", json!(true)), ("signature", json!(sig))]);
                self.descend(FnRef::F(f), &child, depth + 1);
                self.exports.push(json!(name));
            }
            Declaration::ClassDeclaration(c) if c.id.is_some() => {
                let name = c.id.as_ref().unwrap().name.as_str();
                self.push(name, "class", span, None, prev_end, vec![("exported", json!(true))]);
                // descend() on a class has no BlockStatement body: members of an exported class are not extracted.
                self.exports.push(json!(name));
            }
            _ => {}
        }
    }

    fn export_default(&mut self, e: &ExportDefaultDeclaration, depth: usize, prev_end: usize) {
        match &e.declaration {
            ExportDefaultDeclarationKind::FunctionDeclaration(f) if f.id.is_some() => {
                let name = f.id.as_ref().unwrap().name.as_str();
                let sig = signature(self.g, FnRef::F(f), name);
                let child = self.push(name, "function", e.span, None, prev_end, vec![("exported", json!(true)), ("signature", json!(sig))]);
                self.descend(FnRef::F(f), &child, depth + 1);
                self.exports.push(json!("default"));
            }
            ExportDefaultDeclarationKind::ClassDeclaration(c) if c.id.is_some() => {
                let name = c.id.as_ref().unwrap().name.as_str();
                self.push(name, "class", e.span, None, prev_end, vec![("exported", json!(true))]);
                self.exports.push(json!("default"));
            }
            ExportDefaultDeclarationKind::ObjectExpression(o) => {
                // `export default { ... }`: function-valued members are addressable, the object itself is not.
                self.members_of(o, None, depth);
                self.exports.push(json!("default"));
            }
            _ => self.exports.push(json!("default")),
        }
    }

    fn members_of(&mut self, obj: &ObjectExpression, parent: Option<&str>, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        let mut prev_end = obj.span.start as usize;
        for p in obj.properties.iter() {
            match p {
                ObjectPropertyKind::ObjectProperty(p) => {
                    let name = key_name(self.g, &p.key, p.computed);
                    if let (Some(name), Some(f)) = (name.filter(|n| !n.is_empty()), fn_of(&p.value)) {
                        let sig = signature(self.g, f, &name);
                        let child = self.push(&name, "method", p.span, parent, prev_end, vec![("signature", json!(sig))]);
                        self.descend(f, &child, depth + 1);
                    }
                    prev_end = p.span.end as usize;
                }
                ObjectPropertyKind::SpreadProperty(s) => prev_end = s.span.end as usize,
            }
        }
    }
}

pub struct Extracted {
    pub symbols: Value,
    pub imports: Value,
    pub exports: Value,
    pub references: Value,
}

/// extractJavaScript over an already-parsed program.
pub fn extract_javascript(g: &Geo, program: &Program) -> Extracted {
    let mut ex = Ex { g, symbols: vec![], imports: vec![], exports: vec![] };
    let mut prev_end = 0usize;
    if let Some(d) = program.directives.last() {
        prev_end = d.span.end as usize;
    }
    for st in program.body.iter() {
        ex.statement(st, None, 0, prev_end);
        prev_end = st.span().end as usize;
    }
    // Mark the symbols the file actually exposes (top-level only).
    let names: std::collections::HashSet<&str> = ex.exports.iter().filter_map(|v| v.as_str()).collect();
    for s in ex.symbols.iter_mut() {
        if !s.contains_key("parent") && names.contains(s["name"].as_str().unwrap_or("")) {
            s.insert("exported".into(), json!(true));
        }
    }
    let mut calls = Calls { g, refs: vec![] };
    calls.visit_program(program);
    Extracted {
        symbols: Value::Array(ex.symbols.into_iter().map(Value::Object).collect()),
        imports: Value::Array(ex.imports),
        exports: Value::Array(ex.exports),
        references: Value::Array(calls.refs),
    }
}

// ── extract() ───────────────────────────────────────────────────────────────

fn truncate160(s: &str) -> String {
    slice_units(s, 160)
}

fn empty_record(path: &str, lang: &str, lines: usize, parsed: bool, indexed: bool, reason: Option<String>) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("path".into(), json!(path));
    m.insert("lang".into(), json!(lang));
    m.insert("lines".into(), json!(lines));
    m.insert("symbols".into(), json!([]));
    m.insert("imports".into(), json!([]));
    m.insert("exports".into(), json!([]));
    m.insert("parsed".into(), json!(parsed));
    m.insert("indexed".into(), json!(indexed));
    if let Some(r) = reason {
        m.insert("reason".into(), json!(r));
    }
    m
}

pub fn okjs_owns(path: &str) -> bool {
    let p = path.to_lowercase();
    p.ends_with(".ok.js") || p.ends_with(".ok.mjs") || p.ends_with(".ok.html")
}

/// Languages whose extractor lives in tree-sitter (or a registered custom one), out of this crate's scope.
fn foreign_lang(lang: &str) -> bool {
    matches!(lang, "typescript" | "python" | "go" | "rust" | "php" | "java" | "csharp" | "c" | "cpp" | "yaml")
}

pub fn extract(path: &str, src: Option<&str>) -> Result<Value, String> {
    let lang = lang_of(path);
    let Some(src) = src else {
        let reason = if is_secret(path) { "may hold credentials \u{2014} hashed, never read" } else { "binary or oversized \u{2014} hashed, not read" };
        return Ok(Value::Object(empty_record(path, lang, 0, false, false, Some(reason.into()))));
    };
    let lines = src.split('\n').count();
    if okjs_owns(path) {
        return Err("out-of-scope: owned by the okjs extension (okcode-okjs)".into());
    }
    if foreign_lang(lang) {
        return Err(format!("out-of-scope: {lang} is extracted by tree-sitter (okcode-treesitter)"));
    }
    if lang != "javascript" {
        return Ok(Value::Object(empty_record(path, lang, lines, false, true, Some(format!("{lang} \u{2014} searchable, no symbol extractor yet")))));
    }
    let analyzer = format!("{JS_ANALYZER_VERSION}+jsx:{JSX_TAG}");
    let g = Geo::new(src);
    let alloc = Allocator::default();
    // script first, then module (an .mjs parsed as a script fails on `import`).
    let r = parse(&alloc, src, Goal::Script, false);
    let mut ok = fail_of(&r).is_none().then_some(r);
    let mut last_fail = None;
    if ok.is_none() {
        let r = parse(&alloc, src, Goal::Module, false);
        match fail_of(&r) {
            None => ok = Some(r),
            Some(f) => last_fail = Some(f),
        }
    }
    if let Some(r) = ok {
        let x = extract_javascript(&g, &r.program);
        let mut m = empty_record(path, lang, lines, true, true, None);
        m.insert("symbols".into(), x.symbols);
        m.insert("imports".into(), x.imports);
        m.insert("exports".into(), x.exports);
        m.insert("references".into(), x.references);
        m.insert("analyzerVersion".into(), json!(analyzer));
        m.insert("extractor".into(), json!("acorn"));
        return Ok(Value::Object(m));
    }
    let fail = last_fail.unwrap();
    // acorn cannot parse JSX; oxc can. The JS build fell back to tree-sitter's TSX grammar and stamped it.
    if src.as_bytes().get(fail.pos) == Some(&b'<') {
        let alloc2 = Allocator::default();
        let r = parse(&alloc2, src, Goal::Module, true);
        if fail_of(&r).is_none() {
            let x = extract_javascript(&g, &r.program);
            let mut m = empty_record(path, lang, lines, true, true, Some("acorn cannot parse JSX; extracted via the tree-sitter TSX grammar".into()));
            m.insert("symbols".into(), x.symbols);
            m.insert("imports".into(), x.imports);
            m.insert("exports".into(), x.exports);
            m.insert("references".into(), x.references);
            m.insert("analyzerVersion".into(), json!(analyzer));
            m.insert("extractor".into(), json!("treesitter-tsx"));
            return Ok(Value::Object(m));
        }
    }
    let mut m = empty_record(path, lang, lines, false, true, Some(truncate160(&acorn_message(&g, &fail))));
    m.insert("analyzerVersion".into(), json!(analyzer));
    Ok(Value::Object(m))
}

// ── parseSource / extractJavaScript as standalone exports ───────────────────

/// parse.js#parseSource: script first, then module; the last failure is the error (acorn's wording).
pub fn check_parse_source(src: &str) -> Result<&'static str, String> {
    let g = Geo::new(src);
    let alloc = Allocator::default();
    if fail_of(&parse(&alloc, src, Goal::Script, false)).is_none() {
        return Ok("script");
    }
    let r = parse(&alloc, src, Goal::Module, false);
    match fail_of(&r) {
        None => Ok("module"),
        Some(f) => Err(acorn_message(&g, &f)),
    }
}

/// parse.js#extractJavaScript: `{symbols, imports, exports, references}`; Err is the parse failure.
pub fn extract_javascript_source(src: &str) -> Result<Value, String> {
    let g = Geo::new(src);
    let alloc = Allocator::default();
    let r = parse(&alloc, src, Goal::Script, false);
    let r = if fail_of(&r).is_none() {
        r
    } else {
        let r = parse(&alloc, src, Goal::Module, false);
        if let Some(f) = fail_of(&r) {
            return Err(acorn_message(&g, &f));
        }
        r
    };
    let x = extract_javascript(&g, &r.program);
    Ok(json!({"symbols": x.symbols, "imports": x.imports, "exports": x.exports, "references": x.references}))
}
