//! The dependency SURFACE: what the declared dependencies of a project expose (src/analysis/packages.js).
use regex::Regex;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

/// Read workspace-relative paths; missing files are absent from the map.
pub trait Access {
    fn read(&self, paths: &[String]) -> HashMap<String, Vec<u8>>;
}

macro_rules! re {
    ($p:expr) => {{
        static R: OnceLock<Regex> = OnceLock::new();
        R.get_or_init(|| Regex::new($p).unwrap())
    }};
}

const IDENT: &str = r"[A-Za-z_$][A-Za-z0-9_$]*";

pub fn names_from_dts(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |s: &str| {
        if !out.iter().any(|x| x == s) {
            out.push(s.to_string());
        }
    };
    let export = re!(&format!(
        r"(?m)^\s*export\s+(?:declare\s+)?(?:async\s+)?(?:function|class|const|let|var|type|interface|enum|namespace)\s+({IDENT})"
    ));
    for m in export.captures_iter(text) {
        add(&m[1]);
    }
    let list = re!(r"(?m)^\s*export\s*\{([^}]*)\}");
    let as_sep = re!(r"\s+as\s+");
    let ident = re!(&format!("^{IDENT}$"));
    for m in list.captures_iter(text) {
        for part in m[1].split(',') {
            let last = as_sep.split(part.trim()).last().unwrap_or("").trim();
            if !last.is_empty() && ident.is_match(last) {
                add(last);
            }
        }
    }
    if re!(r"(?m)^\s*export\s+default(?-u:\b)").is_match(text) {
        add("default");
    }
    out
}

/// The members of a declared class or interface, as `Type.member`.
pub fn members_from_dts(text: &str) -> Vec<String> {
    let decl = re!(&format!(
        r"^\s*(?:export\s+)?(?:declare\s+)?(?:abstract\s+)?(?:class|interface)\s+({IDENT})"
    ));
    let member = re!(&format!(
        r"^\s+(?:(?:readonly|static|abstract|async|public)\s+)*({IDENT})\s*[(<?:]"
    ));
    let count = |line: &str, ch: char| line.matches(ch).count() as i64;
    let mut out = Vec::new();
    let mut owner: Option<String> = None;
    let mut depth: i64 = 0;
    let mut open: i64 = 0;
    for line in text.split('\n') {
        if owner.is_none() {
            if let Some(m) = decl.captures(line) {
                owner = Some(m[1].to_string());
                depth = 0;
                open = 0;
            }
        }
        let Some(o) = owner.clone() else { continue };
        depth += count(line, '{') - count(line, '}');
        // A wrapped signature's parameter looks like a member; only a line outside an open paren list is one.
        if let Some(m) = member.captures(line) {
            if depth > 0 && open == 0 {
                out.push(format!("{o}.{}", &m[1]));
            }
        }
        open += count(line, '(') - count(line, ')');
        if open < 0 {
            open = 0;
        }
        if depth <= 0 && line.contains('}') {
            owner = None;
        }
    }
    out
}

/// Relative `from '...'` specifiers (single-quoted first, then double-quoted).
pub fn re_exports(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for r in [re!(r"(?-u:\b)from\s+'(\.[^']+)'"), re!(r#"(?-u:\b)from\s+"(\.[^"]+)""#)] {
        for m in r.captures_iter(text) {
            if !out.iter().any(|x| x == &m[1]) {
                out.push(m[1].to_string());
            }
        }
    }
    out
}

/// Declared dependencies, in order, mapped to the list each came from.
pub fn declared(manifest: &Value) -> Vec<(String, &'static str)> {
    let mut out: Vec<(String, &'static str)> = Vec::new();
    for (field, source) in [
        ("dependencies", "dependency"),
        ("devDependencies", "dev"),
        ("optionalDependencies", "optional"),
        ("peerDependencies", "peer"),
    ] {
        if let Some(Value::Object(m)) = manifest.get(field) {
            for k in m.keys() {
                if !out.iter().any(|(n, _)| n == k) {
                    out.push((k.clone(), source));
                }
            }
        }
    }
    out
}

#[derive(Debug, Clone, Default)]
pub struct Package {
    pub name: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub main: Option<String>,
    pub module: Option<String>,
    pub types: Option<String>,
    pub export_keys: Vec<String>,
    pub bin: Vec<String>,
    pub source: String,
    pub surface: Vec<String>,
    pub surface_from: Option<String>,
    pub surface_file: Option<String>,
    pub members: Vec<String>,
    pub surface_truncated: bool,
    pub members_truncated: bool,
}

fn truthy_str(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn from_manifest(name: &str, m: &Value, source: &str) -> Package {
    let export_keys = match m.get("exports") {
        Some(Value::Object(o)) => o.keys().cloned().collect(),
        Some(Value::Array(a)) => (0..a.len()).map(|i| i.to_string()).collect(),
        _ => vec![],
    };
    let bin = match m.get("bin") {
        Some(Value::String(s)) if !s.is_empty() => vec![name.to_string()],
        Some(Value::Object(o)) => o.keys().cloned().collect(),
        Some(Value::Array(a)) => (0..a.len()).map(|i| i.to_string()).collect(),
        _ => vec![],
    };
    let desc_full = match m.get("description") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(true)) => "true".into(),
        Some(Value::Array(_)) | Some(Value::Object(_)) => "[object]".into(),
        _ => String::new(),
    };
    // JS slices by UTF-16 units; cut at 200 units without splitting a char.
    let mut units = 0;
    let desc: String = desc_full
        .chars()
        .take_while(|c| {
            units += c.len_utf16();
            units <= 200
        })
        .collect();
    Package {
        name: name.to_string(),
        version: truthy_str(m.get("version")),
        description: if desc.is_empty() { None } else { Some(desc) },
        main: truthy_str(m.get("main")),
        module: truthy_str(m.get("module")),
        types: truthy_str(m.get("types")).or_else(|| truthy_str(m.get("typings"))),
        export_keys,
        bin,
        source: source.to_string(),
        ..Default::default()
    }
}

/// Workspace-relative canonical path; None when `..` would climb above the root.
pub fn normalise(path: &str) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    let p = path.replace('\\', "/");
    for seg in p.split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            out.pop()?;
            continue;
        }
        out.push(seg);
    }
    Some(out.join("/"))
}

fn uniq(list: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    list.into_iter().filter(|s| seen.insert(s.clone())).collect()
}

fn surface_candidates(pkg: &Package, dir: &str) -> Vec<(String, &'static str)> {
    let at = |rel: &str| normalise(&format!("{dir}/{rel}"));
    let mut out = Vec::new();
    let mut push = |p: Option<String>, from: &'static str| {
        if let Some(p) = p {
            out.push((p, from));
        }
    };
    if let Some(t) = &pkg.types {
        push(at(t), "types");
    }
    push(at("index.d.ts"), "types");
    if let Some(m) = &pkg.main {
        push(at(m), "main");
    }
    push(at("index.js"), "main");
    out
}

/// `extract_exports(path, text)` stands in for parse.js `extract(path, text).exports`.
// TODO(wave2): use okcode-analysis extract
pub fn read_packages(
    access: &dyn Access,
    names: &[String],
    source_of: &dyn Fn(&str) -> String,
    extract_exports: &dyn Fn(&str, &str) -> Vec<String>,
) -> Vec<Package> {
    let dir_of = |n: &str| format!("node_modules/{n}");
    let manifests = access.read(&names.iter().map(|n| format!("{}/package.json", dir_of(n))).collect::<Vec<_>>());
    let mut packages: Vec<Package> = Vec::new();
    let mut idx: HashMap<String, usize> = HashMap::new();
    let mut wanted: Vec<(String, String, &'static str)> = Vec::new();
    for name in names {
        let Some(buf) = manifests.get(&format!("{}/package.json", dir_of(name))) else { continue };
        let Ok(manifest) = serde_json::from_slice::<Value>(String::from_utf8_lossy(buf).as_bytes()) else { continue };
        let pkg = from_manifest(name, &manifest, &source_of(name));
        for (path, from) in surface_candidates(&pkg, &dir_of(name)) {
            wanted.push((name.clone(), path, from));
        }
        match idx.get(name) {
            Some(&i) => packages[i] = pkg,
            None => {
                idx.insert(name.clone(), packages.len());
                packages.push(pkg);
            }
        }
    }
    let bodies = if wanted.is_empty() {
        HashMap::new()
    } else {
        access.read(&uniq(wanted.iter().map(|w| w.1.clone())))
    };
    let mut follow: Vec<(String, String)> = Vec::new();
    for (name, path, from) in &wanted {
        let Some(&i) = idx.get(name) else { continue };
        if !packages[i].surface.is_empty() {
            continue;
        }
        let Some(buf) = bodies.get(path) else { continue };
        let text = String::from_utf8_lossy(buf).into_owned();
        let found = if *from == "types" { names_from_dts(&text) } else { extract_exports(path, &text) };
        if found.is_empty() {
            continue;
        }
        let pkg = &mut packages[i];
        pkg.surface = found;
        pkg.members = if *from == "types" { members_from_dts(&text) } else { vec![] };
        pkg.surface_from = Some(from.to_string());
        pkg.surface_file = Some(path.get(dir_of(name).len() + 1..).unwrap_or("").to_string());
        if *from != "types" {
            continue;
        }
        // JS slice(0, lastIndexOf('/')): -1 when absent drops the last char.
        let dir: String = match path.rfind('/') {
            Some(i) => path[..i].to_string(),
            None => {
                let mut c = path.chars();
                c.next_back();
                c.as_str().to_string()
            }
        };
        for spec in re_exports(&text) {
            let Some(at) = normalise(&format!("{dir}/{spec}")) else { continue };
            follow.push((name.clone(), if at.ends_with(".d.ts") { at.clone() } else { format!("{at}.d.ts") }));
            follow.push((name.clone(), format!("{at}/index.d.ts")));
        }
    }
    if !follow.is_empty() {
        let deeper = access.read(&uniq(follow.iter().map(|f| f.1.clone())));
        for (name, path) in &follow {
            let (Some(&i), Some(buf)) = (idx.get(name), deeper.get(path)) else { continue };
            let text = String::from_utf8_lossy(buf).into_owned();
            packages[i].surface.extend(names_from_dts(&text));
            packages[i].members.extend(members_from_dts(&text));
        }
    }
    for pkg in &mut packages {
        let all = uniq(std::mem::take(&mut pkg.surface));
        let mall = uniq(std::mem::take(&mut pkg.members));
        pkg.surface_truncated = all.len() > 500;
        pkg.members_truncated = mall.len() > 1500;
        pkg.surface = all.into_iter().take(500).collect();
        pkg.members = mall.into_iter().take(1500).collect();
    }
    packages
}

pub struct Scan {
    pub manifest: Option<Value>,
    pub packages: Vec<Package>,
    pub reason: Option<String>,
}

pub fn scan_packages(access: &dyn Access, extract_exports: &dyn Fn(&str, &str) -> Vec<String>) -> Scan {
    let got = access.read(&["package.json".to_string()]);
    let Some(buf) = got.get("package.json") else {
        return Scan { manifest: None, packages: vec![], reason: Some("no package.json at the workspace root".into()) };
    };
    let manifest: Value = match serde_json::from_slice(String::from_utf8_lossy(buf).as_bytes()) {
        Ok(v) => v,
        Err(e) => {
            return Scan { manifest: None, packages: vec![], reason: Some(format!("package.json does not parse: {e}")) }
        }
    };
    let sources = declared(&manifest);
    let names: Vec<String> = sources.iter().map(|s| s.0.clone()).collect();
    let src: HashMap<&str, &str> = sources.iter().map(|(n, s)| (n.as_str(), *s)).collect();
    let packages = read_packages(
        access,
        &names,
        &|n| src.get(n).copied().unwrap_or("transitive").to_string(),
        extract_exports,
    );
    Scan { manifest: Some(manifest), packages, reason: None }
}
