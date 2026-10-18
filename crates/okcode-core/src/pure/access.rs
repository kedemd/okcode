//! pure/access: the pure exports of lib/access/{common,shell}.js (see pure.rs).
//! Facade objects (localFs/shell/fn) are in okcode-node/src/access_api.rs.
use crate::error::{Error, Result};
use okcode_analysis::access as acc;
use serde_json::{json, Value};

fn s(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => "undefined".into(),
        Some(v) => v.to_string(),
    }
}

/// None = not handled here (the router reports OKCODE_NOT_IMPLEMENTED).
pub fn call(module: &str, name: &str, args: &[Value]) -> Option<Result<Value>> {
    let r = match (module, name) {
        ("access/common", "DEFAULT_SKIP") => Ok(json!(acc::DEFAULT_SKIP)),
        ("access/common", "PRUNE_DOT_DIRS") => Ok(json!(acc::PRUNE_DOT_DIRS)),
        ("access/common", "checkPath") => check_path(args),
        ("access/common", "mtimeOf") => {
            let n = |i: usize| args.get(i).and_then(|v| v.as_i64()).unwrap_or(0);
            Ok(json!(acc::common::mtime_of(n(0), n(1))))
        }
        ("access/common", "procStartTime") => {
            let pid = args.first().and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())));
            Ok(pid.and_then(acc::common::proc_start_time).map_or(Value::Null, Value::String))
        }
        ("access/shell", "quote") => Ok(Value::String(acc::shell::quote(&s(args.first())))),
        _ => return None,
    };
    Some(r)
}

fn check_path(args: &[Value]) -> Result<Value> {
    let dialect = match args.get(1) {
        None | Some(Value::Null) => acc::Dialect::Posix,
        Some(Value::String(d)) if d == "posix" => acc::Dialect::Posix,
        Some(Value::String(d)) if d == "windows" => acc::Dialect::Windows,
        Some(v) => return Err(Error::msg(format!("unknown path dialect: {}", s(Some(v))))),
    };
    match args.first() {
        Some(Value::String(p)) => acc::check_path(p, dialect).map(Value::String).map_err(Error::msg),
        other => {
            let shown = match other {
                None | Some(Value::Null) => "undefined".to_string(),
                Some(v) => v.to_string(),
            };
            Err(Error::msg(format!("invalid workspace path: {shown}")))
        }
    }
}
