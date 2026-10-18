//! The chunker behind lib/analysis/chunk.js.
pub mod chunk;
pub use chunk::*;

use serde_json::{json, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64().map_or(true, |f| f != 0.0 && !f.is_nan()),
        _ => true,
    }
}

fn js_string(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(o) => o.to_string(),
    }
}

pub fn dispatch(name: &str, args: &[Value]) -> Result<Value, String> {
    let a = |i: usize| args.get(i);
    match name {
        "chunkCode" => {
            let text = match a(0) {
                Some(Value::String(s)) => s.as_str(),
                _ => return Ok(json!([])),
            };
            let rel = a(1).and_then(|v| v.as_str()).filter(|s| !s.is_empty());
            let out = chunk_code(text, rel);
            Ok(Value::Array(out.into_iter().map(|c| json!({"text": c.text, "start": c.start, "end": c.end})).collect()))
        }
        "regions" => {
            let rel = js_string(a(0));
            let text = match a(1) {
                Some(Value::String(s)) => s.as_str(),
                _ => return Ok(json!([])),
            };
            let (mut max_lines, mut log_lines) = (REGION_LINES as i64, LOG_LINES as i64);
            if let Some(Value::Object(o)) = a(2) {
                if let Some(n) = o.get("maxLines").and_then(|v| v.as_f64()) {
                    max_lines = n as i64;
                }
                if let Some(n) = o.get("logLines").and_then(|v| v.as_f64()) {
                    log_lines = n as i64;
                }
            }
            Ok(regions(&rel, text, max_lines, log_lines).unwrap_or(Value::Null))
        }
        "sectionOf" => {
            let rel = js_string(a(0));
            let text = match a(1) {
                Some(Value::String(s)) => s.as_str(),
                _ => "",
            };
            let title = if truthy(a(2)) { js_string(a(2)) } else { String::new() };
            Ok(section_of(&rel, text, &title))
        }
        "prepareText" => {
            let rel = if truthy(a(0)) { js_string(a(0)) } else { String::new() };
            let indexed = match a(2) {
                Some(Value::Object(o)) => match o.get("indexed") {
                    None => true,
                    Some(v) => truthy(Some(v)),
                },
                _ => true,
            };
            match a(1) {
                Some(Value::String(t)) => Ok(Value::String(prepare_text(&rel, Some(t), indexed))),
                _ => Ok(Value::String(prepare_text(&rel, None, indexed))),
            }
        }
        _ => Err(format!("unknown fn {name}")),
    }
}
