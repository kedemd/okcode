//! Edit commands: edit, edit-batch, write, check — and reportEdit, the honest receipt.

use crate::args::Flag;
use crate::look::{items, plain, u, Ws, R};
use crate::open::truthy;
use crate::render::{err, j, js_str, out};
use crate::Exit;
use serde_json::{json, Value};

fn exit1() -> Exit {
    Exit { code: 1, message: String::new() }
}

/// A flag given with a value (not bare, not absent): `flags.k && flags.k !== true`.
fn val(a: &crate::args::Args, k: &str) -> Option<String> {
    match a.flags.get(k) {
        Some(Flag::Val(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// fs.readFileSync(file, 'utf8'); an error reads as Node's (`ENOENT: no such file or directory,
/// open '<file>'`), the code is returned separately for main's `${code}: ${message}`.
fn read_body(file: &str) -> Result<String, (String, String)> {
    std::fs::read(file).map(|b| String::from_utf8_lossy(&b).into_owned()).map_err(|e| {
        let (code, what) = match e.kind() {
            std::io::ErrorKind::NotFound => ("ENOENT", "no such file or directory"),
            std::io::ErrorKind::PermissionDenied => ("EACCES", "permission denied"),
            std::io::ErrorKind::IsADirectory => ("EISDIR", "illegal operation on a directory"),
            _ => ("EIO", "i/o error"),
        };
        let msg = if code == "EISDIR" { format!("{code}: {what}, read") } else { format!("{code}: {what}, open '{file}'") };
        (code.to_string(), msg)
    })
}

/// An uncaught readBody error, as main's catch prints it.
fn uncaught((code, msg): (String, String)) -> Exit {
    Exit::fail(format!("{code}: {msg}"))
}

/// Print the receipt honestly: an outcome other than 'ok' is real evidence something was
/// attempted, never collapsed into a bare pass/fail.
fn report_edit(w: &Ws, r: &Value) -> R {
    let ok = r["outcome"].as_str() == Some("ok");
    if w.json() {
        j(&plain(r));
        return if ok { Ok(()) } else { Err(exit1()) };
    }
    if !truthy(&r["outcome"]) {
        err(&format!("REFUSED: {}", u(r, "reason")));
        if truthy(&r["stale"]) && truthy(&r["at"]) {
            err(&format!("  current at={}", js_str(&r["at"])));
        }
        for c in items(&r["candidates"]) {
            err(&format!("  candidate: {}", js_str(&c)));
        }
        for d in items(&r["newDiagnostics"]).iter().take(10) {
            let src = if truthy(&d["source"]) { format!("[{}] ", js_str(&d["source"])) } else { String::new() };
            let line = if truthy(&d["line"]) { format!("line {}: ", js_str(&d["line"])) } else { String::new() };
            err(&format!("  {src}{line}{}", u(d, "message")));
        }
        return Err(exit1());
    }
    err(&format!("{}  {}  operation={}", u(r, "outcome"), u(r, "file"), u(r, "operationId")));
    let h = |k: &str| if truthy(&r[k]) { js_str(&r[k]).chars().take(12).collect() } else { String::new() };
    err(&format!(
        "  beforeHash={} candidateHash={} commitReportedHash={} readbackHash={}",
        h("beforeHash"),
        h("candidateHash"),
        h("commitReportedHash"),
        h("readbackHash")
    ));
    if truthy(&r["readbackHash"]) {
        err(&format!("# at={}", js_str(&r["readbackHash"])));
    }
    if truthy(&r["baselineBroken"]) {
        err(&format!("  baselineBroken=true ({} pre-existing diagnostic(s) unchanged)", items(&r["remaining"]).len()));
    }
    let failed: Vec<String> = items(&r["validation"])
        .iter()
        .filter(|v| v["status"].as_str() == Some("failed"))
        .map(|v| if v["validator"].is_null() { String::new() } else { js_str(&v["validator"]) })
        .collect();
    if !failed.is_empty() {
        err(&format!("  WARNING: {} validator(s) reported issues (not blocking)", failed.join(", ")));
    }
    if truthy(&r["reason"]) && !ok {
        err(&format!("  {}", js_str(&r["reason"])));
    }
    for hunk in items(&r["diff"]) {
        out(&u(&hunk, "text"));
        out("");
    }
    if ok { Ok(()) } else { Err(exit1()) }
}

pub fn edit(w: &Ws) -> R {
    let a = w.a;
    let target = a.positional.first().cloned().filter(|t| !t.is_empty());
    let (Some(target), Some(file)) = (target, val(a, "file")) else {
        return Err(Exit::fail("edit needs <symbol> --file <body.txt> --at HASH".into()));
    };
    let Some(at) = val(a, "at") else {
        return Err(Exit::fail("edit needs --at — read the symbol or file first (its at= is printed to stderr) and pass that token".into()));
    };
    let body = read_body(&file).map_err(uncaught)?;
    let body = body.strip_suffix('\n').unwrap_or(&body).to_string();
    let r = w.call("edit", vec![json!(target), json!(body), json!({ "at": at })])?;
    report_edit(w, &r)
}

pub fn edit_batch(w: &Ws) -> R {
    let a = w.a;
    let Some(file) = val(a, "batch") else {
        return Err(Exit::fail("edit-batch needs --json <batch.json>: { \"at\": \"HASH\", \"edits\": [{ \"target\", \"body\" }] }".into()));
    };
    // a JSON syntax error's message is serde's, not V8's
    let batch: Value = read_body(&file)
        .map_err(|(_, m)| m)
        .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
        .map_err(|m| Exit::fail(format!("edit-batch: {file}: {m}")))?;
    if !truthy(&batch["at"]) {
        return Err(Exit::fail("edit-batch: the batch JSON needs \"at\" — a missing token is always refused".into()));
    }
    let r = w.call("editBatch", vec![batch])?;
    report_edit(w, &r)
}

pub fn write(w: &Ws) -> R {
    let a = w.a;
    let rel = a.positional.first().cloned().filter(|t| !t.is_empty());
    let (Some(rel), Some(file)) = (rel, val(a, "file")) else {
        return Err(Exit::fail("write needs <file> --file <text.txt> (--create | --at HASH)".into()));
    };
    let create = a.on("create");
    let at = val(a, "at");
    if !create && at.is_none() {
        return Err(Exit::fail("write needs --create for a new file, or --at HASH (from a read) to replace an existing one".into()));
    }
    if create && at.is_some() {
        return Err(Exit::fail("write takes --create OR --at, not both".into()));
    }
    let body = read_body(&file).map_err(uncaught)?;
    let r = w.call("writeWholeFile", vec![json!(rel), json!(body), json!({ "create": create, "expectedHash": at })])?;
    report_edit(w, &r)
}

pub fn check(w: &Ws) -> R {
    let file = crate::need(w.a.positional.first().cloned(), "check needs <file>")?;
    let r = w.call("syntaxCheck", vec![json!(file)])?;
    j(&plain(&r));
    if truthy(&r["ok"]) { Ok(()) } else { Err(exit1()) }
}
