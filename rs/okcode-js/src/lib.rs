//! okcode-js — port of analysis/parse.js (generic extract paths, JS extraction) and analysis/validate.js (JS syntax gate).
//! oxc replaces acorn; see PARITY.md for what differs.
pub mod parse;
pub mod template_balance;
pub mod text;
pub mod validate;

use serde_json::Value;

/// Cases this crate deliberately does not answer carry this prefix in their error.
pub const OUT_OF_SCOPE: &str = "out-of-scope:";

pub fn golden_call(name: &str, args: &[Value]) -> Result<Value, String> {
    match name {
        "langOf" => {
            let p = args.first().and_then(Value::as_str).ok_or("langOf: path")?;
            Ok(Value::from(parse::lang_of(p)))
        }
        "extract" => {
            let p = args.first().and_then(Value::as_str).ok_or("extract: path")?;
            parse::extract(p, args.get(1).and_then(Value::as_str))
        }
        "validateSource" => {
            let o = args.first().ok_or("validateSource: args")?;
            let rel = o["rel"].as_str().ok_or("validateSource: rel")?;
            let text = o["text"].as_str().ok_or("validateSource: text")?;
            validate::validate_source(&validate::Args {
                rel,
                text,
                module_kind: o["moduleKind"].as_str().unwrap_or("ambiguous"),
                has_palette: o["palette"].as_array().map_or(false, |a| !a.is_empty()),
            })
        }
        other => Err(format!("unknown golden fn {other}")),
    }
}
