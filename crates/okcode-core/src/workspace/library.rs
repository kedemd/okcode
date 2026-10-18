//! Library code (node_modules, vendor, dist… — any skip name) and the unindexed workspace.
//!
//! normPath, libraryPath, badPath, listLibrary, libraryFile, headingNames, scanLibrary,
//! readLibrary, readSection, sectionsOutline, outlineLibrary; api isLibraryPath. Never indexed, never stored: listed, read and parsed on demand, the
//! parsed symbols of the last `libraryScans` subtrees kept in memory (LRU, `LibraryState`).
//! The verbs in lookup/grep call these when `library_path(p)` says so (or `self.unindexed`).
use super::state::sha1_upper;
use super::Workspace;
use crate::error::{Error, Result};
use indexmap::IndexMap;
use okcode_analysis::access::{ListOpts, PRUNE_DOT_DIRS};
use okcode_analysis::chunk::{chunk_code, regions as text_regions, section_of, LOG_LINES, REGION_LINES};
use okcode_analysis::text::text::{decode, from_u16, u16s};
use crate::host_analysis::extract;
use okcode_analysis::{is_secret, is_textual};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

/// One parsed file of a scanned subtree (no text is kept).
#[derive(Clone, Debug)]
pub struct ScanEntry {
    pub size: u64,
    pub mtime: f64,
    pub lines: u64,
    pub lang: Value,
    pub symbols: Vec<Value>,
}

/// subtree -> rel -> entry, least recently used first.
type Scans = IndexMap<String, IndexMap<String, ScanEntry>>;

#[derive(Default)]
pub struct LibraryState {
    scans: Mutex<Scans>,
}

#[derive(Clone, Debug, Default)]
pub struct ReadLibraryOpts {
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub symbol: Option<String>,
    pub max_lines: Option<usize>,
}

/// libraryPath(p): null | { rel } | { error }.
#[derive(Clone, Debug, PartialEq)]
pub enum LibPath {
    No,
    Rel(String),
    Error(String),
}

/// libraryFile(rel): `{ ok: true, rel, text, at, size }` or the refusal value.
enum LibFile {
    Ok { rel: String, text: String, at: String, size: usize },
    Fail(Value),
}

impl LibFile {
    fn is_dir(v: &Value) -> bool {
        v.get("isDir").and_then(Value::as_bool).unwrap_or(false)
    }
}

fn badpath(msg: String) -> Error {
    Error::code("OKCODE_BAD_PATH", msg)
}

/// cappedList(items, { cap: MENTIONS_REASON_CAP }) -> (shown, more)
fn capped<T>(items: &[T]) -> (&[T], String) {
    let cap = super::MENTIONS_REASON_CAP;
    let shown = &items[..items.len().min(cap)];
    let rest = items.len() - shown.len();
    let more = if rest > 0 { format!(" (showing {} of {})", shown.len(), items.len()) } else { String::new() };
    (shown, more)
}

fn truthy_str(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn jint(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(Value::as_f64).unwrap_or(0.0) as i64
}

/// symbol.path || symbol.name
fn sym_path(x: &Value) -> String {
    truthy_str(x.get("path")).or_else(|| truthy_str(x.get("name"))).unwrap_or_default()
}

fn sym_name(x: &Value) -> String {
    x.get("name").and_then(Value::as_str).unwrap_or("").to_string()
}

fn sym_parent(x: &Value) -> Value {
    match x.get("parent") {
        Some(Value::String(s)) if !s.is_empty() => json!(s),
        Some(v) if !v.is_null() && v != &json!(false) && v != &json!(0) && v != &json!("") => v.clone(),
        _ => Value::Null,
    }
}

/// The exact bytes a symbol addresses, by its recorded (UTF-16) offsets; else its line range.
fn symbol_text(src: &str, symbol: &Value) -> String {
    let int = |k: &str| symbol.get(k).and_then(Value::as_f64).filter(|f| f.fract() == 0.0);
    if let (Some(s), Some(e)) = (int("start"), int("end")) {
        let u = u16s(src);
        let s = (s.max(0.0) as usize).min(u.len());
        let e = (e.max(0.0) as usize).min(u.len());
        return if s < e { from_u16(&u[s..e]) } else { String::new() };
    }
    let a = (jint(symbol, "lineStart").max(1) - 1) as usize;
    let b = jint(symbol, "lineEnd").max(0) as usize;
    let lines: Vec<&str> = src.split('\n').collect();
    if a >= b.min(lines.len()) {
        return String::new();
    }
    lines[a..b.min(lines.len())].join("\n")
}

fn sorted_by_rel<T>(mut v: Vec<(String, T)>) -> Vec<(String, T)> {
    v.sort_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()));
    v
}

/// 1-based line of a UTF-16 offset: text.slice(0, n).split('\n').length
fn line_before(u: &[u16], n: usize) -> usize {
    u[..n.min(u.len())].iter().filter(|c| **c == 10).count() + 1
}

impl Workspace {
    /// normPath(p): `\` → `/`, strip the workspace root prefix, `./` and surrounding `/`
    pub fn norm_path(&self, p: &str) -> String {
        let mut s = p.replace('\\', "/");
        if let Some(root) = self.access.root() {
            let root = root.replace('\\', "/");
            let root = root.strip_suffix('/').unwrap_or(&root).to_string();
            if !root.is_empty() {
                // JS: s.toLowerCase().startsWith(root.toLowerCase() + '/') then s.slice(root.length + 1);
                // the cut is root.length UTF-16 units into the ORIGINAL string.
                let n = root.encode_utf16().count() + 1;
                let su = u16s(&s);
                if su.len() >= n && from_u16(&su[..n]).to_lowercase() == format!("{}/", root.to_lowercase()) {
                    s = from_u16(&su[n..]);
                }
            }
        }
        if s == "." {
            s.clear();
        } else if let Some(r) = s.strip_prefix("./") {
            s = r.to_string();
        }
        s.trim_matches('/').to_string()
    }

    fn checked(&self, raw: &str) -> LibPath {
        match self.access.check_path(raw) {
            Ok(rel) => LibPath::Rel(rel),
            Err(e) => LibPath::Error(e),
        }
    }

    /// libraryPath(p) in full. Never calls the facade beyond its lexical `check_path`
    /// Callers MUST NOT hold an `ix` read/write guard when calling this (it takes `read_ix()`;
    /// a pending writer blocks new readers on std RwLock, so that could deadlock).
    ///
    /// (isLibraryPath runs on the JS thread).
    pub fn library_path_ex(&self, p: &str) -> LibPath {
        let raw = self.norm_path(p);
        if self.unindexed {
            if raw.is_empty() {
                return LibPath::Rel(String::new());
            }
            return self.checked(&raw);
        }
        if raw.is_empty() {
            return LibPath::No;
        }
        let skip: HashSet<&str> = match &self.opts.skip {
            Some(v) => v.iter().map(String::as_str).collect(),
            None => okcode_analysis::access::DEFAULT_SKIP.iter().copied().collect(),
        };
        let segs: Vec<&str> = raw.split('/').collect();
        if !segs.iter().any(|x| skip.contains(x) && !x.starts_with('.')) {
            return LibPath::No;
        }
        // A skip name is pruned as a DIRECTORY only: a project file called `build` is indexed.
        if self.read_ix().files.contains_key(&raw) {
            return LibPath::No;
        }
        if !segs.contains(&"..") && segs.iter().any(|x| x.starts_with('.') && *x != ".") {
            return LibPath::No;
        }
        self.checked(&raw)
    }

    /// libraryPath(p) → the cleaned path when it names library code (or the workspace is
    /// unindexed), else None. A path the facade refuses is still library-shaped: it comes back
    /// normalised, and `read_library` / `outline_library` / `library_file` then refuse it with
    /// the facade's reason. The lossy form drops the facade's `{ error }`: callers that must branch
    /// on it (grepAll, outline, read) have to use `library_path_ex`.
    pub fn library_path(&self, p: &str) -> Option<String> {
        match self.library_path_ex(p) {
            LibPath::No => None,
            LibPath::Rel(r) => Some(r),
            LibPath::Error(_) => Some(self.norm_path(p)),
        }
    }

    fn textual_by_path(&self, p: &str, size: u64) -> bool {
        is_textual(p, Some(0)) && (size == 0 || size <= self.text_max)
    }

    fn own_transient(p: &str) -> bool {
        p.split('/').any(|s| s.starts_with(".okcode-tmp-")) || p.ends_with(".okcode-lock")
    }

    fn dot_pruned(p: &str) -> bool {
        let segs: Vec<&str> = p.split('/').collect();
        segs.len() > 1 && segs[..segs.len() - 1].iter().any(|s| PRUNE_DOT_DIRS.contains(s))
    }

    /// A refusal for a path the facade will not check, else None.
    fn path_refusal(&self, rel: &str) -> Option<Value> {
        if rel.is_empty() {
            return None;
        }
        self.access.check_path(rel).err().map(|e| json!({ "ok": false, "reason": e }))
    }

    /// listLibrary(roots, cap) → `{ rows: [{ rel, size, mtime }], truncated, roots, missing }`.
    /// Below a named subtree only a nested node_modules is pruned.
    pub fn list_library(&self, roots: &[String], cap: usize) -> Result<Value> {
        let mut uniq: Vec<&String> = Vec::new();
        for r in roots {
            if !uniq.contains(&r) {
                uniq.push(r);
            }
        }
        let inside = |p: &str, root: &str| root.is_empty() || p == root || p.starts_with(&format!("{root}/"));
        let tops: Vec<String> = if uniq.iter().any(|r| r.is_empty()) {
            vec![String::new()]
        } else {
            uniq.iter().filter(|r| !uniq.iter().any(|o| *o != **r && r.starts_with(&format!("{o}/")))).map(|r| (*r).clone()).collect()
        };
        let mut seen: HashMap<String, (u64, f64)> = HashMap::new();
        let mut truncated = false;
        let mut listed_n = 0usize;
        let mut missing: Vec<String> = Vec::new();
        for root in &tops {
            if listed_n >= cap {
                truncated = true;
                break;
            }
            let left = cap - listed_n;
            let o = ListOpts { skip: vec!["node_modules".into()], under: (!root.is_empty()).then(|| root.clone()), limit: left };
            let listed = self.access.list(&o).map_err(|e| badpath(format!("{root}: {e}")))?;
            if listed.truncated {
                truncated = true;
            }
            listed_n += listed.rows.len();
            if !root.is_empty() && !listed.rows.iter().any(|r| inside(&r.path, root)) {
                missing.push(root.clone());
            }
            for r in &listed.rows {
                // A facade that ignores `under` hands back the whole tree: only the subtree counts.
                if !inside(&r.path, root) {
                    continue;
                }
                if Self::dot_pruned(&r.path) || Self::own_transient(&r.path) {
                    continue;
                }
                if !self.textual_by_path(&r.path, r.size) || is_secret(&r.path) {
                    continue;
                }
                seen.insert(r.path.clone(), (r.size, r.mtime));
            }
        }
        let rows: Vec<Value> = sorted_by_rel(seen.into_iter().collect())
            .into_iter()
            .map(|(rel, (size, mtime))| json!({ "rel": rel, "size": size, "mtime": mtime }))
            .collect();
        Ok(json!({ "rows": rows, "truncated": truncated, "roots": tops, "missing": missing }))
    }

    /// readMany for library files: bounded batches, a refused batch retried file by file
    /// (the failure of a single file is ignored).
    fn read_many_lib(&self, rels: &[String], sizes: &HashMap<String, u64>) -> Result<HashMap<String, Vec<u8>>> {
        let mut out = HashMap::new();
        let mut batch: Vec<String> = Vec::new();
        let mut bytes = 0u64;
        for rel in rels {
            batch.push(rel.clone());
            bytes += sizes.get(rel).copied().unwrap_or(0);
            if batch.len() >= self.opts.read_batch_files || bytes >= self.opts.read_batch_bytes as u64 {
                self.read_batch_lib(std::mem::take(&mut batch), &mut out)?;
                bytes = 0;
            }
        }
        self.read_batch_lib(batch, &mut out)?;
        Ok(out)
    }

    fn read_batch_lib(&self, cur: Vec<String>, out: &mut HashMap<String, Vec<u8>>) -> Result<()> {
        if cur.is_empty() {
            return Ok(());
        }
        match self.access.read(&cur) {
            Ok(got) => out.extend(got),
            Err(err) => {
                let mut failed = 0;
                let mut got_all = Vec::new();
                for r in &cur {
                    match self.access.read(std::slice::from_ref(r)) {
                        Ok(g) => got_all.extend(g),
                        Err(_) => failed += 1,
                    }
                }
                if failed == cur.len() {
                    return Err(Error::msg(err.to_string()));
                }
                out.extend(got_all);
            }
        }
        Ok(())
    }

    fn lib_file(&self, rel: &str) -> Result<LibFile> {
        if rel.is_empty() {
            return Ok(LibFile::Fail(json!({
                "ok": false, "isDir": true,
                "reason": "that is the whole workspace — outline it for its files, or grep it"
            })));
        }
        let o = ListOpts { skip: vec![], under: Some(rel.to_string()), limit: 1 };
        let listed = match self.access.list(&o) {
            Ok(l) => l,
            Err(e) => return Ok(LibFile::Fail(json!({ "ok": false, "reason": e }))),
        };
        let row = match listed.rows.iter().find(|r| r.path == rel) {
            Some(r) => r,
            None => {
                return Ok(LibFile::Fail(if !listed.rows.is_empty() {
                    json!({
                        "ok": false, "isDir": true,
                        "reason": format!("{rel} is a directory — outline it for its files and symbols, or grep with paths: [\"{rel}\"]")
                    })
                } else {
                    json!({
                        "ok": false,
                        "reason": format!("there is no file \"{rel}\" (library code is read directly from disk — check the path)")
                    })
                }))
            }
        };
        if is_secret(rel) {
            return Ok(LibFile::Fail(json!({ "ok": false, "reason": format!("{rel} may hold credentials — never read") })));
        }
        if !self.textual_by_path(rel, row.size) {
            return Ok(LibFile::Fail(json!({ "ok": false, "reason": format!("{rel} is not readable as text (binary or oversized)") })));
        }
        let got = match self.access.read(&[rel.to_string()]) {
            Ok(g) => g,
            Err(e) => return Err(Error::msg(e.to_string())),
        };
        Ok(match got.into_iter().find(|(p, _)| p == rel) {
            None => LibFile::Fail(json!({ "ok": false, "reason": format!("{rel} could not be read") })),
            Some((_, buf)) => LibFile::Ok { rel: rel.to_string(), text: decode(&buf).0, at: sha1_upper(&buf), size: buf.len() },
        })
    }

    /// libraryFile(rel) → `{ ok, rel, text, at, size }` | `{ ok: false, reason[, isDir] }`
    pub fn library_file(&self, rel: &str) -> Result<Value> {
        if let Some(r) = self.path_refusal(rel) {
            return Ok(r);
        }
        Ok(match self.lib_file(rel)? {
            LibFile::Ok { rel, text, at, size } => json!({ "ok": true, "rel": rel, "text": text, "at": at, "size": size }),
            LibFile::Fail(v) => v,
        })
    }

    /// headingNames(rel, text): the headings of a document as symbols.
    fn heading_names(rel: &str, text: &str) -> Vec<Value> {
        match text_regions(rel, text, REGION_LINES as i64, LOG_LINES as i64) {
            Some(Value::Array(a)) => a
                .into_iter()
                .filter(|g| g["kind"] == "heading")
                .map(|g| json!({ "name": g["title"], "path": g["title"], "kind": "heading", "lineStart": g["lineStart"], "lineEnd": g["lineEnd"] }))
                .collect(),
            _ => vec![],
        }
    }

    /// scanLibrary(root) → `{ root, files: [{ rel, size, mtime, lines, lang, symbols }], truncated, missing }`.
    /// The parsed subtree is kept in memory (the last `libraryScans` subtrees); a rescan re-lists
    /// and re-parses only files whose size or mtime moved.
    pub fn scan_library(&self, root: &str) -> Result<Value> {
        let listed = self.list_library(&[root.to_string()], self.opts.library_parse_files.max(1))?;
        let rows = listed["rows"].as_array().cloned().unwrap_or_default();
        // take the prior scan out (it is re-inserted most-recent below); the lock is never held
        // across a facade call
        let prior = self.library.scans.lock().unwrap_or_else(|e| e.into_inner()).shift_remove(root).unwrap_or_default();
        let mut next: IndexMap<String, ScanEntry> = IndexMap::new();
        let mut need: Vec<(String, u64, f64)> = Vec::new();
        for r in &rows {
            let rel = r["rel"].as_str().unwrap_or("").to_string();
            let size = r["size"].as_u64().unwrap_or(0);
            let mtime = r["mtime"].as_f64().unwrap_or(0.0);
            match prior.get(&rel) {
                Some(p) if p.size == size && p.mtime == mtime => {
                    next.insert(rel, p.clone());
                }
                _ => need.push((rel, size, mtime)),
            }
        }
        if !need.is_empty() {
            let sizes: HashMap<String, u64> = need.iter().map(|(r, s, _)| (r.clone(), *s)).collect();
            let rels: Vec<String> = need.iter().map(|(r, _, _)| r.clone()).collect();
            let got = self.read_many_lib(&rels, &sizes)?;
            for (rel, size, mtime) in need {
                let Some(buf) = got.get(&rel) else { continue };
                let text = decode(buf).0;
                let x = extract(&rel, Some(&text));
                let mut symbols = x.get("symbols").and_then(Value::as_array).cloned().unwrap_or_default();
                if symbols.is_empty() && self.unindexed {
                    symbols = Self::heading_names(&rel, &text);
                }
                next.insert(
                    rel,
                    ScanEntry {
                        size,
                        mtime,
                        lines: x.get("lines").and_then(Value::as_f64).unwrap_or(0.0) as u64,
                        lang: x.get("lang").cloned().unwrap_or(Value::Null),
                        symbols,
                    },
                );
            }
        }
        {
            let mut scans = self.library.scans.lock().unwrap_or_else(|e| e.into_inner());
            scans.insert(root.to_string(), next.clone());
            while scans.len() > self.opts.library_scans.max(1) {
                scans.shift_remove_index(0);
            }
        }
        let files: Vec<Value> = sorted_by_rel(next.into_iter().collect())
            .into_iter()
            .map(|(rel, e)| json!({ "rel": rel, "size": e.size, "mtime": e.mtime, "lines": e.lines, "lang": e.lang, "symbols": e.symbols }))
            .collect();
        let missing = listed["missing"].as_array().map_or(false, |m| !m.is_empty());
        Ok(json!({ "root": root, "files": files, "truncated": listed["truncated"], "missing": missing }))
    }

    /// readLibrary(rel, { from, to, symbol, maxLines }): the whole file (capped by lines), a range,
    /// or `file#symbol`. Result shapes of the indexed reads plus `library: true`; nothing recorded.
    pub fn read_library(&self, rel: &str, o: ReadLibraryOpts) -> Result<Value> {
        if let Some(r) = self.path_refusal(rel) {
            return Ok(r);
        }
        let (text, at) = match self.lib_file(rel)? {
            LibFile::Ok { text, at, .. } => (text, at),
            LibFile::Fail(v) => return Ok(v),
        };
        let max_lines = o.max_lines.unwrap_or(self.opts.file_read_lines);
        let lines: Vec<&str> = text.split('\n').collect();
        if let Some(symbol) = o.symbol.as_deref().filter(|s| !s.is_empty()) {
            let syms: Vec<Value> = extract(rel, Some(&text)).get("symbols").and_then(Value::as_array).cloned().unwrap_or_default();
            let mut hits: Vec<&Value> = syms.iter().filter(|x| sym_path(x) == symbol).collect();
            if hits.is_empty() {
                hits = syms.iter().filter(|x| sym_name(x) == symbol).collect();
            }
            if hits.is_empty() {
                // Not a symbol: perhaps a heading (`README.md#Install`).
                let sec = self.read_section(rel, &text, Some(&at), symbol, json!({ "library": true }));
                if !sec.is_null() {
                    return Ok(sec);
                }
                return Ok(json!({
                    "ok": false,
                    "reason": format!("{rel} has no symbol or heading \"{symbol}\" — outline it for its names, or read a range \"{rel}:<from>-<to>\"")
                }));
            }
            if hits.len() > 1 {
                let (shown, more) = capped(&hits);
                return Ok(json!({
                    "ok": false,
                    "ambiguous": true,
                    "reason": format!("\"{symbol}\" is defined in {} places in {rel}{more} — read one by range", hits.len()),
                    "candidates": shown.iter().map(|h| format!("{rel}:{}-{}", h["lineStart"], h["lineEnd"])).collect::<Vec<_>>(),
                }));
            }
            let h = hits[0];
            return Ok(json!({
                "ok": true,
                "library": true,
                "name": h.get("name").cloned().unwrap_or(Value::Null),
                "kind": h.get("kind").cloned().unwrap_or(Value::Null),
                "file": rel,
                "path": sym_path(h),
                "parent": sym_parent(h),
                "lineStart": h.get("lineStart").cloned().unwrap_or(Value::Null),
                "lineEnd": h.get("lineEnd").cloned().unwrap_or(Value::Null),
                "at": at,
                "body": symbol_text(&text, h),
            }));
        }
        let from = o.from.unwrap_or(0);
        let to = o.to.unwrap_or(0);
        if from != 0 || to != 0 {
            let start = if from == 0 { 1 } else { from.max(1) } as usize;
            let want = if to == 0 { start } else { to.max(0) as usize };
            let end = lines.len().min(start.max(want));
            let body = if start <= end { lines[start - 1..end].join("\n") } else { String::new() };
            return Ok(json!({
                "ok": true, "library": true, "file": rel,
                "lineStart": start, "lineEnd": end, "lines": lines.len(), "at": at, "body": body,
            }));
        }
        let shown = lines.len().min(max_lines);
        let mut out = json!({
            "ok": true, "library": true, "file": rel,
            "lineStart": 1, "lineEnd": shown, "lines": lines.len(), "at": at,
            "body": lines[..shown].join("\n"),
        });
        if shown < lines.len() {
            let m = out.as_object_mut().unwrap();
            m.insert("truncated".into(), json!(true));
            m.insert("more".into(), json!(format!("{rel}:{}-{}", shown + 1, lines.len())));
            m.insert(
                "note".into(),
                json!(format!(
                    "showing lines 1-{shown} of {}; read \"{rel}:{}-{}\" for the next stretch",
                    lines.len(),
                    shown + 1,
                    lines.len().min(shown + max_lines)
                )),
            );
        }
        Ok(out)
    }

    /// outlineLibrary(lib, limit, offset): a FILE → its symbols, else sections, else chunks; a
    /// DIRECTORY → the subtree's files with their top-level symbols.
    pub fn outline_library(&self, rel: &str, limit: usize, offset: usize) -> Result<Value> {
        if let Some(r) = self.path_refusal(rel) {
            return Ok(r);
        }
        let off = offset;
        let (orel, text, at) = match self.lib_file(rel)? {
            LibFile::Ok { rel, text, at, .. } => (rel, text, at),
            LibFile::Fail(v) => {
                if !LibFile::is_dir(&v) {
                    return Ok(v);
                }
                return self.outline_dir(rel, limit, off);
            }
        };
        let syms: Vec<Value> = extract(&orel, Some(&text)).get("symbols").and_then(Value::as_array).cloned().unwrap_or_default();
        let lines: Vec<&str> = text.split('\n').collect();
        let mut head = Map::new();
        head.insert("ok".into(), json!(true));
        head.insert("library".into(), json!(true));
        head.insert("file".into(), json!(orel));
        head.insert("lines".into(), json!(lines.len()));
        head.insert("at".into(), json!(at));
        let finish = |mut head: Map<String, Value>, by: &str, total: usize, page_len: usize, symbols: Vec<Value>| {
            let truncated = off + page_len < total;
            head.insert("by".into(), json!(by));
            head.insert("total".into(), json!(total));
            head.insert("offset".into(), json!(off));
            head.insert("truncated".into(), json!(truncated));
            head.insert("next".into(), if truncated { json!(off + page_len) } else { Value::Null });
            head.insert("symbols".into(), Value::Array(symbols));
            Value::Object(head)
        };
        if !syms.is_empty() {
            let page: Vec<&Value> = syms.iter().skip(off).take(limit).collect();
            let rows = page
                .iter()
                .map(|x| {
                    let (a, b) = (jint(x, "lineStart"), jint(x, "lineEnd"));
                    let doc = match x.get("doc") {
                        Some(Value::String(s)) if !s.is_empty() => json!(from_u16(&u16s(s)[..u16s(s).len().min(200)])),
                        Some(Value::Null) | None | Some(Value::Bool(false)) | Some(Value::String(_)) => Value::Null,
                        Some(v) => json!(v.to_string()),
                    };
                    json!({
                        "name": x.get("name").cloned().unwrap_or(Value::Null),
                        "path": sym_path(x),
                        "kind": x.get("kind").cloned().unwrap_or(Value::Null),
                        "parent": sym_parent(x),
                        "lineStart": x.get("lineStart").cloned().unwrap_or(Value::Null),
                        "lineEnd": x.get("lineEnd").cloned().unwrap_or(Value::Null),
                        "span": b - a + 1,
                        "signature": truthy_str(x.get("signature")).map_or(Value::Null, Value::String),
                        "doc": doc,
                    })
                })
                .collect();
            return Ok(finish(head, "symbol", syms.len(), page.len(), rows));
        }
        let sections = self.sections_outline(&orel, &text, limit, off);
        if let Value::Object(s) = sections {
            for (k, v) in s {
                head.insert(k, v);
            }
            return Ok(Value::Object(head));
        }
        let u = u16s(&text);
        let mut cut: Vec<Value> = Vec::new();
        for c in chunk_code(&text, Some(&orel)) {
            let a = line_before(&u, c.start);
            let nl = c.end >= 1 && u.get(c.end - 1) == Some(&10);
            let b = a.max(line_before(&u, c.end) - usize::from(nl));
            for (x, y) in super::lookup::split_piece(a, b, &c.text, self.opts.outline_max_lines) {
                cut.push(json!({ "name": "chunk", "kind": "chunk", "lineStart": x, "lineEnd": y, "doc": Value::Null }));
            }
        }
        let pieces = super::lookup::label_pieces(cut, &lines);
        let page: Vec<&Value> = pieces.iter().skip(off).take(limit).collect();
        let rows = page
            .iter()
            .map(|p| {
                let (a, b) = (jint(p, "lineStart"), jint(p, "lineEnd"));
                json!({
                    "name": p["name"],
                    "path": format!("{orel}:{a}-{b}"),
                    "kind": "chunk",
                    "parent": Value::Null,
                    "lineStart": a,
                    "lineEnd": b,
                    "span": (b - a + 1).max(1),
                    "signature": Value::Null,
                    "doc": p.get("doc").cloned().unwrap_or(Value::Null),
                })
            })
            .collect();
        Ok(finish(head, "chunk", pieces.len(), page.len(), rows))
    }

    fn outline_dir(&self, rel: &str, limit: usize, off: usize) -> Result<Value> {
        let scan = match self.scan_library(rel) {
            Ok(s) => s,
            Err(e) => return Ok(json!({ "ok": false, "reason": e.message })),
        };
        let files = scan["files"].as_array().cloned().unwrap_or_default();
        let n = files.len();
        let page = limit.min(n.saturating_sub(off));
        let truncated = off + page < n;
        let entries: Vec<Value> = files
            .iter()
            .skip(off)
            .take(limit)
            .map(|f| {
                let syms: Vec<Value> = f["symbols"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter(|x| sym_parent(x).is_null())
                            .map(|x| {
                                json!({
                                    "name": x.get("name").cloned().unwrap_or(Value::Null),
                                    "kind": x.get("kind").cloned().unwrap_or(Value::Null),
                                    "lineStart": x.get("lineStart").cloned().unwrap_or(Value::Null),
                                    "lineEnd": x.get("lineEnd").cloned().unwrap_or(Value::Null),
                                    "exported": x.get("exported").map_or(false, |e| !matches!(e, Value::Null | Value::Bool(false)) && e != &json!(0) && e != &json!("")),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                json!({ "file": f["rel"], "lines": f["lines"], "lang": f["lang"], "symbols": syms })
            })
            .collect();
        let lines: u64 = files.iter().map(|f| f["lines"].as_u64().unwrap_or(0)).sum();
        let symbol_count: usize = files.iter().map(|f| f["symbols"].as_array().map_or(0, Vec::len)).sum();
        Ok(json!({
            "ok": true,
            "library": true,
            "by": "files",
            "dir": if rel.is_empty() { ".".to_string() } else { rel.to_string() },
            "total": n,
            "offset": off,
            "lines": lines,
            "symbolCount": symbol_count,
            // The parse cap (opts.libraryParseFiles) stopped the listing.
            "capped": scan["truncated"],
            "cap": self.opts.library_parse_files,
            "truncated": truncated,
            "next": if truncated { json!(off + page) } else { Value::Null },
            "entries": entries,
        }))
    }

    /// readSection(rel, text, at, title, extra): the read of one heading's section (`file#Title`)
    /// from text in hand: a read result, an ambiguity, or null when no heading has that title.
    pub fn read_section(&self, rel: &str, text: &str, at: Option<&str>, title: &str, extra: Value) -> Value {
        let sec = section_of(rel, text, title);
        if sec.is_null() {
            return Value::Null;
        }
        if let Some(amb) = sec.get("ambiguous").and_then(Value::as_array) {
            let (shown, more) = capped(amb);
            return json!({
                "ok": false,
                "ambiguous": true,
                "reason": format!("\"{title}\" heads {} sections in {rel}{more} — read one by range", amb.len()),
                "candidates": shown.iter().map(|g| format!("{rel}:{}-{}", g["lineStart"], g["lineEnd"])).collect::<Vec<_>>(),
            });
        }
        let lines: Vec<&str> = text.split('\n').collect();
        let (a, b) = (jint(&sec, "lineStart").max(1) as usize, jint(&sec, "lineEnd").max(0) as usize);
        let b = b.min(lines.len());
        let mut m = Map::new();
        m.insert("ok".into(), json!(true));
        if let Value::Object(e) = extra {
            for (k, v) in e {
                m.insert(k, v);
            }
        }
        m.insert("name".into(), sec["title"].clone());
        m.insert("kind".into(), json!("heading"));
        m.insert("file".into(), json!(rel));
        m.insert("path".into(), sec["title"].clone());
        m.insert("parent".into(), Value::Null);
        m.insert("lineStart".into(), sec["lineStart"].clone());
        m.insert("lineEnd".into(), sec["lineEnd"].clone());
        m.insert("lines".into(), json!(lines.len()));
        m.insert("at".into(), at.map_or(Value::Null, |s| json!(s)));
        m.insert("body".into(), json!(if a <= b { lines[a - 1..b].join("\n") } else { String::new() }));
        Value::Object(m)
    }

    /// sectionsOutline(rel, text, limit, offset): the outline of a text with no symbols
    /// (`by: 'section'`); null for code.
    pub fn sections_outline(&self, rel: &str, text: &str, limit: usize, offset: usize) -> Value {
        let all = match text_regions(rel, text, REGION_LINES as i64, LOG_LINES as i64) {
            Some(Value::Array(a)) if !a.is_empty() => a,
            _ => return Value::Null,
        };
        let mut seen: HashMap<&str, usize> = HashMap::new();
        for g in &all {
            if g["kind"] == "heading" {
                *seen.entry(g["title"].as_str().unwrap_or("")).or_default() += 1;
            }
        }
        let off = offset;
        let page: Vec<&Value> = all.iter().skip(off).take(limit).collect();
        let truncated = off + page.len() < all.len();
        let symbols: Vec<Value> = page
            .iter()
            .map(|g| {
                let title = g["title"].as_str().unwrap_or("");
                let (a, b) = (jint(g, "lineStart"), jint(g, "lineEnd"));
                let path = if g["kind"] == "heading" && seen.get(title) == Some(&1) { format!("{rel}#{title}") } else { format!("{rel}:{a}-{b}") };
                let mut m = Map::new();
                m.insert("name".into(), g["title"].clone());
                m.insert("path".into(), json!(path));
                m.insert("kind".into(), g["kind"].clone());
                m.insert("parent".into(), Value::Null);
                if jint(g, "level") != 0 {
                    m.insert("level".into(), g["level"].clone());
                }
                m.insert("lineStart".into(), json!(a));
                m.insert("lineEnd".into(), json!(b));
                m.insert("span".into(), json!(b - a + 1));
                m.insert("signature".into(), Value::Null);
                m.insert("doc".into(), Value::Null);
                Value::Object(m)
            })
            .collect();
        json!({
            "by": "section",
            "total": all.len(),
            "offset": off,
            "truncated": truncated,
            "next": if truncated { json!(off + page.len()) } else { Value::Null },
            "symbols": symbols,
        })
    }

    // ── api ─────────────────────────────────────────────────────────────

    /// isLibraryPath(p): synchronous, never calls the facade.
    pub fn is_library_path(&self, p: &str) -> bool {
        self.library_path_ex(p) != LibPath::No
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_before_counts_newlines() {
        let u = u16s("a\nb\nc");
        assert_eq!(line_before(&u, 0), 1);
        assert_eq!(line_before(&u, 3), 2);
        assert_eq!(line_before(&u, 99), 3);
    }

    #[test]
    fn capped_says_how_many_were_cut() {
        let v: Vec<u8> = (0..10).collect();
        let (s, more) = capped(&v);
        assert_eq!(s.len(), 8);
        assert_eq!(more, " (showing 8 of 10)");
    }

    #[test]
    fn symbol_text_uses_utf16_offsets() {
        let s = json!({ "start": 1, "end": 4, "lineStart": 1, "lineEnd": 1 }); // é is 1 unit, 😀 is 2
        assert_eq!(symbol_text("é😀abc", &s), "😀a");
    }
}
