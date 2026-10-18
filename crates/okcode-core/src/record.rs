//! One file's record (workspace.js `files` map value).
//!
//! JS: `{ ...row, ...extract(path, text), path, rel, hash, lossless, content? }` — the facade row
//! (path, size, mtime, hash?) spread with the extract() envelope. The fields every verb reads are
//! typed; whatever else an analyser returned (references, diagnostics, coverage, regions,
//! okAnalysis, extractor, …) rides along in `extra` in insertion order, so `to_json` reproduces
//! the JS object.
use serde_json::{json, Map, Value};
use std::sync::Arc;

#[derive(Clone, Debug, Default)]
pub struct FileRecord {
    /// workspace-relative, `/`-separated (`path` and `rel` in JS are the same string)
    pub rel: String,
    pub size: u64,
    /// `null` in JS when the bytes were observed without a stat (ensureTexts) — re-verify next time.
    pub mtime: Option<f64>,
    /// uppercase hex SHA-1 of the bytes that were parsed
    pub hash: Option<String>,
    pub lang: String,
    pub lines: u64,
    pub symbols: Vec<Value>,
    pub imports: Vec<Value>,
    pub exports: Vec<Value>,
    pub parsed: bool,
    /// false = opaque (binary, oversized, secret): hashed, never read
    pub indexed: bool,
    pub reason: Option<String>,
    pub lossless: bool,
    pub analyzer_version: Option<String>,
    /// every other extract() field, in order
    pub extra: Map<String, Value>,
    /// the bounded text cache (state::Index::cache_content) — never persisted
    pub content: Option<Arc<str>>,
}

const TYPED: &[&str] = &[
    "path", "rel", "size", "mtime", "hash", "lang", "lines", "symbols", "imports", "exports", "parsed", "indexed", "reason",
    "lossless", "analyzerVersion", "content",
];

fn arr(v: Option<&Value>) -> Vec<Value> {
    v.and_then(Value::as_array).cloned().unwrap_or_default()
}

impl FileRecord {
    /// `{ ...row, ...parsed, path, rel, hash, lossless }` (workspace.js#ingestOne).
    pub fn from_extract(rel: &str, size: u64, mtime: Option<f64>, hash: Option<String>, parsed: Value, lossless: bool) -> FileRecord {
        let mut r = FileRecord::from_json(rel, &parsed);
        r.size = size;
        r.mtime = mtime;
        r.hash = hash;
        r.lossless = lossless;
        r
    }

    /// From a stored row / any JS-shaped record object (store.load).
    pub fn from_json(rel: &str, v: &Value) -> FileRecord {
        let o = v.as_object().cloned().unwrap_or_default();
        let s = |k: &str| o.get(k).and_then(Value::as_str).map(String::from);
        let mut extra = Map::new();
        for (k, val) in &o {
            if !TYPED.contains(&k.as_str()) {
                extra.insert(k.clone(), val.clone());
            }
        }
        FileRecord {
            rel: rel.to_string(),
            size: o.get("size").and_then(Value::as_f64).unwrap_or(0.0) as u64,
            mtime: o.get("mtime").and_then(Value::as_f64),
            hash: s("hash"),
            lang: s("lang").unwrap_or_else(|| "other".into()),
            lines: o.get("lines").and_then(Value::as_f64).unwrap_or(0.0) as u64,
            symbols: arr(o.get("symbols")),
            imports: arr(o.get("imports")),
            exports: arr(o.get("exports")),
            parsed: o.get("parsed").and_then(Value::as_bool).unwrap_or(false),
            indexed: o.get("indexed").and_then(Value::as_bool).unwrap_or(true),
            reason: s("reason"),
            lossless: o.get("lossless").and_then(Value::as_bool).unwrap_or(true),
            analyzer_version: s("analyzerVersion"),
            extra,
            content: None,
        }
    }

    /// The JS object (without `content` unless asked).
    pub fn to_json(&self, with_content: bool) -> Value {
        let mut m = Map::new();
        m.insert("path".into(), json!(self.rel));
        m.insert("size".into(), json!(self.size));
        m.insert("mtime".into(), json!(self.mtime));
        if let Some(h) = &self.hash {
            m.insert("hash".into(), json!(h));
        }
        m.insert("lang".into(), json!(self.lang));
        m.insert("lines".into(), json!(self.lines));
        m.insert("symbols".into(), Value::Array(self.symbols.clone()));
        m.insert("imports".into(), Value::Array(self.imports.clone()));
        m.insert("exports".into(), Value::Array(self.exports.clone()));
        m.insert("parsed".into(), json!(self.parsed));
        m.insert("indexed".into(), json!(self.indexed));
        if let Some(r) = &self.reason {
            m.insert("reason".into(), json!(r));
        }
        if let Some(a) = &self.analyzer_version {
            m.insert("analyzerVersion".into(), json!(a));
        }
        for (k, v) in &self.extra {
            m.insert(k.clone(), v.clone());
        }
        m.insert("rel".into(), json!(self.rel));
        m.insert("lossless".into(), json!(self.lossless));
        if with_content {
            if let Some(c) = &self.content {
                m.insert("content".into(), json!(&**c));
            }
        }
        Value::Object(m)
    }
}
