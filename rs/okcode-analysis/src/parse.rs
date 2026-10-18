//! parse.js#extract: the one dispatcher every ingest goes through.
//!
//! Order (as in JS): an extension claiming the path (okjs: `.ok.js|.ok.mjs|.ok.html`) outranks the
//! lang-keyed extractor; then tree-sitter languages; then JavaScript (oxc in place of acorn, with the
//! JSX fallback inside okcode-js); everything else is indexed as searchable text with no symbols.
use serde_json::{json, Map, Value};

pub use okcode_js::parse::{ext_of, is_secret, is_textual, lang_of, TEXT_MAX};

fn opaque_reason(path: &str) -> &'static str {
    if is_secret(path) {
        "may hold credentials \u{2014} hashed, never read"
    } else {
        "binary or oversized \u{2014} hashed, not read"
    }
}

fn record(path: &str, lang: &str, lines: usize, parsed: bool, indexed: bool) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("path".into(), json!(path));
    m.insert("lang".into(), json!(lang));
    m.insert("lines".into(), json!(lines));
    m.insert("symbols".into(), json!([]));
    m.insert("imports".into(), json!([]));
    m.insert("exports".into(), json!([]));
    m.insert("parsed".into(), json!(parsed));
    m.insert("indexed".into(), json!(indexed));
    m
}

/// UTF-16 `String(s).slice(0, 160)`.
fn slice160(s: &str) -> String {
    okcode_js::text::slice_units(s, 160)
}

/// The generic (okcode-js) symbols of a JS source, for the okjs fallback/nesting.
pub fn generic_js_symbols(path: &str, src: &str) -> Vec<Value> {
    // JS runs extractJavaScript directly; okcode-js::extract refuses okjs-owned paths, so hand it a
    // plain `.js` name (the path only picks the language there).
    let _ = path;
    match okcode_js::parse::extract("component.js", Some(src)) {
        Ok(v) if v["parsed"] == true => v["symbols"].as_array().cloned().unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// parse.js#extract. Always returns the JS record shape (never fails: errors become `reason`).
pub fn extract(path: &str, src: Option<&str>) -> Value {
    let lang = lang_of(path);
    let Some(src) = src else {
        let mut m = record(path, lang, 0, false, false);
        m.insert("reason".into(), json!(opaque_reason(path)));
        return Value::Object(m);
    };
    let lines = src.split('\n').count();

    // 1. extension (okjs)
    if okcode_okjs::matches(path) {
        let version = okcode_okjs::tooling_version();
        // The analyser nests the generic (okcode-js) symbols under the component itself.
        let analysed = match okcode_okjs_with_generic(path, src) {
            Value::Object(o) => Ok(o),
            _ => Err("okjs analyser unavailable".to_string()),
        };
        let mut m = Map::new();
        m.insert("path".into(), json!(path));
        m.insert("lang".into(), json!(lang));
        m.insert("lines".into(), json!(lines));
        match analysed {
            Ok(mut env) => {
                let ok_analysis = env.remove("okAnalysis");
                let diagnostics = env.remove("diagnostics");
                let coverage = env.remove("coverage");
                let regions = env.remove("regions");
                env.remove("fallback");
                env.remove("path");
                for (k, v) in env {
                    m.insert(k, v);
                }
                m.insert("indexed".into(), json!(true));
                m.insert("analyzerVersion".into(), json!(version));
                if let Some(a) = ok_analysis.filter(|a| !a.is_null()) {
                    m.insert("okAnalysis".into(), a);
                    m.insert("diagnostics".into(), diagnostics.unwrap_or(Value::Null));
                    m.insert("coverage".into(), coverage.unwrap_or(Value::Null));
                    m.insert("regions".into(), regions.unwrap_or(Value::Null));
                }
            }
            Err(e) => {
                for (k, v) in record(path, lang, lines, false, true) {
                    m.insert(k, v);
                }
                m.insert("analyzerVersion".into(), json!(version));
                m.insert("reason".into(), json!(slice160(&e)));
            }
        }
        return Value::Object(m);
    }

    extract_generic(path, Some(src))
}

/// parse.js#extract minus the extension step: tree-sitter languages, then JavaScript (oxc), then the
/// no-extractor record. An okjs-owned path is handled as the plain JS/HTML file it also is (the
/// extension's `fallback`). `None` = the opaque record.
pub fn extract_generic(path: &str, src: Option<&str>) -> Value {
    let lang = lang_of(path);
    let Some(src) = src else {
        let mut m = record(path, lang, 0, false, false);
        m.insert("reason".into(), json!(opaque_reason(path)));
        return Value::Object(m);
    };
    let lines = src.split('\n').count();
    // okcode-js refuses okjs-owned paths; the path only picks the language there
    let renamed;
    let js_path = if okcode_js::parse::okjs_owns(path) {
        renamed = if lang == "javascript" { "component.js".to_string() } else { path.to_string() };
        renamed.as_str()
    } else {
        path
    };

    // 2. tree-sitter languages
    if okcode_treesitter::lang_id_for_path(path).is_some() {
        return match okcode_treesitter::extract(path, Some(src)) {
            Ok(v) => v,
            Err(e) => {
                let mut m = record(path, lang, lines, false, true);
                m.insert("reason".into(), json!(slice160(&e)));
                Value::Object(m)
            }
        };
    }

    // 3. JavaScript (and the generic no-extractor record for every other language)
    match okcode_js::parse::extract(js_path, Some(src)) {
        Ok(mut v) => {
            if js_path != path {
                v["path"] = json!(path);
            }
            // JSX that acorn can't parse: JS extracts it with tree-sitter's TSX query
            // (treesitter.extractJsxFallback) when that parse is clean, e.g. class methods as
            // `Panel.render`. okcode-js only stamps the record; the symbols come from here.
            if v["extractor"] == "treesitter-tsx" {
                if let Ok(fb) = okcode_treesitter::extract_jsx_fallback(src) {
                    if fb["hasErrors"] != Value::Bool(true) {
                        for k in ["symbols", "imports", "exports", "references"] {
                            v[k] = fb[k].clone();
                        }
                    }
                }
            }
            v
        }
        Err(_) => {
            // a lang okcode-js leaves to tree-sitter but tree-sitter does not register (yaml): searchable text
            let mut m = record(path, lang, lines, false, true);
            m.insert("reason".into(), json!(format!("{lang} \u{2014} searchable, no symbol extractor yet")));
            Value::Object(m)
        }
    }
}

pub fn okcode_okjs_with_generic(path: &str, src: &str) -> Value {
    thread_local! {
        static A: std::cell::RefCell<Option<okcode_okjs::Analyzer>> = const { std::cell::RefCell::new(None) };
    }
    A.with(|a| {
        let mut a = a.borrow_mut();
        if a.is_none() {
            *a = okcode_okjs::Analyzer::new().ok();
        }
        match a.as_ref() {
            Some(an) => an.analyze_with_generic(path, src, &generic_js_symbols),
            None => Value::Null,
        }
    })
}

/// parse.js#analyzerVersionFor: what a stored row's `analyzerVersion` must equal to be trusted.
pub fn analyzer_version_for(path: &str) -> Option<String> {
    if okcode_okjs::matches(path) {
        return Some(okcode_okjs::tooling_version());
    }
    if lang_of(path) == "javascript" {
        return okcode_js::parse::extract(path, Some("")).ok().and_then(|v| v["analyzerVersion"].as_str().map(String::from));
    }
    okcode_treesitter::current_version_for(path)
}

/// `golden_call` for the composed dispatcher (`extract` only).
pub fn golden_call(name: &str, args: &[Value]) -> Result<Value, String> {
    match name {
        "extract" => {
            let p = args.first().and_then(Value::as_str).ok_or("extract: path")?;
            Ok(extract(p, args.get(1).and_then(Value::as_str)))
        }
        "langOf" => {
            let p = args.first().and_then(Value::as_str).ok_or("langOf: path")?;
            Ok(json!(lang_of(p)))
        }
        other => Err(format!("unknown golden fn {other}")),
    }
}

/// ext-okjs.js#analyzeWith over an analysis the caller already holds (an injected tooling's).
pub fn okjs_from_analysis(path: &str, src: &str, analysis: Value) -> Value {
    okcode_okjs::from_analysis(path, src, analysis, &generic_js_symbols)
}
