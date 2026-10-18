//! The JS-facing dispatch: method name + JSON args → the slice method. SHARED FILE (architect-owned).
//!
//! The binding calls `Workspace::call(method, args)` for every API method (args exactly as the JS
//! caller passed them, JSON-converted). Option structs are deserialised here with serde defaults,
//! so a slice changes its opts struct (in its own file) without touching this file. A panic in a
//! slice (todo!()) is caught by the binding and becomes a JS error.
use super::edit::{EditBatchArgs, EditOpts, WriteAtOpts, WriteWholeOpts};
use super::graph::{AskOpts, ImportersOpts, MentionsOpts, PackagesOpts, RefsOpts};
use super::lookup::{FilesOpts, FindOpts, OutlineOpts, ReadFileOpts, ReadOpts, StructureOpts, SymbolsOpts};
use super::scan::{RefreshOpts, SyncOpts};
use super::Workspace;
use crate::error::{Error, Result};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

/// Every method name the JS api object exposes as a function (workspace.js 2370-4093), in order.
/// `isLibraryPath` and `scanWarnings` are sync in JS; the binding calls them on the JS thread.
pub const METHODS: &[&str] = &[
    "refresh", "scanWarnings", "sync", "flush", "structure", "packages", "package", "find", "ask", "files", "symbols",
    "outline", "mentions", "grep", "isLibraryPath", "glob", "refs", "importers", "read", "readFile", "readAt", "editBatch",
    "edit", "writeAt", "writeWholeFile", "syntaxCheck", "stats", "close",
];
pub const SYNC_METHODS: &[&str] = &["isLibraryPath", "scanWarnings"];

fn arg(args: &[Value], i: usize) -> Value {
    args.get(i).cloned().unwrap_or(Value::Null)
}

/// JS `String(x)` for a path-ish argument (undefined/null → "").
fn s(args: &[Value], i: usize) -> String {
    match args.get(i) {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
    }
}

/// An options object; missing/null → defaults.
fn o<T: DeserializeOwned + Default>(args: &[Value], i: usize) -> Result<T> {
    match args.get(i) {
        None | Some(Value::Null) => Ok(T::default()),
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| Error::msg(format!("bad options: {e}"))),
    }
}

impl Workspace {
    pub fn call(&self, method: &str, args: &[Value]) -> Result<Value> {
        match method {
            "refresh" => self.refresh(o::<RefreshOpts>(args, 0)?),
            "scanWarnings" => Ok(self.scan_warnings()),
            "sync" => self.sync(o::<SyncOpts>(args, 0)?),
            "flush" => self.flush(),
            "structure" => self.structure(o::<StructureOpts>(args, 0)?),
            "packages" => self.packages(o::<PackagesOpts>(args, 0)?),
            "package" => self.package(&s(args, 0)),
            "find" => self.find(&arg(args, 0), o::<FindOpts>(args, 1)?),
            "ask" => self.ask(&arg(args, 0), o::<AskOpts>(args, 1)?),
            "files" => self.files(o::<FilesOpts>(args, 0)?),
            "symbols" => self.symbols(&s(args, 0), o::<SymbolsOpts>(args, 1)?),
            "outline" => self.outline(&s(args, 0), o::<OutlineOpts>(args, 1)?),
            "mentions" => self.mentions(&s(args, 0), o::<MentionsOpts>(args, 1)?),
            "grep" => self.grep(&arg(args, 0), &arg(args, 1)),
            "isLibraryPath" => Ok(json!(self.is_library_path(&s(args, 0)))),
            "glob" => self.glob(&arg(args, 0), &arg(args, 1)),
            "refs" => self.refs(&arg(args, 0), o::<RefsOpts>(args, 1)?),
            "importers" => self.importers(&s(args, 0), o::<ImportersOpts>(args, 1)?),
            "read" => self.read(&arg(args, 0), o::<ReadOpts>(args, 1)?),
            "readFile" => self.read_file(&s(args, 0), o::<ReadFileOpts>(args, 1)?),
            "readAt" => self.read_at(&s(args, 0), arg(args, 1), arg(args, 2), args.get(3).and_then(Value::as_str).map(String::from)),
            "editBatch" => self.edit_batch(o::<EditBatchArgs>(args, 0)?),
            "edit" => self.edit(&arg(args, 0), args.get(1).cloned(), o::<EditOpts>(args, 2)?),
            "writeAt" => self.write_at(&s(args, 0), arg(args, 1), arg(args, 2), args.get(3).cloned(), o::<WriteAtOpts>(args, 4)?),
            "writeWholeFile" => self.write_whole_file(&s(args, 0), &s(args, 1), o::<WriteWholeOpts>(args, 2)?),
            "syntaxCheck" => self.syntax_check(&s(args, 0)),
            "stats" => self.stats(),
            "close" => self.close(),
            other => Err(Error::msg(format!("workspace has no method {other}"))),
        }
    }
}

/// The pure functions workspace.js exports beside openWorkspace (compileGitignore is a closure in
/// JS: the binding's JS layer wraps `gitignoreMatches(text, rel)`).
pub fn call_pure(name: &str, args: &[Value]) -> Result<Value> {
    use super::{edit, lookup, scan};
    match name {
        "gitignoreMatches" => Ok(json!(scan::Gitignore::compile(&s(args, 0)).matches(&s(args, 1)))),
        "carveRegions" => Ok(Value::Array(lookup::carve_regions(arg(args, 0).as_array().map(Vec::as_slice).unwrap_or(&[])))),
        "splitPiece" => {
            let n = |i: usize| arg(args, i).as_f64().unwrap_or(0.0) as usize;
            let out = lookup::split_piece(n(0), n(1), &s(args, 2), n(3));
            Ok(Value::Array(out.into_iter().map(|(a, b)| json!([a, b])).collect()))
        }
        "classifyCommit" => Ok(json!(edit::classify_commit(&arg(args, 0)))),
        "parseRange" => Ok(match lookup::parse_range(&s(args, 0)) {
            Some(r) => json!({"file": r.file, "from": r.from, "to": r.to}),
            None => Value::Null,
        }),
        other => Err(Error::msg(format!("workspace.js exports no {other}"))),
    }
}
