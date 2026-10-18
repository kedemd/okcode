//! ask / refs / importers / mentions / packages.
//!
//! cappedList, the edge index (okcode-edges `EdgeIndex`, keyed by `generation`), mentionsOf; api
//! packages, package, ask, mentions, refs, importers. Package surfaces: okcode-edges `packages`
//! (scan_packages/read_packages, with okcode_analysis::extract for exports).
use super::lookup::{owner_of, FindOpts};
use super::Workspace;
use crate::error::{Error, Result};
use crate::store::{as_query, AskOptions};
use okcode_analysis::access::Access as FsAccess;
use okcode_analysis::edges::edges::{EdgeIndex, FileRow, Import, Name, Symbol};
use okcode_analysis::edges::packages::{read_packages, scan_packages, Access as EdgeFs, Package};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// Bridges the okcode Access facade to the edges crate's read-only Access trait. A failed read
/// answers "nothing there", same as a missing file.
pub struct EdgeAccess(pub Arc<dyn FsAccess>);

impl EdgeFs for EdgeAccess {
    fn read(&self, paths: &[String]) -> HashMap<String, Vec<u8>> {
        self.0.read(paths).map(|v| v.into_iter().collect()).unwrap_or_default()
    }
}

/// The edge index and the generation it was built for, behind its own mutex.
#[derive(Default)]
pub struct GraphState {
    edges: Mutex<Option<(std::sync::Arc<EdgeIndex<EdgeAccess>>, u64)>>,
}


fn js_ws(c: char) -> bool {
    c.is_whitespace() || c == '\u{feff}'
}

/// JS `Number(x)` over a JSON value (NaN for anything not number-like).
fn js_number(v: &Value) -> f64 {
    match v {
        Value::Null => 0.0,
        Value::Bool(b) => *b as u8 as f64,
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::String(s) => {
            let t = s.trim_matches(js_ws);
            if t.is_empty() { 0.0 } else { t.parse::<f64>().unwrap_or(f64::NAN) }
        }
        _ => f64::NAN,
    }
}

/// `Number(x) > 0 ? Math.floor(Number(x)) : fallback`, never failing.
fn lenient(v: &Value, fallback: usize) -> usize {
    let n = js_number(v);
    if n > 0.0 {
        n.floor().min((usize::MAX / 4) as f64) as usize
    } else {
        fallback
    }
}

fn de_lim5<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<usize, D::Error> {
    Ok(lenient(&Value::deserialize(d)?, 5))
}
fn de_lim12<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<usize, D::Error> {
    // JS ask: plain destructuring default (missing only); the caller's number passes through.
    let n = js_number(&Value::deserialize(d)?);
    Ok(if n.is_nan() || n <= 0.0 { 0 } else { n.floor().min((usize::MAX / 4) as f64) as usize })
}
fn de_lim40<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<usize, D::Error> {
    Ok(lenient(&Value::deserialize(d)?, 40))
}
fn de_off<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<usize, D::Error> {
    Ok(lenient(&Value::deserialize(d)?, 0))
}
fn de_bool<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<bool, D::Error> {
    Ok(truthy(&Value::deserialize(d)?))
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PackagesOpts {
    #[serde(deserialize_with = "de_bool")]
    pub refresh: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AskOpts {
    #[serde(deserialize_with = "de_lim12")]
    pub limit: usize,
    pub profile: Option<String>,
    pub path: Option<Value>,
}
impl Default for AskOpts {
    fn default() -> Self {
        AskOpts { limit: 12, profile: None, path: None }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MentionsOpts {
    #[serde(deserialize_with = "de_lim5")]
    pub limit: usize,
    #[serde(deserialize_with = "de_off")]
    pub offset: usize,
}
impl Default for MentionsOpts {
    fn default() -> Self {
        MentionsOpts { limit: 5, offset: 0 }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RefsOpts {
    #[serde(deserialize_with = "de_lim40")]
    pub limit: usize,
    #[serde(deserialize_with = "de_off")]
    pub offset: usize,
    #[serde(deserialize_with = "de_bool")]
    pub include_self: bool,
}
impl Default for RefsOpts {
    fn default() -> Self {
        RefsOpts { limit: 40, offset: 0, include_self: false }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ImportersOpts {
    #[serde(deserialize_with = "de_lim40")]
    pub limit: usize,
    #[serde(deserialize_with = "de_off")]
    pub offset: usize,
}
impl Default for ImportersOpts {
    fn default() -> Self {
        ImportersOpts { limit: 40, offset: 0 }
    }
}

/// A JS array carrying extra properties (`total`, `offset`, `truncated`, `next`, …) cannot be JSON.
/// It travels as `{"$arr": [...], "$props": {...}}` and the JS layer rebuilds the array.
pub fn with_props(items: Vec<Value>, props: Value) -> Value {
    json!({ "$arr": items, "$props": props })
}

/// The items of an array result, whether plain or in the `with_props` envelope.
pub fn list_items(v: &Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a.clone(),
        Value::Object(o) => o.get("$arr").and_then(Value::as_array).cloned().unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// JS `s.slice(0, n)` on UTF-16 units, without splitting a char.
fn slice_utf16(s: &str, n: usize) -> String {
    let mut units = 0;
    s.chars()
        .take_while(|c| {
            units += c.len_utf16();
            units <= n
        })
        .collect()
}

/// cappedList(items, { cap, moreHint }) → `{ shown, total, rest, more }`
pub fn capped_list(items: &[Value], cap: usize, more_hint: Option<&str>) -> Value {
    let shown: Vec<Value> = items.iter().take(cap).cloned().collect();
    let rest = items.len() - shown.len();
    let more = if rest > 0 {
        let hint = match more_hint {
            Some(h) if !h.is_empty() => format!("; {h}"),
            _ => String::new(),
        };
        format!(" (showing {} of {}{hint})", shown.len(), items.len())
    } else {
        String::new()
    };
    json!({ "shown": shown, "total": items.len(), "rest": rest, "more": more })
}

fn not_indexed(ws: &Workspace, what: &str) -> Error {
    Error::code(
        "OKCODE_NOT_INDEXED",
        format!("workspace \"{}\" is not indexed — {what} needs an index; grep it, outline a file, or read by path, range or heading", ws.id),
    )
}

fn str_of(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str).map(String::from)
}

fn truthy_str(v: Option<&Value>) -> Option<String> {
    str_of(v).filter(|s| !s.is_empty())
}

fn name_of(v: &Value) -> Name {
    Name { local: str_of(v.get("local")), imported: str_of(v.get("imported")) }
}

fn names_of(v: Option<&Value>) -> Option<Vec<Name>> {
    v.and_then(Value::as_array).map(|a| a.iter().map(name_of).collect())
}

fn names_json(n: &Option<Vec<Name>>) -> Option<Value> {
    n.as_ref().map(|l| Value::Array(l.iter().map(|n| json!({ "local": n.local, "imported": n.imported })).collect()))
}

/// The edges crate's input row for a record.
fn file_row(f: &crate::record::FileRecord) -> FileRow {
    FileRow {
        rel: f.rel.clone(),
        lang: Some(f.lang.clone()),
        imports: f
            .imports
            .iter()
            .map(|i| Import {
                from: str_of(i.get("from")),
                line: i.get("line").and_then(|l| l.as_i64().or_else(|| l.as_f64().map(|x| x as i64))),
                kind: truthy_str(i.get("kind")),
                reexport: i.get("reexport").map(truthy).unwrap_or(false),
                names: names_of(i.get("names")),
            })
            .collect(),
        symbols: f
            .symbols
            .iter()
            .map(|s| Symbol {
                kind: str_of(s.get("kind")).unwrap_or_default(),
                name: str_of(s.get("name")).unwrap_or_default(),
                parent: truthy_str(s.get("parent")),
            })
            .collect(),
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|x| x != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// `extract(path, text).exports || []` — what packages.js needs from the analyser.
fn extract_exports(path: &str, text: &str) -> Vec<String> {
    crate::host_analysis::extract(path, Some(text))
        .get("exports")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

fn package_json(p: &Package) -> Value {
    let mut m = Map::new();
    m.insert("name".into(), json!(p.name));
    m.insert("version".into(), json!(p.version));
    m.insert("description".into(), json!(p.description));
    m.insert("main".into(), json!(p.main));
    m.insert("module".into(), json!(p.module));
    m.insert("types".into(), json!(p.types));
    m.insert("exportKeys".into(), json!(p.export_keys));
    m.insert("bin".into(), json!(p.bin));
    m.insert("source".into(), json!(p.source));
    m.insert("surface".into(), json!(p.surface));
    m.insert("surfaceFrom".into(), json!(p.surface_from));
    m.insert("members".into(), json!(p.members));
    if let Some(f) = &p.surface_file {
        m.insert("surfaceFile".into(), json!(f));
    }
    if p.surface_truncated {
        m.insert("surfaceTruncated".into(), json!(true));
    }
    if p.members_truncated {
        m.insert("membersTruncated".into(), json!(true));
    }
    Value::Object(m)
}

/// Positive integer option the way JS reads it: `Number(x) > 0 ? floor : fallback`.
fn pos(n: usize, fallback: usize) -> usize {
    if n > 0 {
        n
    } else {
        fallback
    }
}

/// JS `String(v)` over a JSON value.
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => match n.as_f64() {
            Some(f) if n.is_f64() && f.abs() >= 1e21 => {
                let t = format!("{:e}", f);
                let (m, e) = t.split_once('e').unwrap_or((&t, "0"));
                let e: i32 = e.parse().unwrap_or(0);
                format!("{m}e{}{}", if e < 0 { "-" } else { "+" }, e.abs())
            }
            Some(f) if n.is_f64() && f.fract() == 0.0 && f.abs() < 1e21 => format!("{}", f as i128),
            _ => n.to_string(),
        },
        Value::Array(a) => a.iter().map(|x| if x.is_null() { String::new() } else { js_string(x) }).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

fn paging(total: usize, off: usize, shown: usize) -> (bool, Value) {
    let truncated = off + shown < total;
    (truncated, if truncated { json!(off + shown) } else { Value::Null })
}

impl Workspace {
    /// Run `f` with the edge index current for the present generation. The index is built from a
    /// row snapshot with NO lock held (ensure() reads config files through the JS-backed access
    /// bridge); the edges mutex is only taken briefly to read or install the finished index.
    /// Two racing callers may both build; the newest generation wins.
    fn with_edges<R>(&self, f: impl FnOnce(&EdgeIndex<EdgeAccess>) -> R) -> R {
        let gen = self.read_ix().generation;
        let cached = {
            let slot = self.graph.edges.lock().unwrap_or_else(|e| e.into_inner());
            match &*slot {
                Some((idx, g)) if *g == gen => Some(idx.clone()),
                _ => None,
            }
        };
        let idx = match cached {
            Some(i) => i,
            None => {
                let (rows, gen) = {
                    let ix = self.read_ix();
                    (ix.files.values().map(file_row).collect::<Vec<_>>(), ix.generation)
                };
                let mut fresh = EdgeIndex::new(EdgeAccess(self.access.clone()));
                fresh.ensure(&rows, gen as i64);
                let fresh = std::sync::Arc::new(fresh);
                let mut slot = self.graph.edges.lock().unwrap_or_else(|e| e.into_inner());
                if !matches!(&*slot, Some((_, g)) if *g > gen) {
                    *slot = Some((fresh.clone(), gen));
                }
                fresh
            }
        };
        f(&idx)
    }

    /// mentionsOf(text) → `[{ rel, line }]`, one hit per file (the first occurrence).
    pub fn mentions_of(&self, text: &str) -> Result<Value> {
        Ok(Value::Array(self.mention_hits(text)?.into_iter().map(|(rel, line)| json!({ "rel": rel, "line": line })).collect()))
    }

    fn mention_hits(&self, text: &str) -> Result<Vec<(String, usize)>> {
        let needle = text.to_lowercase();
        if needle.encode_utf16().count() < 2 {
            return Ok(Vec::new());
        }
        // Withheld files stay withheld: an address is a lead even without the bytes.
        let candidates: Vec<String> =
            self.read_ix().files.values().filter(|f| f.indexed && !okcode_analysis::is_secret(&f.rel)).map(|f| f.rel.clone()).collect();
        self.ensure_texts(&candidates)?;
        let ix = self.read_ix();
        let mut out = Vec::new();
        for f in ix.files.values() {
            let Some(content) = &f.content else { continue };
            if !f.indexed || okcode_analysis::is_secret(&f.rel) {
                continue;
            }
            let lower = content.to_lowercase();
            let Some(at) = lower.find(&needle) else { continue };
            out.push((f.rel.clone(), lower[..at].matches('\n').count() + 1));
        }
        Ok(out)
    }

    /// WHY a target did not resolve, in the caller's own terms (explainTarget).
    fn graph_explain_target(&self, target: &str) -> Result<String> {
        let cut = target.rfind([':', '#']);
        if let Some(cut) = cut.filter(|c| target[..*c].chars().count() > 1) {
            let head = &target[..cut];
            let tail = &target[cut + 1..];
            if let Some(rel) = self.file_of(head) {
                let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
                let range_tail = match tail.split_once('-') {
                    Some((a, b)) => digits(a) && digits(b),
                    None => digits(tail),
                };
                return Ok(if range_tail {
                    format!("{rel} IS indexed — \"{target}\" failed on the separator, not the file; address a line range as \"{rel}:{tail}\"")
                } else {
                    format!("{rel} IS indexed, but has no symbol \"{tail}\" — address a line range as \"{rel}:<from>-<to>\", or read {rel} to find the symbol's real name")
                });
            }
        }
        let r = self.resolve(target)?;
        if let Some(reason) = r.get("reason").and_then(Value::as_str) {
            if !reason.is_empty() && r.get("noSuchFile").map(truthy).unwrap_or(false) {
                return Ok(reason.to_string());
            }
            if !reason.is_empty() {
                return Ok(format!("no indexed file matching \"{target}\" — {reason}"));
            }
        }
        Ok(format!("no indexed file matching \"{target}\""))
    }

    fn changed(&self, rels: &[String]) -> Result<bool> {
        Ok(self.result_set_check(rels)?.get("changed").map(truthy).unwrap_or(false))
    }

    // ── api ─────────────────────────────────────────────────────────────

    /// What this project depends on, and what each exposes. Read lazily, kept until `refresh`.
    pub fn packages(&self, o: PackagesOpts) -> Result<Value> {
        if !o.refresh {
            let ix = self.read_ix();
            if !ix.packages.is_empty() {
                return Ok(Value::Array(ix.packages.values().cloned().collect()));
            }
        }
        let scan = scan_packages(&EdgeAccess(self.access.clone()), &extract_exports);
        if let (Some(reason), true) = (&scan.reason, scan.packages.is_empty()) {
            return Ok(json!({ "error": reason }));
        }
        let list: Vec<Value> = scan.packages.iter().map(package_json).collect();
        {
            let mut ix = self.write_ix();
            ix.packages = scan.packages.iter().zip(&list).map(|(p, v)| (p.name.clone(), v.clone())).collect();
            if self.store.is_some() {
                ix.pending_packages.extend(list.iter().cloned());
            }
        }
        if self.store.is_some() {
            self.schedule_flush();
        }
        Ok(Value::Array(list))
    }

    /// One dependency, by name — including one merely installed rather than declared.
    pub fn package(&self, name: &str) -> Result<Value> {
        if let Some(known) = self.read_ix().packages.get(name) {
            return Ok(known.clone());
        }
        let found = read_packages(&EdgeAccess(self.access.clone()), &[name.to_string()], &|_| "transitive".to_string(), &extract_exports);
        let Some(pkg) = found.into_iter().find(|p| p.name == name) else {
            return Ok(json!({ "error": format!("no package \"{name}\" under node_modules") }));
        };
        let v = package_json(&pkg);
        {
            let mut ix = self.write_ix();
            ix.packages.insert(pkg.name.clone(), v.clone());
            if self.store.is_some() {
                ix.pending_packages.push(v.clone());
            }
        }
        if self.store.is_some() {
            self.schedule_flush();
        }
        Ok(v)
    }

    /// ask(query, { limit, profile, path }) — query: a string or `{ text?, vector?, identity? }`.
    /// find() with the semantic eye attached; throws OKCODE_NO_EMBEDDINGS without a profile.
    pub fn ask(&self, query: &Value, o: AskOpts) -> Result<Value> {
        if self.unindexed {
            return Err(not_indexed(self, "searching by meaning"));
        }
        if let Some(Value::String(p)) = &o.path {
            if !p.is_empty() && self.library_path(p).is_some() {
                let np = self.norm_path(p);
                return Err(Error::code(
                    "OKCODE_LIBRARY_NOT_INDEXED",
                    format!("{np} is library code, which is never embedded — searching it by meaning is not supported; grep it (paths: [\"{np}\"]) or look a name up in it (find with path)"),
                ));
            }
        }
        let store = match &self.store {
            Some(s) if s.has_profiles() => s.clone(),
            _ => {
                return Err(Error::code("OKCODE_NO_EMBEDDINGS", format!("workspace \"{}\" has no embedding profile — ask() is unavailable", self.id)));
            }
        };
        let q = as_query(query)?;
        let limit = o.limit;
        // A FULL set from the semantic eye, so a phrase missed by both lexical eyes is not dropped.
        let semantic_v = store.ask(&q, &AskOptions { profile: o.profile.clone(), limit: limit.max(8), text: false })?;
        let semantic = list_items(&semantic_v);
        let lexical: Vec<Value> = match &q.text {
            Some(t) if !t.is_empty() => list_items(&self.find(&json!(t), FindOpts { limit, ..FindOpts::default() })?),
            _ => {
                self.ensure_open()?;
                Vec::new()
            }
        };
        let key_of = |h: &Value| {
            let p = truthy_str(h.get("path")).or_else(|| truthy_str(h.get("name"))).unwrap_or_default();
            format!("{}:{}", h.get("file").and_then(Value::as_str).unwrap_or(""), p)
        };
        let mut seen: HashSet<String> = lexical.iter().map(key_of).collect();
        // The chunk only says WHERE; every line number and character below comes from the verified record.
        let rels: Vec<String> = semantic.iter().filter_map(|s| str_of(s.get("file"))).collect();
        self.verify(&rels, false)?;
        let mut out = lexical;
        for s in &semantic {
            let Some(rel) = str_of(s.get("file")) else { continue };
            let Some(f) = self.read_ix().files.get(&rel).cloned() else { continue };
            let Some(content) = f.content.clone() else { continue };
            let score = s.get("score").and_then(Value::as_f64).unwrap_or(0.0);
            // start is a UTF-16 offset (JS string index); convert to a byte offset.
            let start = s.get("start").and_then(Value::as_f64).map(|x| x.max(0.0) as usize).unwrap_or(0);
            let off = utf16_to_byte(&content, start);
            let probe_end = off + slice_utf16(&content[off..], 400).len();
            let probe = &content[off..probe_end];
            let lead_len = probe.len() - probe.trim_start_matches(js_ws).len();
            let lead = &probe[..lead_len];
            let skip = lead.rfind('\n').map(|i| i + 1).unwrap_or(0);
            let at = off + skip;
            let line = content[..at].matches('\n').count() + 1;
            let owner = owner_of(&f, line as i64).cloned();
            let win = self.window_at(&f, line as i64, owner.as_ref(), Some(self.opts.window_lines));
            let owner_path = owner.as_ref().map(|ow| truthy_str(ow.get("path")).or_else(|| str_of(ow.get("name"))).unwrap_or_default());
            let key = format!("{}:{}", f.rel, owner_path.clone().unwrap_or_else(|| f.rel.clone()));
            if !seen.insert(key) {
                continue;
            }
            let num = |v: &Value, k: &str| v.get(k).and_then(Value::as_i64).unwrap_or(0);
            let (lines, span) = match (&owner, &win) {
                (Some(ow), _) => (format!("{}-{}", num(ow, "lineStart"), num(ow, "lineEnd")), num(ow, "lineEnd") - num(ow, "lineStart") + 1),
                (None, Some(w)) => (format!("{}-{}", num(w, "lineStart"), num(w, "lineEnd")), num(w, "lineEnd") - num(w, "lineStart") + 1),
                (None, None) => (format!("{line}-{line}"), 1),
            };
            let src_line = content.split('\n').nth(line - 1).unwrap_or("").trim_matches(js_ws);
            out.push(json!({
                "name": owner.as_ref().map(|ow| ow.get("name").cloned().unwrap_or(Value::Null)).unwrap_or_else(|| json!(f.rel)),
                "kind": owner.as_ref().map(|ow| ow.get("kind").cloned().unwrap_or(Value::Null)).unwrap_or_else(|| json!(f.lang)),
                "file": f.rel,
                "path": owner_path.clone().unwrap_or_else(|| f.rel.clone()),
                "parent": owner.as_ref().and_then(|ow| truthy_str(ow.get("parent"))),
                "lines": lines,
                "span": span,
                "signature": owner.as_ref().and_then(|ow| truthy_str(ow.get("signature"))),
                "doc": owner.as_ref().and_then(|ow| truthy_str(ow.get("doc"))).map(|d| slice_utf16(&d, 160)).filter(|d| !d.is_empty()),
                "exported": owner.as_ref().and_then(|ow| ow.get("exported")).map(truthy).unwrap_or(false),
                "at": f.hash,
                "score": 20 + (20.0 * score + 0.5).floor() as i64,
                "via": "vector",
                "relevance": score,
                "quote": slice_utf16(&format!("{}:{}  {}", f.rel, line, src_line), 200),
                "line": line,
                "window": win,
            }));
        }
        // Lexical certainty first, then semantic proximity.
        let rank = |v: &Value, k: &str| v.get(k).and_then(Value::as_f64).unwrap_or(0.0);
        out.sort_by(|a, b| {
            rank(b, "score").partial_cmp(&rank(a, "score")).unwrap_or(std::cmp::Ordering::Equal).then(
                rank(b, "relevance").partial_cmp(&rank(a, "relevance")).unwrap_or(std::cmp::Ordering::Equal),
            )
        });
        let total = out.len();
        out.truncate(limit);
        let mut props = Map::new();
        props.insert("topK".into(), json!(true));
        props.insert("truncated".into(), json!(total > limit));
        // fts: 'symbols' drops the content index: say so, not silently.
        if !store.content_fts() {
            props.insert("contentEyeSkipped".into(), json!(true));
        }
        Ok(with_props(out, Value::Object(props)))
    }

    /// Is this EXACT text anywhere in the workspace? Paged like refs().
    pub fn mentions(&self, text: &str, o: MentionsOpts) -> Result<Value> {
        self.mentions_inner(text, &o, false)
    }

    fn mentions_inner(&self, text: &str, o: &MentionsOpts, retried: bool) -> Result<Value> {
        if self.unindexed {
            return Err(not_indexed(self, "mentions()"));
        }
        self.ensure_open()?;
        let all = self.mention_hits(text)?;
        let off = o.offset;
        let n = pos(o.limit, 5);
        let hits: Vec<(String, usize)> = all.iter().skip(off).take(n).cloned().collect();
        if !retried && !hits.is_empty() {
            let rels: Vec<String> = hits.iter().map(|h| h.0.clone()).collect();
            if self.changed(&rels)? {
                return self.mentions_inner(text, o, true);
            }
        }
        let (truncated, next) = paging(all.len(), off, hits.len());
        let items = hits.into_iter().map(|(rel, line)| json!({ "rel": rel, "line": line })).collect();
        Ok(with_props(items, json!({ "total": all.len(), "offset": off, "truncated": truncated, "next": next })))
    }

    /// "Where is this USED?" — every text hit, attributed to its symbol, ranked and labelled
    /// (`local` / `imported` / `textual`) against the resolved edges.
    pub fn refs(&self, query: &Value, o: RefsOpts) -> Result<Value> {
        self.refs_inner(query, &o, false)
    }

    fn refs_inner(&self, query: &Value, o: &RefsOpts, retried: bool) -> Result<Value> {
        if self.unindexed {
            return Err(not_indexed(self, "finding uses"));
        }
        self.ensure_open()?;
        // isSecret as well as the flag: a store written under older rules can still hold `indexed: true`.
        let want: Vec<String> =
            self.read_ix().files.values().filter(|f| f.indexed && !okcode_analysis::is_secret(&f.rel)).map(|f| f.rel.clone()).collect();
        self.ensure_texts(&want)?;
        let q = match query {
            Value::String(s) => s.clone(),
            other => js_string(other),
        };
        // The ranking needs the WHOLE universe before it can sort; only a pathological query needs a
        // floor under the raw scan, and that floor is reported (`capped`).
        const RAW_CAP: usize = 50000;
        struct Hit {
            rel: String,
            ln: usize,
            symbol: String,
            kind: Option<Value>,
            text: String,
        }
        let mut raw: Vec<Hit> = Vec::new();
        let mut scan_capped = false;
        {
            let ix = self.read_ix();
            'outer: for f in ix.files.values() {
                if !f.indexed || okcode_analysis::is_secret(&f.rel) {
                    continue;
                }
                let Some(content) = &f.content else { continue };
                for (i, line) in content.split('\n').enumerate() {
                    if !line.contains(q.as_str()) {
                        continue;
                    }
                    let ln = i + 1;
                    let owner = owner_of(f, ln as i64);
                    // The declaration itself is not a use of itself.
                    if !o.include_self {
                        if let Some(ow) = owner {
                            if ow.get("name").and_then(Value::as_str) == Some(q.as_str()) && ow.get("lineStart").and_then(Value::as_i64) == Some(ln as i64) {
                                continue;
                            }
                        }
                    }
                    raw.push(Hit {
                        rel: f.rel.clone(),
                        ln,
                        symbol: owner
                            .map(|ow| truthy_str(ow.get("path")).or_else(|| str_of(ow.get("name"))).unwrap_or_default())
                            .unwrap_or_else(|| "(top level)".to_string()),
                        kind: owner.map(|ow| ow.get("kind").cloned()).unwrap_or(Some(Value::Null)),
                        text: slice_utf16(line.trim_matches(js_ws), 160),
                    });
                    if raw.len() >= RAW_CAP {
                        scan_capped = true;
                        break 'outer;
                    }
                }
            }
        }
        let qual_re = {
            let src = format!(r"(^|[^A-Za-z0-9_.])([A-Za-z_$][A-Za-z0-9_$]*)\.{}(?-u:\b)", regex::escape(&q));
            regex::Regex::new(&src).ok()
        };
        let mut out: Vec<Value> = Vec::with_capacity(raw.len());
        // labels per file are the expensive part: memoise the file-level facts.
        self.with_edges(|edges| {
            let ix = self.read_ix();
            let file_defines = |rel: &str, name: &str| ix.files.get(rel).map(|t| t.symbols.iter().any(|s| s.get("name").and_then(Value::as_str) == Some(name))).unwrap_or(false);
            // `imported`: some resolved edge out of this file binds `q` to a name the TARGET defines;
            // one further re-export hop is followed on the target.
            let imported_from = |rel: &str| -> bool {
                for e in edges.imports_of(rel) {
                    if e.to.is_empty() {
                        continue;
                    }
                    let pair = e.names.iter().flatten().find(|n| n.local.as_deref() == Some(q.as_str()));
                    let name: Option<String> = match pair {
                        Some(p) => p.imported.clone(),
                        None if e.name.as_deref() == Some(q.as_str()) => Some(q.clone()),
                        None => None,
                    };
                    let Some(name) = name else { continue };
                    if file_defines(&e.to, &name) {
                        return true;
                    }
                    for e2 in edges.imports_of(&e.to) {
                        if !e2.reexport || e2.to.is_empty() {
                            continue;
                        }
                        let pair2 = e2.names.iter().flatten().find(|n| n.imported.as_deref() == Some(name.as_str()));
                        if pair2.is_some() || e2.names.is_none() {
                            let target = pair2.and_then(|p| p.imported.clone()).unwrap_or_else(|| name.clone());
                            if file_defines(&e2.to, &target) {
                                return true;
                            }
                        }
                    }
                }
                false
            };
            // A Go `pkg.Fn()` / Python `mod.fn()` qualifies its call with the package/module's bound name.
            let qualified_call = |rel: &str, lang: &str, text: &str| -> bool {
                if lang != "go" && lang != "python" {
                    return false;
                }
                let Some(m) = qual_re.as_ref().and_then(|re| re.captures(text)) else { return false };
                let alias = &m[2];
                for e in edges.imports_of(rel) {
                    if e.to.is_empty() {
                        continue;
                    }
                    let first_local = e.names.as_ref().and_then(|n| n.first()).and_then(|n| n.local.clone()).filter(|s| !s.is_empty());
                    let src = e.from.clone().unwrap_or_default();
                    if lang == "go" {
                        if e.kind.as_deref() != Some("import") {
                            continue;
                        }
                        let bound = first_local.unwrap_or_else(|| src.rsplit('/').next().unwrap_or("").to_string());
                        if bound == alias && file_defines(&e.to, &q) {
                            return true;
                        }
                    } else {
                        if e.kind.as_deref() != Some("modimport") {
                            continue;
                        }
                        let bound = first_local.or_else(|| if src.contains('.') { None } else { Some(src.clone()) });
                        if let Some(b) = bound.filter(|b| !b.is_empty()) {
                            if b == alias && file_defines(&e.to, &q) {
                                return true;
                            }
                        }
                    }
                }
                false
            };
            // References are either freshly parsed (every kind) or loaded back already trimmed to calls.
            let call_at = |extra: &Map<String, Value>, ln: usize| {
                extra.get("references").and_then(Value::as_array).map(|rs| {
                    rs.iter().any(|r| {
                        r.get("line").and_then(Value::as_i64) == Some(ln as i64)
                            && r.get("name").and_then(Value::as_str) == Some(q.as_str())
                            && (!r.get("kind").map(truthy).unwrap_or(false) || r.get("kind").and_then(Value::as_str) == Some("call"))
                    })
                }).unwrap_or(false)
            };
            for h in &raw {
                let Some(f) = ix.files.get(&h.rel) else { continue };
                let label = if f.symbols.iter().any(|s| s.get("name").and_then(Value::as_str) == Some(q.as_str())) {
                    "local"
                } else if imported_from(&f.rel) || qualified_call(&f.rel, &f.lang, &h.text) {
                    "imported"
                } else {
                    "textual"
                };
                let mut m = Map::new();
                m.insert("file".into(), json!(h.rel));
                m.insert("line".into(), json!(h.ln));
                m.insert("symbol".into(), json!(h.symbol));
                if let Some(k) = &h.kind {
                    m.insert("kind".into(), k.clone());
                }
                m.insert("text".into(), json!(h.text));
                m.insert("confidence".into(), json!(label));
                m.insert("label".into(), json!(label));
                m.insert("call".into(), json!(call_at(&f.extra, h.ln)));
                m.insert("at".into(), json!(f.hash));
                out.push(Value::Object(m));
            }
        });
        // Definitions' own files and importers first: a STABLE sort, ties keep file/line order.
        let rank = |v: &Value| match v.get("label").and_then(Value::as_str) {
            Some("local") => 0,
            Some("imported") => 1,
            _ => 2,
        };
        out.sort_by_key(rank);
        let off = o.offset;
        let n = pos(o.limit, 40);
        let page: Vec<Value> = out.iter().skip(off).take(n).cloned().collect();
        if !retried && !page.is_empty() {
            let rels: Vec<String> = page.iter().filter_map(|h| str_of(h.get("file"))).collect();
            if self.changed(&rels)? {
                return self.refs_inner(query, o, true);
            }
        }
        // `total` is exact unless the raw scan hit its cap: then it is "at least", never a fake total.
        let truncated = scan_capped || off + page.len() < out.len();
        let mut props = Map::new();
        props.insert("offset".into(), json!(off));
        props.insert("capped".into(), json!(scan_capped));
        if scan_capped {
            props.insert("total".into(), Value::Null);
            props.insert("atLeast".into(), json!(out.len()));
        } else {
            props.insert("total".into(), json!(out.len()));
        }
        props.insert("truncated".into(), json!(truncated));
        props.insert("next".into(), if truncated { json!(off + page.len()) } else { Value::Null });
        Ok(with_props(page, Value::Object(props)))
    }

    /// The reverse of `imports`: every file with a RESOLVED edge into `file` (one re-export hop;
    /// for okjs a component's tag counts).
    pub fn importers(&self, file: &str, o: ImportersOpts) -> Result<Value> {
        if self.unindexed {
            return Err(not_indexed(self, "finding importers"));
        }
        self.ensure_open()?;
        let Some(rel) = self.file_of(file) else {
            return Ok(json!({ "ok": false, "reason": self.graph_explain_target(file)? }));
        };
        let hits: Vec<Value> = self.with_edges(|edges| {
            edges
                .importers_of(&rel)
                .iter()
                .map(|e| {
                    let mut m = Map::new();
                    m.insert("file".into(), json!(e.file));
                    m.insert("line".into(), json!(e.line));
                    m.insert("kind".into(), json!(e.kind));
                    if let Some(n) = names_json(&e.names) {
                        m.insert("names".into(), n);
                    }
                    m.insert("via".into(), json!(e.via.clone().filter(|s| !s.is_empty())));
                    Value::Object(m)
                })
                .collect()
        });
        let off = o.offset;
        let n = pos(o.limit, 40);
        let page: Vec<Value> = hits.iter().skip(off).take(n).cloned().collect();
        let (truncated, next) = paging(hits.len(), off, page.len());
        Ok(json!({
            "ok": true,
            "file": rel,
            "total": hits.len(),
            "offset": off,
            "truncated": truncated,
            "next": next,
            "importers": page,
        }))
    }
}

/// JS string index (UTF-16 units) → byte offset, clamped to the end.
fn utf16_to_byte(s: &str, units: usize) -> usize {
    let mut u = 0;
    for (i, c) in s.char_indices() {
        if u >= units {
            return i;
        }
        u += c.len_utf16();
    }
    s.len()
}
