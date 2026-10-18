//! pure/ts: the pure exports of analysis/treesitter.js and analysis/validate.js (see pure.rs).
use crate::error::{Error, Result};
use okcode_analysis::text::convention_color::Palette;
use okcode_analysis::validate::{compare_diagnostics, validate_source_ext, DiagTarget, ValidateArgs};
use okcode_analysis::treesitter as ts;
use serde_json::{json, Value};
use std::collections::HashMap;

fn s_arg(args: &[Value], i: usize, what: &str) -> Result<String> {
    args.get(i).and_then(Value::as_str).map(str::to_string).ok_or_else(|| Error::msg(format!("{what}: expected a string")))
}

fn opt(o: Option<String>) -> Value {
    o.map_or(Value::Null, Value::String)
}

fn palette_of(v: &Value) -> Vec<Palette> {
    v.as_array()
        .map(|a| {
            a.iter()
                .map(|p| Palette {
                    name: p["name"].as_str().unwrap_or("").to_string(),
                    value: p["value"].as_str().unwrap_or("").to_string(),
                    rgb: [0, 1, 2].map(|k| p["rgb"][k].as_i64().unwrap_or(0)),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn num(v: &Value) -> i64 {
    v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)).unwrap_or(0)
}

fn goals(kind: &str, module: &str, script: &str) -> Value {
    match kind {
        "module" => json!([module]),
        "commonjs" => json!([script]),
        _ => json!([script, module]),
    }
}

/// None = not handled here (the router reports OKCODE_NOT_IMPLEMENTED).
pub fn call(module: &str, name: &str, args: &[Value]) -> Option<Result<Value>> {
    match (module, name) {
        ("analysis/treesitter", "langIdForPath") => Some(s_arg(args, 0, name).map(|p| opt(ts::lang_id_for_path(&p).map(str::to_string)))),
        ("analysis/treesitter", "currentVersionFor") => Some(s_arg(args, 0, name).map(|p| opt(ts::current_version_for(&p)))),
        // Grammars are compiled in: "loading" one is knowing the id. Truthy for a known language, null otherwise.
        ("analysis/treesitter", "ensureLanguage") => Some(s_arg(args, 0, name).map(|id| {
            let known = ts::languages_json().get(&id).is_some();
            if known { json!({"id": id}) } else { Value::Null }
        })),
        ("analysis/treesitter", "extractFor") => Some((|| {
            let (id, p, src) = (s_arg(args, 0, name)?, s_arg(args, 1, name)?, s_arg(args, 2, name)?);
            if ts::languages_json().get(&id).is_none() {
                return Ok(Value::Null);
            }
            ts::extract_for(&id, &p, &src).map_err(Error::msg)
        })()),
        ("analysis/treesitter", "syntaxDiagnostics") => Some((|| {
            let (p, t) = (s_arg(args, 0, name)?, s_arg(args, 1, name)?);
            Ok(Value::Array(ts::syntax_diagnostics(&p, &t)))
        })()),
        ("analysis/treesitter", "LANGUAGES") => Some(Ok(ts::languages_json())),
        ("analysis/treesitter", "jsxGrammarAvailable") => Some(Ok(Value::Bool(true))),
        ("analysis/treesitter", "jsxSyntaxDiagnostics") => Some(s_arg(args, 0, name).map(|t| Value::Array(ts::jsx_syntax_diagnostics(&t)))),
        ("analysis/treesitter", "jsxFallbackVersionTag") => Some(Ok(Value::String(ts::jsx_fallback_version_tag()))),
        ("analysis/treesitter", "extractJsxFallback") => Some(s_arg(args, 1, name).and_then(|t| ts::extract_jsx_fallback(&t).map_err(Error::msg))),

        ("analysis/validate", "validateSource") => Some((|| {
            let o = args.first().ok_or_else(|| Error::msg("validateSource: expected an object"))?;
            let a = ValidateArgs {
                rel: o["rel"].as_str().ok_or_else(|| Error::msg("validateSource: rel"))?.to_string(),
                text: o["text"].as_str().ok_or_else(|| Error::msg("validateSource: text"))?.to_string(),
                module_kind: o["moduleKind"].as_str().unwrap_or("ambiguous").to_string(),
                palette: palette_of(&o["palette"]),
            };
            validate_source_ext(&a, o.get("extOutcome").filter(|v| !v.is_null())).map_err(Error::msg)
        })()),
        ("analysis/validate", "acornGoalsFor") => Some(Ok(goals(args.first().and_then(Value::as_str).unwrap_or(""), "module", "script"))),
        ("analysis/validate", "nodeCheckGoalsFor") => Some(Ok(goals(args.first().and_then(Value::as_str).unwrap_or(""), "module", "commonjs"))),
        // (baseline, candidate, { mapped: [[start, mappedStart|null], ..], targets }): the JS side
        // evaluates its mapOffset closure per baseline start; the matching is native.
        ("analysis/validate", "compareDiagnostics") => Some((|| {
            let arr = |i: usize| args.get(i).and_then(Value::as_array).cloned().unwrap_or_default();
            let (baseline, candidate) = (arr(0), arr(1));
            let opts = args.get(2).cloned().unwrap_or(Value::Null);
            let has_map = opts.get("mapped").map_or(false, |m| !m.is_null());
            let mut table: HashMap<i64, Option<i64>> = HashMap::new();
            for pair in opts["mapped"].as_array().into_iter().flatten() {
                table.insert(num(&pair[0]), if pair[1].is_null() { None } else { Some(num(&pair[1])) });
            }
            let map = |o: i64| table.get(&o).copied().unwrap_or(Some(o));
            let targets: Vec<DiagTarget> = opts["targets"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|t| DiagTarget {
                    name: match &t["name"] {
                        Value::String(s) => s.clone(),
                        Value::Null => "undefined".into(),
                        v => v.to_string(),
                    },
                    baseline_start: num(&t["baselineStart"]),
                    baseline_end: num(&t["baselineEnd"]),
                    candidate_start: num(&t["candidateStart"]),
                    candidate_end: num(&t["candidateEnd"]),
                })
                .collect();
            let f: &dyn Fn(i64) -> Option<i64> = &map;
            Ok(compare_diagnostics(&baseline, &candidate, if has_map { Some(f) } else { None }, &targets))
        })()),
        _ => None,
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goals_shapes() {
        assert_eq!(goals("module", "module", "script"), json!(["module"]));
        assert_eq!(goals("commonjs", "module", "commonjs"), json!(["commonjs"]));
        assert_eq!(goals("ambiguous", "module", "commonjs"), json!(["commonjs", "module"]));
    }

    #[test]
    fn compare_identity_and_mapped() {
        let d = json!({"source": "s", "code": "c", "severity": "error", "message": "m", "start": 5});
        let r = call("analysis/validate", "compareDiagnostics", &[json!([d]), json!([d]), json!({})]).unwrap().unwrap();
        assert_eq!(r["remaining"].as_array().unwrap().len(), 1);
        let cand = json!({"source": "s", "code": "c", "severity": "error", "message": "m", "start": 9});
        let r = call("analysis/validate", "compareDiagnostics", &[json!([d]), json!([cand]), json!({"mapped": [[5, 9]]})]).unwrap().unwrap();
        assert_eq!(r["remaining"].as_array().unwrap().len(), 1);
        let r = call("analysis/validate", "compareDiagnostics", &[json!([d]), json!([cand]), json!({"mapped": [[5, null]]})]).unwrap().unwrap();
        assert_eq!(r["newDiagnostics"].as_array().unwrap().len(), 1);
    }
}
