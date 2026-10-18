//! okcode-okjs — port of src/analysis/ext-okjs.js (+ extensions.js registry).
//!
//! okjs's own static analyser (`@kedem/okjs/src/tooling`, plain ESM + acorn) runs UNCHANGED inside
//! QuickJS (rquickjs). The JS sources are embedded with include_str! and served by an in-memory
//! module resolver/loader — no bundler, no Node at runtime. The runtime and context are created
//! once per thread and reused.
//!
//! The generic acorn extractor's symbols (nested under the component, F4 item 1) belong to
//! okcode-js; `analyze_with_generic` takes them through a callback. `analyze` alone yields the
//! component symbol, imports, regions, diagnostics, coverage and the raw analysis.
use rquickjs::loader::{Loader, Resolver};
use rquickjs::module::Declared;
use rquickjs::{Context, Ctx, Module, Result as QResult, Runtime};
use serde_json::{json, Map, Value};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

pub const EXTENSION_CONTRACT_VERSION: u32 = 2;

// --- extension registry (extensions.js): one registrant, kept as a plain matcher ---------------
pub fn matches(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    p.ends_with(".ok.js") || p.ends_with(".ok.mjs") || p.ends_with(".ok.html")
}

/// Versions of the embedded analyser (package.json version, SCHEMA_VERSION), as in toolingVersion().
pub const OKJS_PKG_VERSION: &str = "1.3.1";
pub const OKJS_SCHEMA_VERSION: u32 = 1;
pub fn tooling_version() -> String {
    format!("{OKJS_PKG_VERSION}:{OKJS_SCHEMA_VERSION}+ext{EXTENSION_CONTRACT_VERSION}")
}

// --- embedded sources ---------------------------------------------------------------------------
macro_rules! nm {
    ($p:literal) => {
        include_str!(concat!("../../../node_modules/", $p))
    };
}
const MODULES: &[(&str, &str)] = &[
    ("okjs/glue.js", include_str!("js/glue.js")),
    ("okjs/tooling/analyze.js", nm!("@kedem/okjs/src/tooling/analyze.js")),
    ("okjs/tooling/css.js", nm!("@kedem/okjs/src/tooling/css.js")),
    ("okjs/tooling/import-resolution.js", nm!("@kedem/okjs/src/tooling/import-resolution.js")),
    ("okjs/tooling/javascript.js", nm!("@kedem/okjs/src/tooling/javascript.js")),
    ("okjs/tooling/js-analysis.js", nm!("@kedem/okjs/src/tooling/js-analysis.js")),
    ("okjs/tooling/json.js", nm!("@kedem/okjs/src/tooling/json.js")),
    ("okjs/tooling/model.js", nm!("@kedem/okjs/src/tooling/model.js")),
    ("okjs/syntax/ok-html-blocks.js", nm!("@kedem/okjs/src/syntax/ok-html-blocks.js")),
    ("okjs/runtime/ok-import-resolution.js", nm!("@kedem/okjs/src/runtime/ok-import-resolution.js")),
    ("acorn", nm!("acorn/dist/acorn.mjs")),
];
const PRELUDE: &str = include_str!("js/prelude.js");

struct MemResolver;
impl Resolver for MemResolver {
    fn resolve<'js>(&mut self, _ctx: &Ctx<'js>, base: &str, name: &str, _a: Option<rquickjs::loader::ImportAttributes<'js>>) -> QResult<String> {
        if !name.starts_with('.') {
            return Ok(name.to_string());
        }
        let mut parts: Vec<&str> = base.split('/').collect();
        parts.pop();
        for seg in name.split('/') {
            match seg {
                "." | "" => {}
                ".." => {
                    parts.pop();
                }
                s => parts.push(s),
            }
        }
        Ok(parts.join("/"))
    }
}
struct MemLoader(HashMap<&'static str, &'static str>);
impl Loader for MemLoader {
    fn load<'js>(&mut self, ctx: &Ctx<'js>, name: &str, _a: Option<rquickjs::loader::ImportAttributes<'js>>) -> QResult<Module<'js, Declared>> {
        match self.0.get(name) {
            Some(src) => Module::declare(ctx.clone(), name, *src),
            None => Err(rquickjs::Error::new_loading(name)),
        }
    }
}

pub struct Analyzer {
    // `_rt` must outlive `ctx`.
    ctx: Context,
    _rt: Runtime,
}

impl Analyzer {
    pub fn new() -> Result<Self, String> {
        let rt = Runtime::new().map_err(|e| e.to_string())?;
        rt.set_max_stack_size(4 * 1024 * 1024);
        rt.set_loader(MemResolver, MemLoader(MODULES.iter().copied().collect()));
        let ctx = Context::full(&rt).map_err(|e| e.to_string())?;
        ctx.with(|c| -> Result<(), String> {
            let fail = |c: &Ctx, e: rquickjs::Error| format!("{e}: {:?}", c.catch());
            c.eval::<(), _>(PRELUDE).map_err(|e| fail(&c, e))?;
            let p = Module::import(&c, "okjs/glue.js").map_err(|e| fail(&c, e))?;
            p.finish::<()>().map_err(|e| fail(&c, e))?;
            Ok(())
        })?;
        Ok(Analyzer { ctx, _rt: rt })
    }

    /// The analyser's own result (`analyzeOKSource`), or Err(message) if it threw.
    pub fn analyze_raw(&self, path: &str, source: &str) -> Result<Value, String> {
        let base = base_url_for(path);
        let json: String = self
            .ctx
            .with(|c| -> Result<String, String> {
                let f: rquickjs::Function = c.globals().get("__okjs_analyze").map_err(|e| e.to_string())?;
                f.call((path, source, base)).map_err(|e| format!("{e}: {:?}", c.catch()))
            })?;
        let v: Value = serde_json::from_str(&json).map_err(|e| e.to_string())?;
        if let Some(m) = v.get("$error").and_then(Value::as_str) {
            return Err(m.to_string());
        }
        Ok(v)
    }

    /// Component symbol + okjs-only analysis (no generic nested symbols).
    pub fn analyze(&self, path: &str, source: &str) -> Value {
        self.analyze_with_generic(path, source, &|_, _| Vec::new())
    }

    /// `extract_js(path, js_source)` is the generic extractor (okcode-js) returning symbols.
    pub fn analyze_with_generic(&self, path: &str, source: &str, extract_js: &dyn Fn(&str, &str) -> Vec<Value>) -> Value {
        match self.analyze_raw(path, source) {
            Err(m) => failed(&m),
            Ok(analysis) => from_analysis(path, source, analysis, extract_js),
        }
    }
}

/// ext-okjs.js#analyzeWith's failure record (`reason` is `String(err.message).slice(0, 160)`).
pub fn failed(m: &str) -> Value {
    json!({"symbols": [], "imports": [], "exports": [], "parsed": false, "reason": m.chars().take(160).collect::<String>()})
}

/// The normalised envelope for an analysis already in hand (the analyser's own, or a injected one).
pub fn from_analysis(path: &str, source: &str, analysis: Value, extract_js: &dyn Fn(&str, &str) -> Vec<Value>) -> Value {
    let comps = translate_symbols(&analysis);
    let generic = generic_symbols_for(path, source, &analysis, extract_js);
    let mut symbols = comps.clone();
    symbols.extend(nest_component_members(&comps, generic));
    json!({
        "symbols": symbols,
        "imports": translate_imports(&analysis),
        "exports": [],
        "parsed": true,
        "diagnostics": analysis.get("diagnostics").cloned().unwrap_or(Value::Null),
        "coverage": analysis.get("coverage").cloned().unwrap_or(Value::Null),
        "regions": normalize_regions(&analysis),
        "okAnalysis": analysis,
    })
}

thread_local! {
    static ANALYZER: RefCell<Option<Analyzer>> = const { RefCell::new(None) };
}
/// Analyse with the per-thread shared runtime (created on first use).
pub fn analyze(path: &str, source: &str) -> Result<Value, String> {
    ANALYZER.with(|a| {
        let mut a = a.borrow_mut();
        if a.is_none() {
            *a = Some(Analyzer::new()?);
        }
        Ok(a.as_ref().unwrap().analyze(path, source))
    })
}

/// A synthetic identifier, never a filesystem lookup.
pub fn base_url_for(path: &str) -> String {
    format!("file:///{}", path.replace('\\', "/").trim_start_matches('/'))
}

fn s(v: &Value) -> Option<&str> {
    v.as_str()
}
fn line_of(range: &Value, which: &str) -> Value {
    range["loc"][which]["line"].clone()
}

fn translate_symbols(a: &Value) -> Vec<Value> {
    let Some(syms) = a["symbols"].as_array() else { return vec![] };
    syms.iter()
        .filter(|x| x["kind"] == "component" && x["range"].is_object())
        .filter_map(|x| {
            let name = match &x["name"] {
                Value::Null => String::new(),
                Value::String(t) => t.clone(),
                // a RegExp crosses as {"$re": source, "flags": f}; String(/x/) is `/x/`
                Value::Object(o) if o.contains_key("$re") => {
                    format!("/{}/{}", o["$re"].as_str().unwrap_or(""), o.get("flags").and_then(Value::as_str).unwrap_or(""))
                }
                other => other.to_string(),
            };
            if name.is_empty() {
                return None;
            }
            let r = &x["range"];
            Some(json!({
                "name": name, "kind": x["kind"], "path": name,
                "start": r["start"], "end": r["end"],
                "lineStart": line_of(r, "start"), "lineEnd": line_of(r, "end"),
            }))
        })
        .collect()
}

fn truthy(v: &Value) -> bool {
    !(v.is_null() || v == &Value::Bool(false) || v.as_str() == Some("") || v.as_f64() == Some(0.0))
}

fn normalize_regions(a: &Value) -> Vec<Value> {
    let Some(rs) = a["regions"].as_array() else { return vec![] };
    rs.iter()
        .filter(|r| matches!(s(&r["source"]), Some("definition" | "block")))
        .filter_map(|r| {
            let span = [&r["contentRange"], &r["valueRange"], &r["range"]].into_iter().find(|v| truthy(v))?;
            let (st, en) = (span["start"].as_f64()?, span["end"].as_f64()?);
            if en <= st {
                return None;
            }
            let ls = span["loc"]["start"]["line"].as_i64().unwrap_or(0);
            let le = span["loc"]["end"]["line"].as_i64().unwrap_or(0);
            let role = &r["role"];
            let fmt = if truthy(&r["format"]) { &r["format"] } else { &r["language"] };
            let doc: Vec<&str> = [role, fmt].into_iter().filter(|v| truthy(v)).filter_map(|v| s(v)).collect();
            Some(json!({
                "name": if truthy(role) { role.clone() } else { r["kind"].clone() },
                "kind": r["kind"],
                "lineStart": ls, "lineEnd": ls.max(le),
                "doc": if doc.is_empty() { Value::Null } else { Value::String(doc.join(" ")) },
            }))
        })
        .collect()
}

fn translate_imports(a: &Value) -> Vec<Value> {
    let Some(ds) = a["dependencies"].as_array() else { return vec![] };
    ds.iter()
        .map(|d| {
            let at = if truthy(&d["statementRange"]) { &d["statementRange"] } else { &d["range"] };
            let from = if !d["specifier"].is_null() {
                d["specifier"].clone()
            } else if truthy(&d["tag"]) {
                d["tag"].clone()
            } else {
                Value::Null
            };
            let kind = s(&d["kind"]).unwrap_or("");
            json!({
                "local": null, "from": from,
                "line": if at["loc"].is_object() { at["loc"]["start"]["line"].clone() } else { Value::Null },
                "esm": kind.starts_with("esm"), "reexport": kind == "esm-export", "kind": d["kind"],
            })
        })
        .collect()
}

// JS strings index by UTF-16 unit; ranges from the analyser (offsetEncoding utf-16) and acorn are in those units.
fn utf16_slice(src: &str, start: usize, end: usize) -> String {
    let u: Vec<u16> = src.encode_utf16().collect();
    String::from_utf16_lossy(&u[start.min(u.len())..end.min(u.len())])
}

fn generic_symbols_for(path: &str, source: &str, analysis: &Value, extract_js: &dyn Fn(&str, &str) -> Vec<Value>) -> Vec<Value> {
    let mut js = source.to_string();
    let mut offset = 0usize;
    if path.to_ascii_lowercase().ends_with(".ok.html") {
        let region = analysis["regions"]
            .as_array()
            .and_then(|rs| rs.iter().find(|r| r["role"] == "script" && truthy(&r["contentRange"])));
        let Some(region) = region else { return vec![] };
        let (st, en) = (region["contentRange"]["start"].as_u64().unwrap_or(0) as usize, region["contentRange"]["end"].as_u64().unwrap_or(0) as usize);
        offset = st;
        js = utf16_slice(source, st, en);
    }
    let syms = extract_js(path, &js);
    if offset == 0 {
        return syms;
    }
    let base_line = utf16_slice(source, 0, offset).split('\n').count() as i64;
    syms.into_iter()
        .map(|mut x| {
            if let Value::Object(m) = &mut x {
                for k in ["start", "end"] {
                    if let Some(n) = m.get(k).and_then(Value::as_i64) {
                        m.insert(k.into(), json!(n + offset as i64));
                    }
                }
                for k in ["lineStart", "lineEnd"] {
                    if let Some(n) = m.get(k).and_then(Value::as_i64) {
                        m.insert(k.into(), json!(n + base_line - 1));
                    }
                }
            }
            x
        })
        .collect()
}

/// Nests generic symbols under the component whose range contains them (by offset containment).
pub fn nest_component_members(components: &[Value], generic: Vec<Value>) -> Vec<Value> {
    if components.is_empty() || generic.is_empty() {
        return generic;
    }
    let num = |v: &Value, k: &str| v[k].as_i64().unwrap_or(0);
    let mut claimed: HashSet<usize> = HashSet::new();
    let mut nested = Vec::new();
    for tag in components {
        let tname = tag["name"].as_str().unwrap_or("");
        let roots: Vec<String> = generic
            .iter()
            .enumerate()
            .filter(|(i, g)| !truthy(&g["parent"]) && !claimed.contains(i) && num(g, "start") >= num(tag, "start") && num(g, "end") <= num(tag, "end"))
            .map(|(_, g)| g["path"].as_str().unwrap_or("").to_string())
            .collect();
        if roots.is_empty() {
            continue;
        }
        for (i, g) in generic.iter().enumerate() {
            if claimed.contains(&i) {
                continue;
            }
            let p = g["path"].as_str().unwrap_or("");
            if !roots.iter().any(|r| p == r || p.strip_prefix(r.as_str()).is_some_and(|t| t.starts_with('.'))) {
                continue;
            }
            claimed.insert(i);
            let mut m: Map<String, Value> = g.as_object().cloned().unwrap_or_default();
            m.insert("path".into(), json!(format!("{tname}.{p}")));
            m.insert("parent".into(), if truthy(&g["parent"]) { json!(format!("{tname}.{}", g["parent"].as_str().unwrap_or(""))) } else { json!(tname) });
            nested.push(Value::Object(m));
        }
    }
    for (i, g) in generic.into_iter().enumerate() {
        if !claimed.contains(&i) {
            nested.push(g);
        }
    }
    nested
}

/// Golden entry point. Only `extract` (restricted to .ok.* paths) is recorded for this crate.
/// The result is the okjs-owned part of the extract envelope: component symbols only (generic
/// symbols come from okcode-js), imports, exports, parsed, diagnostics, coverage, regions, okAnalysis.
pub fn golden_call(name: &str, args: &[Value]) -> Result<Value, String> {
    match name {
        "extract" => {
            let path = args.first().and_then(Value::as_str).ok_or("path")?;
            let source = args.get(1).and_then(Value::as_str).ok_or("source")?;
            let mut v = analyze(path, source)?;
            v["path"] = json!(path);
            Ok(v)
        }
        other => Err(format!("unknown golden fn {other}")),
    }
}

/// Reduce a recorded `extract` envelope to the part this crate produces: drop parse.js-owned
/// fields and the generic (non-component) symbols. Returns None for cases recorded with the
/// okjs tooling unavailable (analyzerVersion "0:0"), which the Rust port cannot reproduce.
pub fn project_expected(out: &Value) -> Option<Value> {
    if out["analyzerVersion"] == "0:0" {
        return None;
    }
    let mut o = out.as_object()?.clone();
    for k in ["lang", "lines", "indexed", "analyzerVersion", "reason"] {
        o.remove(k);
    }
    if let Some(Value::Array(syms)) = o.get_mut("symbols") {
        syms.retain(|x| x["kind"] == "component");
    }
    Some(Value::Object(o))
}
