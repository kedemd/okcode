//! validate.js — the syntax gate on edits (validateSource / compareDiagnostics), dispatching to
//! okcode-js (JS family, template balance), okcode-treesitter (tree-sitter languages), okcode-okjs
//! (`.ok.*` diagnostics) and okcode-text (convention colour).
//!
//! The gate is synchronous (there is no async twin: nothing spawns
//! `node --check`; oxc stands in for acorn and the runtime check, see okcode-js).
use serde_json::{json, Map, Value};
use std::collections::HashMap;

/// validate.js#validateSource arguments.
#[derive(Clone, Debug, Default)]
pub struct ValidateArgs {
    pub rel: String,
    pub text: String,
    /// 'module' | 'commonjs' | 'ambiguous' (workspace.resolveModuleKind)
    pub module_kind: String,
    /// paletteFrom() of the workspace's convention sources (empty = no colour check)
    pub palette: Vec<okcode_text::convention_color::Palette>,
}

fn len16(s: &str) -> usize {
    s.encode_utf16().count()
}

/// The path the JS-family validators see: `.ok.js`/`.ok.mjs` are JavaScript for the gate (JS
/// validates them as JS AND as an extension), but okcode-js refuses okjs-owned paths outright.
fn js_path(rel: &str) -> String {
    let lower = rel.to_ascii_lowercase();
    if lower.ends_with(".ok.js") {
        format!("{}.js", &rel[..rel.len() - 6])
    } else if lower.ends_with(".ok.mjs") {
        format!("{}.mjs", &rel[..rel.len() - 7])
    } else {
        rel.to_string()
    }
}

fn clip(s: &str, n: usize) -> String {
    String::from_utf16_lossy(&s.encode_utf16().take(n).collect::<Vec<u16>>())
}

fn push_all(into: &mut Vec<Value>, from: &Value, key: &str) {
    if let Some(a) = from.get(key).and_then(Value::as_array) {
        into.extend(a.iter().cloned());
    }
}

/// validate.js#validateSource → `{ validators, diagnostics, coverage }` in the JS shape.
pub fn validate_source(a: &ValidateArgs) -> Result<Value, String> {
    validate_source_ext(a, None)
}

/// As [`validate_source`], with the outcome of a NON-native extension the JS registry matched
/// (the wrapper ran `ext.analyze` itself, it holds user functions): `{id, skipped: note}` for a
/// fallback result or a throw, `{id, diagnostics: [..]}` otherwise. When present it replaces the
/// embedded okjs analysis and makes the file count as extension-owned; the status mapping stays here.
pub fn validate_source_ext(a: &ValidateArgs, ext_outcome: Option<&Value>) -> Result<Value, String> {
    let rel = a.rel.as_str();
    let text = a.text.as_str();
    let is_js = okcode_js::parse::lang_of(rel) == "javascript";
    let ext = ext_outcome.is_some() || okcode_okjs::matches(rel);
    let ts = okcode_treesitter::lang_id_for_path(rel).is_some();
    if !is_js && !ext && !ts {
        return Ok(json!({
            "validators": [{"validator": "none", "status": "skipped", "covers": []}],
            "diagnostics": [],
            "coverage": "complete",
        }));
    }
    let full = json!([{"start": 0, "end": len16(text)}]);
    let mut validators: Vec<Value> = Vec::new();
    let mut diagnostics: Vec<Value> = Vec::new();

    // THE CONVENTION CHECK: runs bluntly; compareDiagnostics keeps it to what the edit introduces.
    if !a.palette.is_empty() {
        let found = okcode_text::convention_color::color_literals(text, &a.palette);
        validators.push(json!({
            "validator": "convention-color",
            "status": if found.is_empty() { "passed" } else { "failed" },
            "covers": full.clone(),
        }));
        let u16s: Vec<u16> = text.encode_utf16().collect();
        for (_, d) in found {
            let (line, column) = okcode_text::text::line_col_at(&u16s, d.start as i64);
            diagnostics.push(json!({
                "source": "convention-color", "code": "CONVENTION_COLOR_LITERAL", "severity": "warning",
                "message": d.message, "start": d.start, "end": d.end, "line": line, "column": column,
            }));
        }
    }

    if is_js {
        let kind = if a.module_kind.is_empty() { "ambiguous" } else { a.module_kind.as_str() };
        let r = okcode_js::validate::validate_source(&okcode_js::validate::Args {
            rel: &js_path(rel),
            text,
            module_kind: kind,
            has_palette: false,
        })?;
        push_all(&mut validators, &r, "validators");
        push_all(&mut diagnostics, &r, "diagnostics");
    }

    if ts {
        let r = okcode_treesitter::validate_source(rel, text)?;
        push_all(&mut validators, &r, "validators");
        push_all(&mut diagnostics, &r, "diagnostics");
    }

    if ext {
        // An extension's own analysis; a checker that fails must never make a file uneditable.
        let id = ext_outcome.and_then(|o| o.get("id")).and_then(Value::as_str).unwrap_or("okjs");
        let vname = format!("{id}-analyze");
        let analysed: Result<Value, String> = match ext_outcome {
            Some(o) => match o.get("skipped") {
                Some(n) => Err(format!("\u{0}{}", n.as_str().unwrap_or("analyser unavailable"))),
                None => Ok(o.clone()),
            },
            None => okcode_okjs::analyze(rel, text),
        };
        match analysed {
            Err(m) => validators.push(json!({
                "validator": vname, "status": "skipped", "covers": [],
                "note": match m.strip_prefix('\u{0}') {
                    Some(n) => clip(n, 160),
                    None => clip(&format!("okjs tooling unavailable ({m}) \u{2014} okjs-specific analysis skipped"), 160),
                },
            })),
            Ok(res) => {
                let diags = res.get("diagnostics").and_then(Value::as_array).cloned().unwrap_or_default();
                for d in &diags {
                    let range = d.get("range").cloned().unwrap_or(Value::Null);
                    let start = &range["loc"]["start"];
                    diagnostics.push(json!({
                        "source": d.get("source").cloned().unwrap_or(Value::Null),
                        "code": d.get("code").cloned().unwrap_or(Value::Null),
                        "severity": d.get("severity").cloned().unwrap_or(Value::Null),
                        "message": d.get("message").cloned().unwrap_or(Value::Null),
                        "start": range.get("start").cloned().unwrap_or(Value::Null),
                        "end": range.get("end").cloned().unwrap_or(Value::Null),
                        "line": start.get("line").cloned().unwrap_or(Value::Null),
                        "column": start.get("column").cloned().unwrap_or(Value::Null),
                    }));
                }
                let failed = diags.iter().any(|d| d.get("severity").and_then(Value::as_str) == Some("error"));
                validators.push(json!({
                    "validator": vname,
                    "status": if failed { "failed" } else { "passed" },
                    "covers": full,
                }));
            }
        }
    }

    Ok(json!({
        "validators": validators,
        "diagnostics": diagnostics,
        "coverage": if is_js { "fail-fast" } else { "structural" },
    }))
}

/// One edited target, in both coordinate spaces (compareDiagnostics `targets`).
#[derive(Clone, Debug)]
pub struct DiagTarget {
    pub name: String,
    pub baseline_start: i64,
    pub baseline_end: i64,
    pub candidate_start: i64,
    pub candidate_end: i64,
}

fn normalise_message(m: &Value) -> String {
    let s = match m {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        v => v.to_string(),
    };
    s.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
}

fn target_at<'a>(targets: &'a [DiagTarget], offset: i64, baseline: bool) -> Option<&'a DiagTarget> {
    targets.iter().find(|t| {
        let (s, e) = if baseline { (t.baseline_start, t.baseline_end) } else { (t.candidate_start, t.candidate_end) };
        offset >= s && offset < e
    })
}

fn field(d: &Value, k: &str) -> String {
    match d.get(k) {
        None | Some(Value::Null) => "undefined".into(),
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
    }
}

fn start_of(d: &Value) -> i64 {
    d.get("start").and_then(Value::as_f64).map(|f| f as i64).unwrap_or(0)
}

/// validate.js#compareDiagnostics: structural matching, never a count. `map_offset` maps a
/// baseline offset to the candidate (None = the content there is gone).
pub fn compare_diagnostics(
    baseline: &[Value],
    candidate: &[Value],
    map_offset: Option<&dyn Fn(i64) -> Option<i64>>,
    targets: &[DiagTarget],
) -> Value {
    let baseline_broken = baseline.iter().any(|d| d.get("severity").and_then(Value::as_str) == Some("error"));
    let tail = |d: &Value| format!("{}::{}::{}::{}", field(d, "source"), field(d, "code"), field(d, "severity"), normalise_message(d.get("message").unwrap_or(&Value::Null)));

    // keyed maps preserve JS Map insertion order for `resolved`
    let mut inside: Vec<(String, Value)> = Vec::new();
    let mut outside: Vec<(String, Value)> = Vec::new();
    fn put(v: &mut Vec<(String, Value)>, k: String, d: Value) {
        if let Some(e) = v.iter_mut().find(|e| e.0 == k) {
            e.1 = d;
        } else {
            v.push((k, d));
        }
    }
    for d in baseline {
        let s = start_of(d);
        if let Some(t) = target_at(targets, s, true) {
            put(&mut inside, format!("{}::{}", t.name, tail(d)), d.clone());
            continue;
        }
        let mapped = match map_offset {
            Some(f) => f(s),
            None => Some(s),
        };
        let Some(mapped) = mapped else { continue };
        put(&mut outside, format!("{}::{}", mapped, tail(d)), d.clone());
    }

    let mut new_diagnostics = Vec::new();
    let mut remaining = Vec::new();
    let mut matched: HashMap<(bool, String), ()> = HashMap::new();
    for d in candidate {
        let s = start_of(d);
        let (key, is_inside) = match target_at(targets, s, false) {
            Some(t) => (format!("{}::{}", t.name, tail(d)), true),
            None => (format!("{}::{}", s, tail(d)), false),
        };
        let bucket = if is_inside { &inside } else { &outside };
        if bucket.iter().any(|e| e.0 == key) {
            matched.insert((is_inside, key), ());
            remaining.push(d.clone());
        } else {
            new_diagnostics.push(d.clone());
        }
    }
    // JS keys both buckets in one `matched` set; the two key spaces cannot collide in practice
    // (inside keys begin with a target name, outside keys with a number), so either bucket's match
    // counts for both.
    let is_matched = |k: &str| matched.contains_key(&(true, k.to_string())) || matched.contains_key(&(false, k.to_string()));
    let resolved: Vec<Value> = inside.iter().chain(outside.iter()).filter(|(k, _)| !is_matched(k)).map(|(_, d)| d.clone()).collect();

    let mut o = Map::new();
    o.insert("newDiagnostics".into(), Value::Array(new_diagnostics));
    o.insert("resolved".into(), Value::Array(resolved));
    o.insert("remaining".into(), Value::Array(remaining));
    o.insert("baselineBroken".into(), Value::Bool(baseline_broken));
    Value::Object(o)
}
