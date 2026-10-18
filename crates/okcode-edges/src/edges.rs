//! Resolved import/export edges (lib/edges.js). Derived from per-file rows
//! (imports + symbols) plus a few config files read through `Access`.
//! Unresolvable stays unresolved, never guessed.
use crate::packages::Access;
use regex::Regex;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::OnceLock;

macro_rules! re {
    ($p:expr) => {{
        static R: OnceLock<Regex> = OnceLock::new();
        R.get_or_init(|| Regex::new($p).unwrap())
    }};
}

// ── input rows ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Name {
    pub local: Option<String>,
    pub imported: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Import {
    /// None for a Rust `mod foo;` (kind `modpath`).
    pub from: Option<String>,
    pub line: Option<i64>,
    pub kind: Option<String>,
    pub reexport: bool,
    /// None means no names (wildcard / not applicable); Some(vec) lists them.
    pub names: Option<Vec<Name>>,
}

#[derive(Debug, Clone, Default)]
pub struct Symbol {
    pub kind: String,
    pub name: String,
    pub parent: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct FileRow {
    pub rel: String,
    pub lang: Option<String>,
    pub imports: Vec<Import>,
    pub symbols: Vec<Symbol>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Edge {
    pub from: Option<String>,
    pub to: String,
    pub name: Option<String>,
    pub line: Option<i64>,
    pub kind: Option<String>,
    pub reexport: bool,
    pub names: Option<Vec<Name>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Importer {
    pub file: String,
    pub line: Option<i64>,
    pub kind: Option<String>,
    pub names: Option<Vec<Name>>,
    pub via: Option<String>,
}

// ── posix path helpers (node path.posix subset) ─────────────────────────

fn normalize(p: &str) -> String {
    if p.is_empty() {
        return ".".into();
    }
    let abs = p.starts_with('/');
    let trailing = p.ends_with('/');
    let mut out: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if matches!(out.last(), Some(l) if *l != "..") {
                    out.pop();
                } else if !abs {
                    out.push("..");
                }
            }
            s => out.push(s),
        }
    }
    let mut r = out.join("/");
    if r.is_empty() && !abs {
        r = ".".into();
    }
    if trailing && !r.is_empty() {
        r.push('/');
    }
    if abs {
        format!("/{r}")
    } else {
        r
    }
}

fn join(a: &str, b: &str) -> String {
    let parts: Vec<&str> = [a, b].into_iter().filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        ".".into()
    } else {
        normalize(&parts.join("/"))
    }
}

fn dirname(p: &str) -> String {
    let t = p.trim_end_matches('/');
    if t.is_empty() {
        return if p.starts_with('/') { "/".into() } else { ".".into() };
    }
    match t.rfind('/') {
        None => ".".into(),
        Some(0) => "/".into(),
        Some(i) => t[..i].to_string(),
    }
}

fn basename(p: &str) -> &str {
    let t = p.trim_end_matches('/');
    t.rsplit('/').next().unwrap_or("")
}

/// dirname with the root dir spelled `""` (the JS `=== '.' ? '' : …` idiom).
fn dir_or_root(p: &str) -> String {
    let d = dirname(p);
    if d == "." { String::new() } else { d }
}

fn is_relative_specifier(spec: &str) -> bool {
    spec.starts_with('.') || spec.starts_with('/')
}

fn norm_join(dir: &str, spec: &str) -> String {
    let raw = if let Some(s) = spec.strip_prefix('/') { s.to_string() } else { join(dir, spec) };
    let mut n = normalize(&raw);
    while let Some(r) = n.strip_prefix("../") {
        n = r.to_string(); // never escape the workspace root
    }
    n
}

// ── JS/TS ───────────────────────────────────────────────────────────────

const JS_EXTS: [&str; 10] = ["", ".ts", ".tsx", ".d.ts", ".mts", ".cts", ".js", ".mjs", ".cjs", ".jsx"];
const JS_INDEXES: [&str; 9] = [
    "index.ts", "index.tsx", "index.d.ts", "index.mts", "index.cts", "index.js", "index.mjs", "index.cjs", "index.jsx",
];

fn resolve_js_path(norm: &str, index: &HashSet<String>) -> Option<String> {
    for ext in JS_EXTS {
        let c = format!("{norm}{ext}");
        if index.contains(&c) {
            return Some(c);
        }
    }
    for f in JS_INDEXES {
        let cand = if norm.is_empty() { f.to_string() } else { format!("{norm}/{f}") };
        if index.contains(&cand) {
            return Some(cand);
        }
    }
    None
}

/// Strip // and /* */ comments without touching strings; drop trailing commas.
pub fn strip_jsonc(text: &str) -> String {
    let t: Vec<char> = text.chars().collect();
    let n = t.len();
    let mut out = String::new();
    let mut i = 0;
    while i < n {
        let c = t[i];
        if c == '"' || c == '\'' {
            out.push(c);
            i += 1;
            while i < n {
                out.push(t[i]);
                if t[i] == '\\' {
                    i += 1;
                    if i < n {
                        out.push(t[i]);
                        i += 1;
                    }
                    continue;
                }
                if t[i] == c {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        if c == '/' && i + 1 < n && t[i + 1] == '/' {
            while i < n && t[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && i + 1 < n && t[i + 1] == '*' {
            i += 2;
            while i < n && !(t[i] == '*' && i + 1 < n && t[i + 1] == '/') {
                i += 1;
            }
            i += 2;
            continue;
        }
        out.push(c);
        i += 1;
    }
    re!(r",(\s*[}\]])").replace_all(&out, "$1").into_owned()
}

fn parse_tsconfig(text: &str) -> Option<Value> {
    let v: Value = serde_json::from_str(&strip_jsonc(text)).ok()?;
    match &v {
        Value::Null | Value::Bool(false) => None,
        Value::String(s) if s.is_empty() => None,
        Value::Number(n) if n.as_f64() == Some(0.0) => None,
        _ => Some(v),
    }
}

struct Tsconfig {
    path: String,
    options: serde_json::Map<String, Value>,
}

fn nearest_tsconfig<'a>(file_dir: &str, ts: &'a HashMap<String, Tsconfig>) -> Option<&'a Tsconfig> {
    let mut dir = file_dir.to_string();
    loop {
        if let Some(c) = ts.get(&dir) {
            return Some(c);
        }
        if dir.is_empty() || dir == "." {
            return None;
        }
        let up = dirname(&dir);
        if up == dir {
            return None;
        }
        dir = if up == "." { String::new() } else { up };
    }
}

fn effective_compiler_options(
    dir: &str,
    parsed: &Value,
    raws: &HashMap<String, (Value, String)>,
) -> serde_json::Map<String, Value> {
    let mut opts = match parsed.get("compilerOptions") {
        Some(Value::Object(o)) => o.clone(),
        _ => Default::default(),
    };
    if let Some(Value::String(ext)) = parsed.get("extends") {
        if ext.starts_with('.') {
            let ext_path = norm_join(dir, &if ext.ends_with(".json") { ext.clone() } else { format!("{ext}.json") });
            // Only a direct hit on an already-parsed tsconfig counts as "trivial".
            if let Some((raw, path)) = raws.get(&dirname(&ext_path)) {
                if basename(&ext_path) == basename(path) {
                    let mut merged = match raw.get("compilerOptions") {
                        Some(Value::Object(o)) => o.clone(),
                        _ => Default::default(),
                    };
                    for (k, v) in opts {
                        merged.insert(k, v);
                    }
                    opts = merged;
                }
            }
        }
    }
    opts
}

fn resolve_ts_paths(spec: &str, file_dir: &str, ts: &HashMap<String, Tsconfig>, index: &HashSet<String>) -> Option<String> {
    let cfg = nearest_tsconfig(file_dir, ts)?;
    let base_url = cfg.options.get("baseUrl").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    let base_dir = match base_url {
        Some(b) => norm_join(&dirname(&cfg.path), b),
        None => dirname(&cfg.path),
    };
    if let Some(Value::Object(paths)) = cfg.options.get("paths") {
        for (pattern, targets) in paths {
            let captured = match pattern.find('*') {
                None => {
                    if spec != pattern {
                        continue;
                    }
                    String::new()
                }
                Some(star) => {
                    let (prefix, suffix) = (&pattern[..star], &pattern[star + 1..]);
                    if !spec.starts_with(prefix) || !spec.ends_with(suffix) || spec.len() < prefix.len() + suffix.len() {
                        continue;
                    }
                    spec[prefix.len()..spec.len() - suffix.len()].to_string()
                }
            };
            for target in targets.as_array().into_iter().flatten().filter_map(|t| t.as_str()) {
                let candidate = target.replacen('*', &captured, 1);
                if let Some(f) = resolve_js_path(&norm_join(&base_dir, &candidate), index) {
                    return Some(f);
                }
            }
        }
    }
    // baseUrl alone also lets a NON-relative specifier resolve against it.
    if base_url.is_some() && !is_relative_specifier(spec) {
        return resolve_js_path(&norm_join(&base_dir, spec), index);
    }
    None
}

// ── workspace config ────────────────────────────────────────────────────

const CONFIG_BASENAMES: [&str; 6] = ["tsconfig.json", "go.mod", "Cargo.toml", "pyproject.toml", "setup.py", "composer.json"];

fn go_module_name(text: &str) -> Option<String> {
    re!(r"(?m)^\s*module\s+(\S+)").captures(text).map(|m| m[1].to_string())
}
fn has_project_table(text: &str) -> bool {
    re!(r"(?m)^\s*\[(project|tool\.poetry|build-system)\]").is_match(text)
}

struct GoModule {
    dir: String,
    name: String,
}
struct PhpRoot {
    prefix: String,
    dir: String,
}

fn psr4_roots_from(dir: &str, body: &str) -> Vec<PhpRoot> {
    let mut out = Vec::new();
    let Ok(json) = serde_json::from_str::<Value>(body) else { return out };
    for key in ["autoload", "autoload-dev"] {
        let Some(Value::Object(table)) = json.get(key).and_then(|v| v.get("psr-4")) else { continue };
        for (prefix, target) in table {
            let norm_prefix = if prefix.ends_with('\\') { prefix.clone() } else { format!("{prefix}\\") };
            let targets: Vec<&Value> = match target {
                Value::Array(a) => a.iter().collect(),
                v => vec![v],
            };
            for t in targets {
                let Some(t) = t.as_str() else { continue };
                let td = norm_join(dir, t);
                out.push(PhpRoot { prefix: norm_prefix.clone(), dir: td.strip_suffix('/').unwrap_or(&td).to_string() });
            }
        }
    }
    out
}

fn push_unique(v: &mut Vec<String>, s: String) {
    if !v.contains(&s) {
        v.push(s);
    }
}

struct Configs {
    tsconfigs: HashMap<String, Tsconfig>,
    go_modules: Vec<GoModule>,
    py_roots: Vec<String>,
    php_roots: Vec<PhpRoot>,
    java: JavaIndex,
    csharp: CsharpIndex,
    c_roots: CRoots,
}

#[allow(clippy::type_complexity)]
fn read_configs(files: &[FileRow], access: &dyn Access) -> (HashMap<String, Tsconfig>, Vec<GoModule>, Vec<String>, Vec<PhpRoot>) {
    let wanted: Vec<&FileRow> = files.iter().filter(|f| CONFIG_BASENAMES.contains(&basename(&f.rel))).collect();
    let mut text: Vec<(String, String)> = Vec::new();
    if !wanted.is_empty() {
        let got = access.read(&wanted.iter().map(|f| f.rel.clone()).collect::<Vec<_>>());
        for f in &wanted {
            if let Some(b) = got.get(&f.rel) {
                text.push((f.rel.clone(), String::from_utf8_lossy(b).into_owned()));
            }
        }
    }
    let mut raws: HashMap<String, (Value, String)> = HashMap::new();
    let mut go_modules = Vec::new();
    let mut py_roots: Vec<String> = Vec::new();
    let mut php_roots = Vec::new();
    for (rel, body) in &text {
        let dir = dir_or_root(rel);
        let base = basename(rel);
        if base == "tsconfig.json" {
            if let Some(raw) = parse_tsconfig(body) {
                raws.insert(dir, (raw, rel.clone()));
            }
        } else if base == "go.mod" {
            if let Some(name) = go_module_name(body) {
                go_modules.push(GoModule { dir, name });
            }
        } else if (base == "pyproject.toml" && has_project_table(body)) || base == "setup.py" {
            push_unique(&mut py_roots, dir.clone());
            let src = if dir.is_empty() { "src".to_string() } else { format!("{dir}/src") };
            if files.iter().any(|f| f.rel.starts_with(&format!("{src}/"))) {
                push_unique(&mut py_roots, src);
            }
        } else if base == "composer.json" {
            php_roots.extend(psr4_roots_from(&dir, body));
        }
    }
    let mut tsconfigs = HashMap::new();
    for (dir, (raw, path)) in &raws {
        let options = effective_compiler_options(dir, raw, &raws);
        tsconfigs.insert(dir.clone(), Tsconfig { path: path.clone(), options });
    }
    // Python roots: parent of every top-level `__init__.py` dir, plus the project dirs, plus the root.
    let mut init_dirs: Vec<String> = Vec::new();
    for f in files {
        if basename(&f.rel) == "__init__.py" {
            push_unique(&mut init_dirs, dirname(&f.rel));
        }
    }
    for dir in &init_dirs {
        let mut d = dir.clone();
        while !d.is_empty() && init_dirs.contains(&dir_or_root(&d)) {
            d = dir_or_root(&d);
        }
        push_unique(&mut py_roots, dir_or_root(&d));
    }
    push_unique(&mut py_roots, String::new());
    php_roots.sort_by(|a, b| b.prefix.len().cmp(&a.prefix.len()));
    (tsconfigs, go_modules, py_roots, php_roots)
}

// ── Python ──────────────────────────────────────────────────────────────

fn py_dotted_to_path(root: &str, parts: &[String]) -> String {
    if root.is_empty() { parts.join("/") } else { format!("{root}/{}", parts.join("/")) }
}

fn resolve_py_module(root: &str, parts: &[String], index: &HashSet<String>) -> Option<String> {
    if parts.is_empty() {
        let c = if root.is_empty() { "__init__.py".to_string() } else { format!("{root}/__init__.py") };
        return index.contains(&c).then_some(c);
    }
    let base = py_dotted_to_path(root, parts);
    for c in [format!("{base}.py"), format!("{base}/__init__.py")] {
        if index.contains(&c) {
            return Some(c);
        }
    }
    None
}

fn split_dots(s: &str) -> Vec<String> {
    s.split('.').filter(|p| !p.is_empty()).map(String::from).collect()
}

fn resolve_py_from_import(
    from_rel: &str,
    from_text: &str,
    names: &Option<Vec<Name>>,
    py_roots: &[String],
    index: &HashSet<String>,
) -> Vec<(String, Option<String>)> {
    let level = from_text.chars().take_while(|c| *c == '.').count();
    let mut module_file = None;
    let module_dir: Option<String>;
    if level > 0 {
        let parts = split_dots(&from_text[level..]);
        let mut dir = dirname(from_rel);
        for _ in 1..level {
            dir = dir_or_root(&dir);
        }
        if dir == "." {
            dir = String::new();
        }
        module_file = resolve_py_module(&dir, &parts, index);
        module_dir = Some(if parts.is_empty() { dir } else { py_dotted_to_path(&dir, &parts) });
    } else {
        let parts = split_dots(from_text);
        let mut md = None;
        for root in py_roots {
            if let Some(f) = resolve_py_module(root, &parts, index) {
                module_file = Some(f);
                md = Some(py_dotted_to_path(root, &parts));
                break;
            }
        }
        module_dir = md;
    }
    let mut out = Vec::new();
    let Some(names) = names else {
        if let Some(f) = module_file {
            out.push((f, None));
        }
        return out;
    };
    for n in names {
        if let Some(f) = &module_file {
            out.push((f.clone(), n.imported.clone()));
            continue;
        }
        if let Some(md) = &module_dir {
            let part = n.imported.clone().unwrap_or_default();
            if let Some(sub) = resolve_py_module(md, &[part], index) {
                out.push((sub, None));
            }
        }
    }
    out
}

// ── Go ──────────────────────────────────────────────────────────────────

fn resolve_go_import(spec: &str, mods: &[GoModule], files: &[FileRow]) -> Vec<String> {
    let mut best: Option<&GoModule> = None;
    for m in mods {
        if (spec == m.name || spec.starts_with(&format!("{}/", m.name))) && best.map_or(true, |b| m.name.len() > b.name.len()) {
            best = Some(m);
        }
    }
    let Some(best) = best else { return vec![] };
    let rest = spec[best.name.len()..].strip_prefix('/').unwrap_or(&spec[best.name.len()..]);
    let dir = if best.dir.is_empty() {
        rest.to_string()
    } else if rest.is_empty() {
        best.dir.clone()
    } else {
        format!("{}/{rest}", best.dir)
    };
    let want = if dir.is_empty() { ".".to_string() } else { dir };
    let mut seen = HashSet::new();
    files
        .iter()
        .map(|f| &f.rel)
        .filter(|p| seen.insert(*p))
        .filter(|p| dirname(p) == want && p.ends_with(".go") && !p.ends_with("_test.go"))
        .cloned()
        .collect()
}

// ── Rust ────────────────────────────────────────────────────────────────

struct RustCrate {
    root: String,
}
struct RustTree {
    file_to_path: HashMap<String, Vec<String>>,
    path_to_file: HashMap<String, String>,
}

fn find_rust_crates(files: &[FileRow], index: &HashSet<String>) -> Vec<RustCrate> {
    let mut crates = Vec::new();
    for root in ["src/main.rs", "src/lib.rs"] {
        if index.contains(root) {
            crates.push(RustCrate { root: root.into() });
        }
    }
    for f in files {
        if (f.rel.ends_with("/src/main.rs") || f.rel.ends_with("/src/lib.rs")) && f.rel != "src/main.rs" && f.rel != "src/lib.rs" {
            crates.push(RustCrate { root: f.rel.clone() });
        }
    }
    crates
}

fn build_rust_module_tree(crate_: &RustCrate, by_rel: &HashMap<&str, &FileRow>, index: &HashSet<String>) -> RustTree {
    let mut file_to_path: HashMap<String, Vec<String>> = HashMap::from([(crate_.root.clone(), vec![])]);
    let mut path_to_file: HashMap<String, String> = HashMap::from([(String::new(), crate_.root.clone())]);
    let mut queue = std::collections::VecDeque::from([crate_.root.clone()]);
    while let Some(file) = queue.pop_front() {
        let segs = file_to_path[&file].clone();
        let row = by_rel.get(file.as_str());
        let base = basename(&file).trim_end_matches(".rs").to_string();
        // A non-root module file's children live under a directory named after it, unless it is mod.rs/root.
        let child_dir = if file == crate_.root || base == "mod" { dirname(&file) } else { format!("{}/{base}", dirname(&file)) };
        for c in row.iter().flat_map(|r| r.imports.iter()).filter(|i| i.kind.as_deref() == Some("modpath")) {
            for name in c.names.iter().flatten().filter_map(|n| n.local.as_ref()).filter(|n| !n.is_empty()) {
                let a = format!("{child_dir}/{name}.rs");
                let b = format!("{child_dir}/{name}/mod.rs");
                let child = if index.contains(&a) { a } else if index.contains(&b) { b } else { continue };
                if file_to_path.contains_key(&child) {
                    continue;
                }
                let mut cs = segs.clone();
                cs.push(name.clone());
                path_to_file.insert(cs.join("::"), child.clone());
                file_to_path.insert(child.clone(), cs);
                queue.push_back(child);
            }
        }
    }
    RustTree { file_to_path, path_to_file }
}

fn resolve_rust_path(from_file: &str, text: &str, tree: &RustTree) -> Option<(String, Option<String>)> {
    let segs: Vec<&str> = text.split("::").filter(|s| !s.is_empty()).collect();
    let head = *segs.first()?;
    let own = tree.file_to_path.get(from_file).cloned().unwrap_or_default();
    let (base, rest): (Vec<String>, &[&str]) = match head {
        "crate" => (vec![], &segs[1..]),
        "self" => (own, &segs[1..]),
        "super" => (own[..own.len().saturating_sub(1)].to_vec(), &segs[1..]),
        _ => return None, // external crate / std
    };
    for n in [rest.len() as i64, rest.len() as i64 - 1] {
        if n < 0 {
            continue;
        }
        let n = n as usize;
        let key = base.iter().map(|s| s.as_str()).chain(rest[..n].iter().copied()).collect::<Vec<_>>().join("::");
        if let Some(file) = tree.path_to_file.get(&key) {
            return Some((file.clone(), if n < rest.len() { Some(rest[rest.len() - 1].to_string()) } else { None }));
        }
    }
    None
}

// ── PHP ─────────────────────────────────────────────────────────────────

fn resolve_php_fqn(fqn: &str, roots: &[PhpRoot], index: &HashSet<String>) -> Option<String> {
    let mut best: Option<&PhpRoot> = None;
    for r in roots {
        if (fqn == &r.prefix[..r.prefix.len() - 1] || fqn.starts_with(&r.prefix)) && best.map_or(true, |b| r.prefix.len() > b.prefix.len()) {
            best = Some(r);
        }
    }
    let best = best?;
    let rest = &fqn[best.prefix.len().min(fqn.len())..];
    if rest.is_empty() {
        return None;
    }
    let rel = rest.split('\\').filter(|s| !s.is_empty()).collect::<Vec<_>>().join("/");
    let cand = if best.dir.is_empty() { format!("{rel}.php") } else { format!("{}/{rel}.php", best.dir) };
    index.contains(&cand).then_some(cand)
}

fn resolve_php_require(from_file: &str, spec: &str, index: &HashSet<String>) -> Option<String> {
    let norm = norm_join(&dirname(from_file), spec);
    if index.contains(&norm) {
        return Some(norm);
    }
    let with = format!("{norm}.php");
    (!norm.ends_with(".php") && index.contains(&with)).then_some(with)
}

// ── Java / C# ───────────────────────────────────────────────────────────

#[derive(Default)]
struct JavaIndex {
    by_fqcn: HashMap<String, String>,
    by_package: HashMap<String, Vec<String>>,
}

fn is_type_kind(k: &str) -> bool {
    matches!(k, "class" | "interface" | "enum")
}
fn no_parent(s: &Symbol) -> bool {
    s.parent.as_deref().map_or(true, str::is_empty)
}

fn build_java_index(files: &[FileRow]) -> JavaIndex {
    let mut ix = JavaIndex::default();
    for f in files.iter().filter(|f| f.lang.as_deref() == Some("java")) {
        let pkg = f.symbols.iter().find(|s| s.kind == "module" && no_parent(s));
        let pkg_name = pkg.map(|p| p.name.clone()).unwrap_or_default();
        if !pkg_name.is_empty() {
            ix.by_package.entry(pkg_name.clone()).or_default().push(f.rel.clone());
        }
        for s in &f.symbols {
            if !no_parent(s) || !is_type_kind(&s.kind) {
                continue;
            }
            let key = if pkg_name.is_empty() { s.name.clone() } else { format!("{pkg_name}.{}", s.name) };
            ix.by_fqcn.insert(key, f.rel.clone());
        }
    }
    ix
}

fn resolve_java_import(imp: &Import, from: &str, ix: &JavaIndex) -> Vec<(String, Option<String>)> {
    match imp.kind.as_deref() {
        Some("importstar") => ix.by_package.get(from).into_iter().flatten().map(|t| (t.clone(), None)).collect(),
        Some("staticimport") => {
            let parts: Vec<&str> = from.split('.').collect();
            for i in (1..parts.len()).rev() {
                if let Some(f) = ix.by_fqcn.get(&parts[..i].join(".")) {
                    return vec![(f.clone(), Some(parts[i..].join(".")))];
                }
            }
            vec![]
        }
        _ => ix.by_fqcn.get(from).map(|f| vec![(f.clone(), None)]).unwrap_or_default(),
    }
}

#[derive(Default)]
struct CsharpIndex {
    by_fqcn: HashMap<String, String>,
    by_namespace: HashMap<String, Vec<String>>,
}

fn build_csharp_index(files: &[FileRow]) -> CsharpIndex {
    let mut ix = CsharpIndex::default();
    for f in files.iter().filter(|f| f.lang.as_deref() == Some("csharp")) {
        for s in &f.symbols {
            if s.kind == "module" {
                ix.by_namespace.entry(s.name.clone()).or_default().push(f.rel.clone());
            } else if is_type_kind(&s.kind) {
                let key = match s.parent.as_deref() {
                    Some(p) if !p.is_empty() => format!("{p}.{}", s.name),
                    _ => s.name.clone(),
                };
                ix.by_fqcn.insert(key, f.rel.clone());
            }
        }
    }
    ix
}

fn resolve_csharp_import(imp: &Import, from: &str, ix: &CsharpIndex) -> Vec<(String, Option<String>)> {
    if imp.kind.as_deref() == Some("alias") {
        let alias = imp.names.as_ref().and_then(|n| n.first()).and_then(|n| n.local.clone());
        return ix.by_fqcn.get(from).map(|f| vec![(f.clone(), alias)]).unwrap_or_default();
    }
    ix.by_namespace.get(from).into_iter().flatten().map(|t| (t.clone(), None)).collect()
}

// ── C / C++ ─────────────────────────────────────────────────────────────

#[derive(Default)]
struct CRoots {
    proven_roots: HashSet<String>,
    include_dirs: Vec<String>,
}

fn compute_c_include_roots(files: &[FileRow], index: &HashSet<String>) -> CRoots {
    let mut proven = HashSet::new();
    for f in files.iter().filter(|f| matches!(f.lang.as_deref(), Some("c") | Some("cpp"))) {
        let dir = dir_or_root(&f.rel);
        for imp in &f.imports {
            if let (Some("include"), Some(from)) = (imp.kind.as_deref(), imp.from.as_deref()) {
                if index.contains(&norm_join(&dir, from)) {
                    proven.insert(dir.clone());
                }
            }
        }
    }
    let mut inc: Vec<String> = Vec::new();
    for f in files {
        let d = dirname(&f.rel);
        if basename(&d) == "include" {
            push_unique(&mut inc, d);
        }
    }
    inc.sort();
    CRoots { proven_roots: proven, include_dirs: inc }
}

fn resolve_c_include(from_file: &str, spec: &str, c: &CRoots, index: &HashSet<String>) -> Option<String> {
    let own = norm_join(&dir_or_root(from_file), spec);
    if index.contains(&own) {
        return Some(own);
    }
    if index.contains(spec) {
        return Some(spec.to_string());
    }
    let mut roots: Vec<&String> = c.proven_roots.iter().collect();
    roots.sort();
    roots.into_iter().map(|r| norm_join(r, spec)).find(|c| index.contains(c))
}

fn resolve_c_sys_include(spec: &str, c: &CRoots, index: &HashSet<String>) -> Option<String> {
    let bare = spec.strip_prefix('<').unwrap_or(spec);
    let bare = bare.strip_suffix('>').unwrap_or(bare);
    c.include_dirs.iter().map(|d| norm_join(d, bare)).find(|c| index.contains(c))
}

// ── per-file resolution ─────────────────────────────────────────────────

fn resolve_file(
    f: &FileRow,
    index: &HashSet<String>,
    cfg: &Configs,
    files: &[FileRow],
    tags: &HashMap<String, String>,
    rust_trees: &HashMap<String, Rc<RustTree>>,
) -> Vec<Edge> {
    let mut out: Vec<Edge> = Vec::new();
    let lang = f.lang.as_deref();
    for imp in &f.imports {
        let Some(from) = imp.from.as_deref() else { continue }; // Rust `mod foo;` is resolved structurally below
        let mut push = |to: Option<String>, name: Option<String>| {
            if let Some(to) = to.filter(|t| !t.is_empty()) {
                out.push(Edge {
                    from: imp.from.clone(),
                    to,
                    name,
                    line: imp.line,
                    kind: imp.kind.clone(),
                    reexport: imp.reexport,
                    names: imp.names.clone(),
                });
            }
        };
        // okjs component tag: `from` is a tag, resolved against component symbols, before language dispatch.
        if matches!(imp.kind.as_deref(), Some("component") | Some("component-tag")) {
            if let Some(owner) = tags.get(from) {
                push(Some(owner.clone()), Some(from.to_string()));
            }
            continue;
        }
        let fdir = dirname(&f.rel);
        match lang {
            Some("javascript") | Some("typescript") => {
                let names: Vec<Option<String>> = match &imp.names {
                    Some(n) if !n.is_empty() => n.iter().map(|n| n.imported.clone()).collect(),
                    _ => vec![None],
                };
                let found = if is_relative_specifier(from) {
                    resolve_js_path(&norm_join(&fdir, from), index)
                } else {
                    resolve_ts_paths(from, &fdir, &cfg.tsconfigs, index)
                };
                if let Some(found) = found {
                    for n in names {
                        push(Some(found.clone()), n);
                    }
                }
            }
            Some("python") => {
                if imp.kind.as_deref() == Some("modimport") {
                    let parts = split_dots(from);
                    if let Some(found) = cfg.py_roots.iter().find_map(|r| resolve_py_module(r, &parts, index)) {
                        push(Some(found), None);
                    }
                } else {
                    for (to, name) in resolve_py_from_import(&f.rel, from, &imp.names, &cfg.py_roots, index) {
                        push(Some(to), name);
                    }
                }
            }
            Some("go") => {
                for to in resolve_go_import(from, &cfg.go_modules, files) {
                    push(Some(to), None);
                }
            }
            Some("rust") => {
                if let Some(tree) = rust_trees.get(&f.rel) {
                    if let Some((to, name)) = resolve_rust_path(&f.rel, from, tree) {
                        push(Some(to), name);
                    }
                }
            }
            Some("php") => {
                if imp.kind.as_deref() == Some("require") {
                    push(resolve_php_require(&f.rel, from, index), None);
                } else {
                    for n in imp.names.iter().flatten() {
                        let imported = n.imported.clone().filter(|s| !s.is_empty());
                        let fqn = match &imported {
                            Some(i) => format!("{}{i}", if from.is_empty() { String::new() } else { format!("{from}\\") }),
                            None => from.to_string(),
                        };
                        push(resolve_php_fqn(&fqn, &cfg.php_roots, index), n.imported.clone());
                    }
                }
            }
            Some("java") => {
                for (to, name) in resolve_java_import(imp, from, &cfg.java) {
                    push(Some(to), name);
                }
            }
            Some("csharp") => {
                for (to, name) in resolve_csharp_import(imp, from, &cfg.csharp) {
                    push(Some(to), name);
                }
            }
            Some("c") | Some("cpp") => match imp.kind.as_deref() {
                Some("include") => push(resolve_c_include(&f.rel, from, &cfg.c_roots, index), None),
                Some("sysinclude") => push(resolve_c_sys_include(from, &cfg.c_roots, index), None),
                _ => {}
            },
            _ => {
                if is_relative_specifier(from) {
                    let n = norm_join(&fdir, from);
                    let found = resolve_js_path(&n, index).or_else(|| index.contains(&n).then(|| n.clone()));
                    push(found, None);
                }
            }
        }
    }
    // Rust `mod foo;` declarations resolve structurally via the crate's module tree.
    if let Some(tree) = rust_trees.get(&f.rel) {
        for imp in f.imports.iter().filter(|i| i.kind.as_deref() == Some("modpath")) {
            for name in imp.names.iter().flatten().filter_map(|n| n.local.clone()) {
                let mut segs = tree.file_to_path.get(&f.rel).cloned().unwrap_or_default();
                segs.push(name);
                if let Some(file) = tree.path_to_file.get(&segs.join("::")) {
                    out.push(Edge { from: None, to: file.clone(), name: None, line: imp.line, kind: Some("modpath".into()), reexport: false, names: None });
                }
            }
        }
    }
    out
}

// ── the index ───────────────────────────────────────────────────────────

pub struct EdgeIndex<A: Access> {
    access: A,
    gen: i64,
    forward: HashMap<String, Vec<Edge>>,
    reverse: HashMap<String, Vec<Importer>>,
    built: bool,
}

impl<A: Access> EdgeIndex<A> {
    pub fn new(access: A) -> Self {
        EdgeIndex { access, gen: -1, forward: HashMap::new(), reverse: HashMap::new(), built: false }
    }

    pub fn ensure(&mut self, files: &[FileRow], current_gen: i64) {
        if self.built && self.gen == current_gen {
            return;
        }
        let index: HashSet<String> = files.iter().map(|f| f.rel.clone()).collect();
        let by_rel: HashMap<&str, &FileRow> = files.iter().map(|f| (f.rel.as_str(), f)).collect();
        let (tsconfigs, go_modules, py_roots, php_roots) = read_configs(files, &self.access);
        let mut tags = HashMap::new();
        for f in files {
            for s in &f.symbols {
                if s.kind == "component" && no_parent(s) {
                    tags.insert(s.name.clone(), f.rel.clone());
                }
            }
        }
        let mut rust_trees: HashMap<String, Rc<RustTree>> = HashMap::new();
        for c in find_rust_crates(files, &index) {
            let tree = Rc::new(build_rust_module_tree(&c, &by_rel, &index));
            for file in tree.file_to_path.keys() {
                rust_trees.insert(file.clone(), tree.clone());
            }
        }
        let cfg = Configs {
            tsconfigs,
            go_modules,
            py_roots,
            php_roots,
            java: build_java_index(files),
            csharp: build_csharp_index(files),
            c_roots: compute_c_include_roots(files, &index),
        };
        let mut forward: HashMap<String, Vec<Edge>> = HashMap::new();
        let mut order: Vec<String> = Vec::new();
        for f in files {
            if !forward.contains_key(&f.rel) {
                order.push(f.rel.clone());
            }
            forward.insert(f.rel.clone(), resolve_file(f, &index, &cfg, files, &tags, &rust_trees));
        }
        let mut reverse: HashMap<String, Vec<Importer>> = HashMap::new();
        // One row per (target, importing file, line, hop): a multi-name import pushes several forward edges.
        fn add(reverse: &mut HashMap<String, Vec<Importer>>, target: &str, e: Importer) {
            let arr = reverse.entry(target.to_string()).or_default();
            if arr.iter().any(|x| x.file == e.file && x.line == e.line && x.via == e.via) {
                return;
            }
            arr.push(e);
        }
        for rel in &order {
            for e in &forward[rel] {
                add(&mut reverse, &e.to, Importer { file: rel.clone(), line: e.line, kind: e.kind.clone(), names: e.names.clone(), via: None });
            }
        }
        // A re-export edge A -> B: every importer of A (for that name) is also an importer of B, one hop.
        for rel in &order {
            for e in &forward[rel] {
                if !e.reexport {
                    continue;
                }
                let direct = reverse.get(rel).cloned().unwrap_or_default();
                for imp in direct {
                    if let (Some(en), Some(names)) = (&e.name, &imp.names) {
                        if !en.is_empty() && !names.iter().any(|n| n.imported.as_deref() == Some(en)) {
                            continue;
                        }
                    }
                    add(&mut reverse, &e.to, Importer { file: imp.file, line: imp.line, kind: imp.kind, names: imp.names, via: Some(rel.clone()) });
                }
            }
        }
        self.forward = forward;
        self.reverse = reverse;
        self.gen = current_gen;
        self.built = true;
    }

    pub fn imports_of(&self, rel: &str) -> &[Edge] {
        self.forward.get(rel).map(Vec::as_slice).unwrap_or(&[])
    }
    pub fn importers_of(&self, rel: &str) -> &[Importer] {
        self.reverse.get(rel).map(Vec::as_slice).unwrap_or(&[])
    }
    pub fn invalidate(&mut self) {
        self.gen = -1;
    }
}
