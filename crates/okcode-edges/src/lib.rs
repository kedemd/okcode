//! okcode-edges — resolved edges (lib/edges.js) and package surfaces (lib/analysis/packages.js).
pub mod edges;
pub mod packages;

use serde_json::{json, Value};

/// The JS-name dispatcher. `name` is the JS fn (`packages.declared`, ...).
pub fn dispatch(name: &str, args: &[Value]) -> Result<Value, String> {
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
        // Not dispatchable: these two are async in JS and take an access facade, which cannot be
        // rebuilt from JSON. The real functions are `packages::read_packages` / `scan_packages`.
        "readPackages" | "scanPackages" => Err("readPackages and scanPackages need an access facade: call them through a workspace".into()),
        other => Err(format!("unknown fn {other}")),
    }
}
