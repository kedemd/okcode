//! okcode-edges — port of src/edges.js and src/analysis/packages.js.
pub mod edges;
pub mod packages;

use serde_json::{json, Value};

/// Entry point for the golden test. `name` is the recorded JS fn (`packages.declared`, ...).
pub fn golden_call(name: &str, args: &[Value]) -> Result<Value, String> {
    let name = name.strip_prefix("packages.").unwrap_or(name);
    let text = |i: usize| args.get(i).and_then(|v| v.as_str()).unwrap_or("").to_string();
    match name {
        "namesFromDts" => Ok(json!(packages::names_from_dts(&text(0)))),
        "membersFromDts" => Ok(json!(packages::members_from_dts(&text(0)))),
        "reExports" => Ok(json!(packages::re_exports(&text(0)))),
        "declared" => {
            let m = packages::declared(args.first().unwrap_or(&Value::Null));
            Ok(json!({ "$map": m.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>() }))
        }
        // The recorded JS result of these two is `{}`: they are async, so the recorder serialised
        // the Promise, and `access` was a mock/localFs descriptor that cannot be rebuilt here.
        // The real functions are `packages::read_packages` / `scan_packages`; see PARITY.md.
        "readPackages" | "scanPackages" => Err("no golden: recorded out is a serialised Promise and access cannot be rebuilt".into()),
        other => Err(format!("unknown fn {other}")),
    }
}
