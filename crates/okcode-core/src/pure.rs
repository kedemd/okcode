//! The pure exports of the lib/ modules, for their thin JS
//! wrappers (lib/native/pure.js). `call(module, name, args)` takes
//! and returns the JS shapes (JSON conventions: $buf/$map/$set/$re, paged $arr/$props).
//! The router only; each module group lives in pure/<group>.rs.
use crate::error::{Error, Result};
use serde_json::Value;

mod access;
mod misc;
mod parse;
mod text;
mod ts;

/// `module` is the src/-relative path without `.js` (`analysis/text`, `grep`, `access/shell`).
pub fn call(module: &str, name: &str, args: &[Value]) -> Result<Value> {
    let r = match module {
        "analysis/text" | "analysis/chunk" | "analysis/template-balance" | "analysis/convention-color" => text::call(module, name, args),
        "analysis/parse" | "analysis/extensions" | "analysis/ext-okjs" | "analysis/packages" | "analysis/index" => parse::call(module, name, args),
        "analysis/treesitter" | "analysis/validate" => ts::call(module, name, args),
        "grep" | "identity" | "edges" | "embedders" => misc::call(module, name, args),
        m if m.starts_with("access/") || m == "access" => access::call(module, name, args),
        _ => None,
    };
    r.unwrap_or_else(|| Err(Error::todo(&format!("{module}.{name}"))))
}

/// dispatch's `Result<Value, String>` as a pure-call result.
pub(crate) fn g(r: std::result::Result<Value, String>) -> Option<Result<Value>> {
    Some(r.map_err(Error::msg))
}
