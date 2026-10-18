//! JS syntax gate (analysis/validate.js). oxc stands in for both acorn and `node --check`.
use crate::parse::{self, acorn_message, fail_of, lang_of, okjs_owns, Fail, Geo, Goal};
use crate::template_balance;
use crate::text::{line_col_at, offset_at_line_col};
use oxc_allocator::Allocator;
use serde_json::{json, Value};

fn ts_lang_for(rel: &str) -> bool {
    matches!(
        parse::ext_of(rel).as_str(),
        ".ts" | ".mts" | ".cts" | ".tsx" | ".py" | ".go" | ".rs" | ".php" | ".phtml" | ".java" | ".cs" | ".c" | ".cpp" | ".cc" | ".cxx" | ".h" | ".hpp" | ".hh" | ".hxx" | ".ipp"
    )
}

fn try_goals(text: &str, goals: &[Goal], jsx: bool) -> Result<(), Fail> {
    let mut last = None;
    for &g in goals {
        let alloc = Allocator::default();
        let r = parse::parse(&alloc, text, g, jsx);
        match fail_of(&r) {
            None => return Ok(()),
            Some(f) => last = Some(f),
        }
    }
    Err(last.unwrap())
}

/// node's own wording for the common cases; best effort.
fn node_message(text: &str, f: &Fail) -> String {
    if f.message.starts_with("Unexpected token") || f.message.starts_with("Expected") {
        if f.pos >= text.len() {
            return "Unexpected end of input".into();
        }
        let tok: String = text[f.pos..].chars().take(1).collect();
        let end = (f.pos + f.len).min(text.len());
        let tok = if f.len > 0 && text.is_char_boundary(end) { text[f.pos..end].to_string() } else { tok };
        return format!("Unexpected token '{tok}'");
    }
    f.message.clone()
}

pub struct Args<'a> {
    pub rel: &'a str,
    pub text: &'a str,
    pub module_kind: &'a str,
    pub has_palette: bool,
}

pub fn validate_source(a: &Args) -> Result<Value, String> {
    let is_js = lang_of(a.rel) == "javascript";
    let ext = okjs_owns(a.rel);
    if !is_js && !ext && !ts_lang_for(a.rel) {
        return Ok(json!({
            "validators": [{"validator": "none", "status": "skipped", "covers": []}],
            "diagnostics": [],
            "coverage": "complete",
        }));
    }
    if ext {
        return Err("out-of-scope: owned by the okjs extension (okcode-okjs)".into());
    }
    if !is_js {
        return Err("out-of-scope: tree-sitter syntax gate (okcode-treesitter)".into());
    }
    if a.has_palette {
        return Err("out-of-scope: convention-color palette check (okcode-text)".into());
    }
    let text = a.text;
    let u16s: Vec<u16> = text.encode_utf16().collect();
    let g = Geo::new(text);
    let full = json!([{"start": 0, "end": u16s.len()}]);
    let mut validators: Vec<Value> = Vec::new();
    let mut diagnostics: Vec<Value> = Vec::new();

    let (acorn_goals, node_goals): (&[Goal], &[Goal]) = match a.module_kind {
        "module" => (&[Goal::Module], &[Goal::Module]),
        "commonjs" => (&[Goal::Script], &[Goal::CommonJs]),
        _ => (&[Goal::Script, Goal::Module], &[Goal::CommonJs, Goal::Module]),
    };

    let acorn = try_goals(text, acorn_goals, false);
    let unparseable = acorn.is_err() && try_goals(text, &[Goal::Script, Goal::Module], false).is_err();
    let looks_like_jsx = unparseable
        && acorn.as_ref().err().map_or(false, |f| text.as_bytes().get(f.pos) == Some(&b'<'));
    // oxc parses JSX itself, so "the JSX grammar parses it cleanly" is the tree-sitter fallback's stand-in.
    let jsx_fallback = looks_like_jsx && try_goals(text, &[Goal::Module], true).is_ok();

    let status = if acorn.is_ok() { "passed" } else if jsx_fallback { "skipped" } else { "failed" };
    let mut v = json!({"validator": "javascript-acorn", "status": status, "covers": if acorn.is_ok() { full.clone() } else { json!([]) }});
    if jsx_fallback {
        v["note"] = json!("acorn cannot parse JSX; using tree-sitter-syntax instead");
    }
    validators.push(v);
    if let (Err(f), false) = (&acorn, jsx_fallback) {
        let pos = g.u16(f.pos.min(text.len()));
        let (line, column) = line_col_at(&u16s, pos);
        diagnostics.push(json!({
            "source": "javascript-acorn", "code": "SyntaxError", "severity": "error",
            "message": acorn_message(&g, f), "start": pos, "end": pos, "line": line, "column": column,
        }));
    }

    if jsx_fallback {
        validators.push(json!({
            "validator": "javascript-node-check", "status": "skipped", "covers": [],
            "note": "skipped: acorn cannot parse JSX, node --check would fail the same way",
        }));
        validators.push(json!({"validator": "treesitter-syntax", "status": "passed", "covers": full}));
    } else {
        // Node is not available to the Rust build: the runtime's verdict is emulated with oxc under node's goals.
        let nc = try_goals(text, node_goals, false);
        validators.push(json!({"validator": "javascript-node-check", "status": if nc.is_ok() { "passed" } else { "failed" }, "covers": full.clone()}));
        if let Err(f) = nc {
            let pos0 = g.u16(f.pos.min(text.len()));
            let (line, column) = line_col_at(&u16s, pos0);
            let offset = offset_at_line_col(&u16s, line, column);
            diagnostics.push(json!({
                "source": "javascript-node-check", "code": "SyntaxError", "severity": "error",
                "message": node_message(text, &f), "start": offset, "end": offset, "line": line, "column": column,
            }));
        }
    }

    let tb = template_balance::check(&u16s);
    let failed = !tb.is_empty();
    for d in tb {
        diagnostics.push(json!({
            "source": "template-balance", "code": d.code, "severity": "error", "message": d.message,
            "start": d.start, "end": d.end, "line": d.line, "column": d.column,
        }));
    }
    validators.push(json!({"validator": "template-balance", "status": if failed { "failed" } else { "passed" }, "covers": full}));

    Ok(json!({"validators": validators, "diagnostics": diagnostics, "coverage": "fail-fast"}))
}
