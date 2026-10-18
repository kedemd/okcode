//! Persistence for ONE workspace's code graph, in okdb (lib/store.js).
//!
//! One okdb ENVIRONMENT per workspace (`env_name_for(id)`); types `files` (metadata only, NO content),
//! `symbols` (key `rel::symPath::line`, indexed on `file` and `name`), `packages`. FTS `text` over
//! symbols (name/path/doc/signature) and, with `fts: full`, over `files.content` — a RESOLVED field
//! served by a batch resolver that asks the workspace's text cache first (`set_text_source`) and
//! reads the rest through the access facade. Embedding profiles index `files.prepared`.
//!
//! All okdb work runs on the db's owner thread through `DbHost::call`; `Store` itself is `Send + Sync`.
//! Requests use okdb's JS call shapes (see `Store::emb`).
use crate::db::DbHost;
use crate::error::{Error, Result};
use crate::record::FileRecord;
use indexmap::IndexMap;
use okcode_analysis::access::Access;
use okcode_analysis::chunk as chunkmod;
use okcode_analysis::embed::identity::{self, Parts};
use crate::db::{fn_marker, register_callback, BatchCall};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

pub const FILES: &str = "files";
pub const SYMBOLS: &str = "symbols";
pub const PACKAGES: &str = "packages";
pub const FTS_NAME: &str = "text";
pub const CONTENT: &str = "content";
pub const PREPARED: &str = "prepared";

#[rustfmt::skip]
pub const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "can", "do", "does",
    "for", "from", "get", "how", "i", "if", "in", "into", "is", "it", "its",
    "of", "on", "or", "that", "the", "their", "then", "there", "these", "this",
    "to", "was", "we", "what", "when", "where", "which", "why", "will", "with",
    "you", "your",
];

const SLUG_MAX: usize = 40;

/// store.js `fts` option.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FtsMode {
    #[default]
    Full,
    Symbols,
}

impl FtsMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            FtsMode::Full => "full",
            FtsMode::Symbols => "symbols",
        }
    }
}

/// The workspace's text cache, asked first by the content/prepared resolvers: (rel, hash) → text.
/// LOCKING: it runs on the db owner thread (inside `fts.flush`/`ready`/drains) and on the caller's thread
/// in `prefetch`. Callers must NEVER hold the workspace `ix` lock across `save_files`/`search`/`settle`,
/// or a source that takes `ix.read()` deadlocks (same thread, or owner thread vs. the blocked caller).
pub type TextSource = Arc<dyn Fn(&str, &str) -> Option<Arc<str>> + Send + Sync>;
pub type LogFn = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Clone, Default)]
pub struct StoreOptions {
    pub id: String,
    /// embedding profiles as given (JSON; a custom embedder's JS function is registered on the db by lib/embedders.js)
    pub profiles: Vec<Value>,
    pub fts: FtsMode,
    pub log: Option<LogFn>,
}

/// store.load() → `{ files, packages }` (insertion order = okdb key order).
#[derive(Clone, Debug, Default)]
pub struct Loaded {
    pub files: Vec<FileRecord>,
    pub packages: Vec<Value>,
}

/// store.saveFiles() → `{ written, failed: [{ rel, error }] }`.
#[derive(Clone, Debug, Default)]
pub struct SaveOutcome {
    pub written: usize,
    pub failed: Vec<(String, String)>,
}

/// asQuery(): a string, or `{ text?, vector?, identity? }`.
#[derive(Clone, Debug, Default)]
pub struct Query {
    pub text: Option<String>,
    pub vector: Option<Vec<f32>>,
    pub identity: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AskOptions {
    pub profile: Option<String>,
    pub limit: usize,
    pub text: bool,
}
impl Default for AskOptions {
    fn default() -> Self {
        AskOptions { profile: None, limit: 8, text: false }
    }
}

/// One quoted line (quoteLines): `{ line, n, text }`.
#[derive(Clone, Debug, PartialEq)]
pub struct Quote {
    pub line: usize,
    pub n: usize,
    pub text: String,
}

// ── small JS-semantics helpers ─────────────────────────────────────────────


fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64().map_or(true, |f| f != 0.0 && !f.is_nan()),
        _ => true,
    }
}

/// JS `String(number)` for the values that occur (integers print without a fraction).
fn js_number(f: f64) -> String {
    if f.is_nan() {
        "NaN".into()
    } else if f.is_infinite() {
        if f > 0.0 { "Infinity".into() } else { "-Infinity".into() }
    } else if f.fract() == 0.0 && f.abs() < 1e21 {
        format!("{}", f as i128)
    } else {
        format!("{f}")
    }
}

/// JS `String(v)`.
fn js_string(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => js_number(n.as_f64().unwrap_or(f64::NAN)),
        // Array.prototype.toString: elements joined by ',', null as ''
        Some(Value::Array(a)) => a.iter().map(|x| if x.is_null() { String::new() } else { js_string(Some(x)) }).collect::<Vec<_>>().join(","),
        Some(Value::Object(o)) if o.contains_key("$re") => {
            format!("/{}/{}", js_string(o.get("$re")), o.get("flags").and_then(Value::as_str).unwrap_or(""))
        }
        Some(Value::Object(_)) => "[object Object]".into(),
    }
}

/// JS `Number(v)` for the JSON shapes that occur.
fn js_to_number(v: &Value) -> f64 {
    match v {
        Value::Null => 0.0,
        Value::Bool(b) => *b as u8 as f64,
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::String(s) => {
            let t = s.trim_matches(is_js_space);
            if t.is_empty() { 0.0 } else { t.parse::<f64>().unwrap_or(f64::NAN) }
        }
        _ => f64::NAN,
    }
}

/// A non-negative count from a JS number: NaN -> 0, +Infinity/huge -> capped (never overflows `limit * 3`).
fn clamp_count(f: f64) -> usize {
    if f.is_nan() || f <= 0.0 { 0 } else { f.min(9_007_199_254_740_991.0) as usize }
}

fn is_js_space(c: char) -> bool {
    c != '\u{85}' && (c.is_whitespace() || c == '\u{FEFF}')
}

/// `s.slice(0, n)` in UTF-16 code units (a split surrogate pair is dropped, never emitted).
fn slice_utf16(s: &str, n: usize) -> String {
    let mut out = String::new();
    let mut units = 0;
    for c in s.chars() {
        let w = c.len_utf16();
        if units + w > n {
            break;
        }
        units += w;
        out.push(c);
    }
    out
}

fn f64_of(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64)
}

/// symKey(rel, symPath, line): deterministic keys, so a re-scan overwrites rather than accumulating.
pub fn sym_key(rel: &str, sym_path: &str, line: &str) -> String {
    format!("{rel}::{sym_path}::{line}")
}

/// indexedText(v): the SYMBOLS index fields are written as strings whatever an analyser handed over.
pub fn indexed_text(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => js_number(n.as_f64().unwrap_or(f64::NAN)),
        Some(Value::Object(o)) if o.contains_key("$re") => {
            format!("/{}/{}", js_string(o.get("$re")), o.get("flags").and_then(Value::as_str).unwrap_or(""))
        }
        Some(other) => serde_json::to_string(other).unwrap_or_else(|_| other.to_string()),
    }
}

// ── pure helpers (exported by store.js; used by the workspace too) ─────────

/// envNameFor(id): `okcode_` + slug, plus a short sha1 of the exact id when the slug changed it.
pub fn env_name_for(id: &str) -> Result<String> {
    use sha1::{Digest, Sha1};
    if id.is_empty() {
        return Err(Error::msg("a workspace id is required"));
    }
    let slug = identity::slug(id);
    let s: String = slug.chars().take(SLUG_MAX).collect();
    let tag = if s == id {
        String::new()
    } else {
        let d = Sha1::digest(id.as_bytes());
        let hex: String = d.iter().map(|b| format!("{b:02x}")).collect();
        format!("_{}", &hex[..8])
    };
    Ok(format!("okcode_{}{tag}", if s.is_empty() { "ws" } else { &s }))
}

/// queryTerms(query): lowercase terms, split on non-word, len > 1, stopwords out.
pub fn query_terms(query: &str) -> Vec<String> {
    query
        .to_lowercase()
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$'))
        .filter(|t| t.chars().count() > 1 && !STOPWORDS.contains(t))
        .map(String::from)
        .collect()
}

/// quoteLines(text, query, perFile = 3): the lines matching the MOST of the query's terms.
pub fn quote_lines(text: &str, query: &str, per_file: usize) -> Vec<Quote> {
    let terms = query_terms(query);
    let mut scored: Vec<Quote> = Vec::new();
    for (i, line) in text.split('\n').enumerate() {
        let low = line.to_lowercase();
        let n = terms.iter().filter(|t| low.contains(t.as_str())).count();
        if n > 0 {
            scored.push(Quote { line: i + 1, n, text: slice_utf16(line.trim_matches(is_js_space), 200) });
        }
    }
    scored.sort_by(|a, b| b.n.cmp(&a.n).then(a.line.cmp(&b.line)));
    scored.truncate(per_file);
    scored
}

/// asQuery(query) — OKCODE_BAD_QUERY (as Err) for anything but a string or `{ text?, vector?, identity? }`.
pub fn as_query(query: &Value) -> Result<Query> {
    let bad = |why: &str| Error::code("OKCODE_BAD_QUERY", format!("ask: {why}"));
    if let Value::String(s) = query {
        return Ok(Query { text: Some(s.clone()), vector: None, identity: None });
    }
    let Value::Object(o) = query else {
        return Err(bad("the query must be a string or { text?, vector? }"));
    };
    let mut vector = None;
    if let Some(v) = o.get("vector").filter(|v| !v.is_null()) {
        let Value::Array(a) = v else {
            return Err(bad("vector must be a Float32Array (or an array of numbers)"));
        };
        let vec: Vec<f32> = a.iter().map(|x| x.as_f64().map_or(f32::NAN, |f| f as f32)).collect();
        if vec.is_empty() {
            return Err(bad("vector is empty"));
        }
        vector = Some(vec);
    }
    let text = o.get("text").and_then(Value::as_str).filter(|t| !t.trim_matches(is_js_space).is_empty()).map(String::from);
    if text.is_none() && vector.is_none() {
        return Err(bad("needs text, a vector, or both"));
    }
    let identity = o.get("identity").and_then(Value::as_str).map(String::from);
    Ok(Query { text, vector, identity })
}

fn query_json(q: &Query) -> Value {
    json!({
        "text": q.text,
        "vector": q.vector.as_ref().map(|v| v.iter().map(|x| json!(*x as f64)).collect::<Vec<_>>()),
        "identity": q.identity,
    })
}

/// store.js's pure exports: envNameFor, quoteLines, queryTerms, asQuery.
pub fn call_pure(name: &str, args: &[Value]) -> Result<Value> {
    let a = |i: usize| args.get(i).unwrap_or(&Value::Null);
    match name {
        "envNameFor" => match a(0) {
            Value::String(s) => env_name_for(s).map(Value::String),
            _ => Err(Error::msg("a workspace id is required")),
        },
        "queryTerms" => Ok(json!(query_terms(&js_string(Some(a(0)))))),
        "quoteLines" => {
            let text = if truthy(Some(a(0))) { js_string(Some(a(0))) } else { String::new() };
            let all = quote_lines(&text, &js_string(Some(a(1))), usize::MAX);
            // `.slice(0, perFile)`: ToIntegerOrInfinity, a negative end counts from the back
            let n = all.len() as f64;
            let pf = js_to_number(a(2));
            let pf = if pf.is_nan() { 0.0 } else { pf.trunc() };
            let end = if pf < 0.0 { (n + pf).max(0.0) } else { pf.min(n) } as usize;
            Ok(Value::Array(all.into_iter().take(end).map(|q| json!({"line": q.line, "n": q.n, "text": q.text})).collect()))
        }
        "asQuery" => as_query(a(0)).map(|q| query_json(&q)),
        other => Err(Error::msg(format!("store.js has no pure export {other}"))),
    }
}

// ── chunking (analysis/chunk.js#headedChunks) ──────────────────────────────

/// The chunk strategy the store registers on the db under `chunk::STRATEGY`: chunks of the PREPARED
/// text (`//@ rel\n` + text), each headed with where it came from. Offsets (UTF-16) stay relative to
/// the prepared text. The store registers this as the Rust-side `okcode-symbols` strategy.
pub fn headed_chunks(text: &str) -> Vec<chunkmod::Chunk> {
    let mut body = text;
    let mut rel: Option<String> = None;
    let mut offset = 0usize;
    if text.starts_with(chunkmod::MARK) {
        let mark = chunkmod::MARK.len();
        match text.find('\n') {
            Some(nl) => {
                rel = Some(text[mark..nl.max(mark)].to_string());
                offset = text[..nl + 1].encode_utf16().count();
                body = &text[nl + 1..];
            }
            None => {
                // no newline: JS `text.slice(MARK.length, -1)` (drops the last char), body = the whole text, offset 0
                let mut end = text.len();
                if end > mark {
                    end -= text[mark..].chars().next_back().map_or(0, char::len_utf8);
                }
                rel = Some(text[mark..end.max(mark)].to_string());
            }
        }
    }
    let chunks = chunkmod::chunk_code(body, rel.as_deref());
    let Some(rel) = rel.filter(|r| !r.is_empty()) else { return chunks };
    let symbols: Vec<Value> = okcode_analysis::extract(&rel, Some(body)).get("symbols").and_then(Value::as_array).cloned().unwrap_or_default();
    // char offset -> line, once
    let mut line_at = vec![0usize];
    for (i, u) in body.encode_utf16().enumerate() {
        if u == 10 {
            line_at.push(i + 1);
        }
    }
    let line_of = |pos: usize| -> f64 {
        let (mut lo, mut hi) = (0usize, line_at.len() - 1);
        while lo < hi {
            let mid = (lo + hi + 1) >> 1;
            if line_at[mid] <= pos {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        (lo + 1) as f64
    };
    let span = |s: &Value| f64_of(s.get("lineEnd")).unwrap_or(f64::NAN) - f64_of(s.get("lineStart")).unwrap_or(f64::NAN);
    chunks
        .into_iter()
        .map(|c| {
            let line = line_of(c.start);
            let mut owner: Option<&Value> = None;
            for s in &symbols {
                let (ls, le) = (f64_of(s.get("lineStart")).unwrap_or(f64::NAN), f64_of(s.get("lineEnd")).unwrap_or(f64::NAN));
                if line < ls || line > le {
                    continue;
                }
                if owner.map_or(true, |o| span(s) < span(o)) {
                    owner = Some(s);
                }
            }
            let mut head = format!("// {rel}");
            if let Some(o) = owner {
                let name = if truthy(o.get("path")) { o.get("path") } else { o.get("name") };
                head.push_str(&format!(" \u{203a} {}", js_string(name)));
            }
            head.push('\n');
            if let Some(doc) = owner.and_then(|o| o.get("doc")).filter(|d| truthy(Some(d))) {
                head.push_str(&format!("// {}\n", slice_utf16(&js_string(Some(doc)), 200)));
            }
            chunkmod::Chunk { text: format!("{head}\n{}", c.text), start: c.start + offset, end: c.end + offset }
        })
        .collect()
}

// ── the store ──────────────────────────────────────────────────────────────

/// profile name -> { name, model, parts, dims, identity, pipeline, scoped, embedderName, error, pending, spec }
#[derive(Clone, Debug)]
struct ProfileState {
    name: String,
    model: Option<String>,
    parts: Parts,
    dims: Option<Value>,
    identity: Option<String>,
    pipeline: Option<String>,
    scoped: Option<String>,
    embedder_name: Option<String>,
    error: Option<String>,
    pending: Option<String>,
    spec: Value,
    /// which `addProfile` created this state (a stale attempt must not overwrite a newer one)
    gen: u64,
}

fn next_gen() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static G: AtomicU64 = AtomicU64::new(1);
    G.fetch_add(1, Ordering::Relaxed)
}

/// A dispatch that the native db does not offer (yet) answers "not implemented": where JS guards the call
/// with a feature check (`resolveModelDims`, `_durableIndexerStats`), that is "absent", not a failure.
fn not_implemented(e: &Error) -> bool {
    e.code.as_deref() == Some("OKCODE_NOT_IMPLEMENTED")
}

impl ProfileState {
    fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "model": self.model,
            "parts": self.parts.to_json(),
            "dims": self.dims,
            "identity": self.identity,
            "pipeline": self.pipeline,
            "scoped": self.scoped,
            "embedderName": self.embedder_name,
            "error": self.error,
            "pending": self.pending,
            "spec": self.spec,
        })
    }
}

enum Attach {
    Done,
    Wait(String),
}

/// Text the resolvers may use WITHOUT reading through the access facade. A facade read can be served
/// by the JS thread, and the resolvers run on the db owner thread — which a JS thread blocked in a
/// sync store call is waiting on. So the threads that save files (never the JS thread, never the owner)
/// read what the text cache lacks BEFORE handing the rows over, and leave it here (bounded, oldest out).
#[derive(Default)]
struct Stash {
    map: IndexMap<String, (String, Arc<str>)>,
    bytes: usize,
}

const STASH_MAX_BYTES: usize = 96 << 20;

impl Stash {
    fn put(&mut self, rel: &str, hash: &str, text: Arc<str>) {
        if let Some((_, old)) = self.map.shift_remove(rel) {
            self.bytes -= old.len();
        }
        self.bytes += text.len();
        self.map.insert(rel.to_string(), (hash.to_string(), text));
        while self.bytes > STASH_MAX_BYTES && self.map.len() > 1 {
            if let Some((_, (_, old))) = self.map.shift_remove_index(0) {
                self.bytes -= old.len();
            }
        }
    }
    fn get(&self, rel: &str, hash: &str) -> Option<Arc<str>> {
        self.map.get(rel).filter(|(h, _)| h == hash).map(|(_, t)| t.clone())
    }
    fn forget(&mut self, rel: &str) {
        if let Some((_, old)) = self.map.shift_remove(rel) {
            self.bytes -= old.len();
        }
    }
}

/// Sync store calls in flight (the JS thread is blocked on the db owner thread for each).
static SYNC_PENDING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Marks a sync store call (JS thread blocked on the owner) for as long as it lives.
pub struct SyncGuard;
impl SyncGuard {
    pub fn enter() -> SyncGuard {
        SYNC_PENDING.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        SyncGuard
    }
}
impl Drop for SyncGuard {
    fn drop(&mut self) {
        SYNC_PENDING.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Code of a resolver item whose content is still being read: okdb holds and retries such items
/// (`FIELD_RESOLVER_PENDING`) instead of recording an FTS error and dropping the document.
const PENDING: &str = "FIELD_RESOLVER_PENDING";

/// Facade reads started by the resolvers and not yet collected: (facade, rel) -> bytes (None while in
/// flight). Keyed by the facade too: two stores in one process index the same names (consecutive tests),
/// and a read left in flight by one facade must never stall another.
static READS: std::sync::Mutex<Option<HashMap<String, Option<Vec<u8>>>>> = std::sync::Mutex::new(None);

/// The resolvers' last-resort facade read, on the db owner thread. The facade may need the JS thread,
/// which may be blocked on this very thread by a sync call, so this NEVER waits: it takes what an earlier
/// call's background read delivered, starts reads for the rest, and reports those as pending (None). The
/// caller marks such items `FIELD_RESOLVER_PENDING`, which okdb retries; nothing is dropped.
fn owner_read(acc: &Arc<dyn Access>, rels: Vec<String>) -> Vec<Option<Vec<u8>>> {
    let sync_pending = SYNC_PENDING.load(std::sync::atomic::Ordering::SeqCst) > 0;
    let mut out = Vec::with_capacity(rels.len());
    let mut start: Vec<String> = Vec::new();
    let fac = Arc::as_ptr(acc) as *const () as usize;
    let key = move |r: &str| format!("{fac:x}\0{r}");
    {
        let mut g = READS.lock().unwrap_or_else(|e| e.into_inner());
        let m = g.get_or_insert_with(HashMap::new);
        for r in &rels {
            let k = key(r);
            match m.get(&k) {
                Some(Some(_)) => out.push(m.remove(&k).flatten()),
                Some(None) => out.push(None),
                None => {
                    if !sync_pending {
                        m.insert(k, None);
                        start.push(r.clone());
                    }
                    out.push(None);
                }
            }
        }
    }
    if !start.is_empty() {
        let a = acc.clone();
        std::thread::spawn(move || {
            let got = a.read(&start).unwrap_or_default();
            let mut g = READS.lock().unwrap_or_else(|e| e.into_inner());
            let m = g.get_or_insert_with(HashMap::new);
            let mut have: HashMap<String, Vec<u8>> = got.into_iter().collect();
            for r in &start {
                // unreadable/missing: an empty answer ("no content"), same as JS's read failure path
                m.insert(key(r), Some(have.remove(r).unwrap_or_default()));
            }
        });
    }
    out
}

pub struct Store {
    pub id: String,
    pub env_name: String,
    pub fts_mode: FtsMode,
    db: Arc<DbHost>,
    access: Arc<dyn Access>,
    fts: bool,
    content_fts: bool,
    /// `db.role.processors !== false`: this process drains derived work (FTS, embeddings)
    drains: bool,
    /// `db.role.engines !== false`: this process may create/repair a profile's embedder and pipeline
    creates: bool,
    text_source: Arc<RwLock<Option<TextSource>>>,
    stash: Arc<Mutex<Stash>>,
    profiles: Mutex<IndexMap<String, ProfileState>>,
    /// profile name -> the in-flight attach attempt (concurrent callers wait for it, as in JS)
    attaching: Mutex<HashMap<(String, u64), Arc<Attempt>>>,
    log: LogFn,
}

/// One in-flight attach attempt: its waiters block on `done` until the leader publishes the outcome.
struct Attempt {
    done: std::sync::Condvar,
    outcome: Mutex<Option<ProfileState>>,
}

fn fts_config(fields: &[&str]) -> Value {
    json!({ "fields": fields, "tokenizer": { "stopwords": STOPWORDS } })
}

fn shape(c: Option<&Value>) -> String {
    let join = |v: Option<&Value>| -> String {
        v.and_then(Value::as_array).map(|a| a.iter().map(|x| js_string(Some(x))).collect::<Vec<_>>().join(",")).unwrap_or_default()
    };
    format!("{}|{}", join(c.and_then(|c| c.get("fields"))), join(c.and_then(|c| c.get("tokenizer")).and_then(|t| t.get("stopwords"))))
}


/// Make the `text` FTS index of `ty` match `config` (rebuild only on a real difference). The index
/// builds in the background; a draining process waits for it. true = the index is present.
///
/// Runs on the CALLING thread, one short owner job per step: a build that needs a resolver answer
/// (FIELD_RESOLVER_PENDING / BUILD_CONTINUE) must let the owner loop run between steps, as okdb-node's
/// own awaiting does, or the index is left `resetting`.
fn ensure_fts(db: &Arc<DbHost>, env_name: &str, ty: &str, config: &Value, drains: bool, log: &LogFn) -> bool {
    let en = Some(env_name);
    let step = (|| -> Result<()> {
        let current = db
            .call("fts", None, "list", vec![json!(ty), json!(env_name)])?
            .as_array()
            .and_then(|l| l.iter().find(|x| x.get("name").and_then(Value::as_str) == Some(FTS_NAME)).cloned());
        let want = shape(Some(config));
        if let Some(cur) = &current {
            if shape(cur.get("config").filter(|c| !c.is_null())) != want {
                let _ = db.call("fts", None, "drop", vec![json!(ty), json!(FTS_NAME), json!(env_name)]);
            }
        }
        let ensure = || db.call("fts", None, "ensure", vec![json!(ty), json!(FTS_NAME), config.clone(), json!(env_name)]).map(|_| ());
        match ensure() {
            // okdb 3.0.0 keeps a removed env's FTS registration in memory: an env recreated under the same name
            // (reset scope 'all') is refused ALREADY_EXISTS for an index its fts.list does not show. Drop the
            // stale registration and ensure again.
            Err(e) if current.is_none() && e.code.as_deref() == Some("ALREADY_EXISTS") => {
                let _ = db.call("fts", None, "drop", vec![json!(ty), json!(FTS_NAME), json!(env_name)]);
                ensure()
            }
            r => r,
        }
    })();
    let _ = en;
    let mut outcome: std::result::Result<(), String> = match step {
        Ok(()) => Ok(()),
        Err(e) if matches!(e.code.as_deref(), Some("FIELD_RESOLVER_PENDING") | Some("FTS_BUILD_CONTINUE")) => {
            // The index is registered. okdb-node's `ensure` (the JS `await db.fts.ensure`) finishes the
            // build whatever the processor role, so every process drives it here, not only a draining one.
            let first = if e.code.as_deref() == Some("FIELD_RESOLVER_PENDING") { Some(Value::Bool(true)) } else { None };
            crate::db::drive_build(db, ty, FTS_NAME, env_name, first).map_err(|e| e.message)
        }
        Err(e) => Err(e.message),
    };
    if outcome.is_ok() && drains {
        // Searching before the index is ready looks exactly like "no such symbol", so a process that
        // drains waits for it. One that does not must not: nothing here would ever make it ready.
        let _ = db.call("fts", Some(env_name), "ready", vec![json!(ty), json!(FTS_NAME), json!(env_name)]);
    }
    match std::mem::replace(&mut outcome, Ok(())) {
        Ok(()) => true,
        Err(m) => {
            log(&format!("[okcode] full-text index on {ty} unavailable: {m}"));
            db.call("fts", None, "has", vec![json!(ty), json!(FTS_NAME), json!(env_name)]).ok().and_then(|v| v.as_bool()).unwrap_or(false)
        }
    }
}


fn row_str<'a>(row: &'a Value, k: &str) -> Option<&'a str> {
    row.get(k).and_then(Value::as_str)
}

/// One file's rows, ready to write.
struct Staged {
    rel: String,
    row: Value,
    symbols: Vec<(String, Value)>,
    warnings: Vec<String>,
    /// A row the binding could not encode (a cycle): fails alone, with this message.
    bad: Option<String>,
    /// JS already logged the unencodable row (synchronously), so native does not log it again.
    logged: bool,
}

impl Store {
    /// openStore({ db, id, access, profiles, fts, log }): get-or-create the env, ensure types,
    /// resolvers, FTS indexes and profiles.
    pub fn open(db: Arc<DbHost>, access: Arc<dyn Access>, opts: StoreOptions) -> Result<Store> {
        let env_name = env_name_for(&opts.id)?;
        let log: LogFn = opts.log.clone().unwrap_or_else(|| Arc::new(|_| {}));
        let text_source: Arc<RwLock<Option<TextSource>>> = Arc::new(RwLock::new(None));
        let mode = opts.fts;

        let stash: Arc<Mutex<Stash>> = Arc::new(Mutex::new(Stash::default()));
        let (acc, ts, sh) = (access.clone(), text_source.clone(), stash.clone());
        // get-or-create: openEnv throws ENV_NOT_FOUND for a name never registered, which is exactly the first run.
        match db.call("db", None, "openEnv", vec![json!(env_name)]) {
            Ok(_) => {}
            Err(e) if e.code.as_deref() == Some("ENV_NOT_FOUND") => {
                db.call("db", None, "createEnvironment", vec![json!(env_name), json!({})])?;
            }
            Err(e) => return Err(e),
        }
        // Registered per PROCESS, before any profile attaches: okdb calls back into headedChunks.
        static CHUNK_FN: OnceLock<u64> = OnceLock::new();
        let chunk_fn = *CHUNK_FN.get_or_init(|| {
            register_callback(Arc::new(|a: &[Value]| {
                let text = a.first().and_then(Value::as_str).unwrap_or("");
                Ok(Value::Array(headed_chunks(text).into_iter().map(|c| json!({ "text": c.text, "start": c.start, "end": c.end })).collect()))
            }))
        });
        let setup = db.call_batch(
            false,
            vec![
                BatchCall { scope: "embeddings".into(), env: None, method: "registerChunkStrategy".into(), args: vec![json!(chunkmod::STRATEGY), fn_marker(chunk_fn)] },
                BatchCall { scope: "env".into(), env: Some(env_name.clone()), method: "ensureType".into(), args: vec![json!(FILES)] },
                BatchCall { scope: "env".into(), env: Some(env_name.clone()), method: "ensureType".into(), args: vec![json!(SYMBOLS), json!({ "indexes": [["file"], ["name"]] })] },
                BatchCall { scope: "env".into(), env: Some(env_name.clone()), method: "ensureType".into(), args: vec![json!(PACKAGES)] },
            ],
        )?;
        for r in setup {
            r?;
        }

        // ── resolved content ────────────────────────────────────
        // The workspace holds the text it has already read, keyed by rel and hash. A resolver batch
        // asks there first, then the stash the saving thread filled, and reads through the facade only
        // for what neither has. They run on okdb's owner thread (attached: the JS thread): they must never
        // be the reason the JS thread is waited on (see `Stash`); a read still in flight answers PENDING and
        // okdb holds and retries the item.
        let texts_of = Arc::new(move |items: &[Value]| -> Vec<std::result::Result<Option<String>, ()>> {
            let mut out: Vec<std::result::Result<Option<String>, ()>> = (0..items.len()).map(|_| Ok(None)).collect();
            let mut need: Vec<usize> = Vec::new();
            let source = ts.read().ok().and_then(|g| g.clone());
            for (i, it) in items.iter().enumerate() {
                let row = &it["row"];
                // Opaque rows (binary, oversized, secret) are never read: a `.env` must not become tokens.
                let Some(rel) = row_str(row, "rel") else { continue };
                if row.get("indexed") == Some(&Value::Bool(false)) {
                    continue;
                }
                let hash = row_str(row, "hash").unwrap_or("");
                let cached = source.as_ref().and_then(|f| f(rel, hash)).or_else(|| sh.lock().ok().and_then(|g| g.get(rel, hash)));
                match cached {
                    Some(t) => out[i] = Ok(Some(t.to_string())),
                    None => need.push(i),
                }
            }
            if !need.is_empty() {
                let rels: Vec<String> = need.iter().filter_map(|&i| row_str(&items[i]["row"], "rel").map(String::from)).collect();
                let got = owner_read(&acc, rels);
                for (&i, buf) in need.iter().zip(got) {
                    out[i] = match buf {
                        Some(b) => Ok(Some(String::from_utf8_lossy(&b).into_owned())),
                        None => Err(()),
                    };
                }
            }
            out
        });
        let pending = || json!({ "__error": true, "code": PENDING, "message": "content read in flight" });
        let items_of = |a: &[Value]| a.first().and_then(Value::as_array).cloned().unwrap_or_default();
        let t1 = texts_of.clone();
        let content_fn = register_callback(Arc::new(move |a: &[Value]| {
            let items = items_of(a);
            Ok(Value::Array(t1(&items).into_iter().map(|t| match t {
                Ok(Some(t)) => Value::String(t),
                Ok(None) => Value::Null,
                Err(()) => pending(),
            }).collect()))
        }));
        let t2 = texts_of.clone();
        let prepared_fn = register_callback(Arc::new(move |a: &[Value]| {
            let items = items_of(a);
            let texts = t2(&items);
            Ok(Value::Array(items.iter().zip(texts).map(|(it, t)| match t {
                Err(()) => pending(),
                Ok(t) => {
                    let row = &it["row"];
                    let rel = row_str(row, "rel").unwrap_or("");
                    let indexed = row.get("indexed") != Some(&Value::Bool(false));
                    let p = chunkmod::prepare_text(rel, t.as_deref(), indexed);
                    if p.is_empty() { Value::Null } else { Value::String(p) }
                }
            }).collect()))
        }));
        for (field, id) in [(CONTENT, content_fn), (PREPARED, prepared_fn)] {
            db.call("env", Some(&env_name), "resolveField", vec![json!(FILES), json!(field), Value::Null, json!({ "batch": fn_marker(id) })])?;
        }
        let role = db.call("db", None, "role", vec![])?;
        struct Role {
            processors: bool,
            engines: bool,
        }
        let role = Role { processors: role.get("processors") != Some(&Value::Bool(false)), engines: role.get("engines") != Some(&Value::Bool(false)) };
        let (drains, creates) = (role.processors, role.engines);
        // ── full text ───────────────────────────────────────────
        let fts = ensure_fts(&db, &env_name, SYMBOLS, &fts_config(&["name", "path", "doc", "signature"]), drains, &log);
        // `fts: 'symbols'` drops the full-text index over file CONTENT; 'full' builds it if absent.
        // Flipping the option on a workspace that already has one takes effect on this very open.
        let content_fts = if mode == FtsMode::Symbols {
            if db.call("fts", None, "has", vec![json!(FILES), json!(FTS_NAME), json!(env_name)]).ok().and_then(|v| v.as_bool()).unwrap_or(false) {
                let _ = db.call("fts", None, "drop", vec![json!(FILES), json!(FTS_NAME), json!(env_name)]);
            }
            false
        } else {
            ensure_fts(&db, &env_name, FILES, &fts_config(&["rel", CONTENT]), drains, &log)
        };

        let store = Store {
            id: opts.id.clone(),
            env_name,
            fts_mode: mode,
            db,
            access,
            fts,
            content_fts,
            drains,
            creates,
            text_source,
            stash,
            profiles: Mutex::new(IndexMap::new()),
            attaching: Mutex::new(HashMap::new()),
            log,
        };
        for p in &opts.profiles {
            store.ensure_profile(p)?;
        }
        Ok(store)
    }

    pub fn db(&self) -> &Arc<DbHost> {
        &self.db
    }
    pub fn access(&self) -> &Arc<dyn Access> {
        &self.access
    }
    /// `store.fts` — symbol FTS present
    pub fn fts(&self) -> bool {
        self.fts
    }
    /// `store.contentFts` — content FTS present
    pub fn content_fts(&self) -> bool {
        self.content_fts
    }

    fn log(&self, msg: &str) {
        (self.log)(msg);
    }

    /// One request against this store's env.
    fn env_call(&self, method: &str, args: Vec<Value>) -> Result<Value> {
        self.db.call("env", Some(&self.env_name), method, args)
    }

    fn env_req(&self, method: &str, args: Vec<Value>) -> BatchCall {
        BatchCall { scope: "env".into(), env: Some(self.env_name.clone()), method: method.into(), args }
    }

    pub fn set_text_source(&self, f: Option<TextSource>) {
        if let Ok(mut g) = self.text_source.write() {
            *g = f;
        }
    }

    /// load() as JSON in the JS shape: `(files: [(rel, row)], packages: [(name | null, row)])`. Rows keep
    /// every stored field verbatim (nulls included); `path`, `rel`, `symbols`, `imports`, `exports` are
    /// set the way store.js sets them.
    pub fn load_raw(&self) -> Result<(Vec<(String, Value)>, Vec<(Value, Value)>)> {
        let mut got = self.db.call_batch(false, vec![self.env_req("getRange", vec![json!(FILES), json!({})]), self.env_req("getRange", vec![json!(SYMBOLS), json!({})]), self.env_req("getRange", vec![json!(PACKAGES), json!({})])])?.into_iter();
        let mut entries = || -> Result<Vec<Value>> { Ok(got.next().unwrap_or(Ok(Value::Null))?.as_array().cloned().unwrap_or_default()) };
        let files: Vec<(String, Value)> = entries()?
            .into_iter()
            .filter_map(|e| Some((e.get("key")?.as_str()?.to_string(), e.get("value")?.clone())))
            .filter(|(_, v)| truthy(Some(v)))
            .collect();
        let symbols: Vec<Value> = entries()?.into_iter().filter_map(|e| e.get("value").cloned()).filter(|v| truthy(Some(v))).collect();
        let packages: Vec<Value> = entries()?.into_iter().filter_map(|e| e.get("value").cloned()).filter(|v| truthy(Some(v))).collect();
        let mut recs: IndexMap<String, Map<String, Value>> = IndexMap::new();
        for (rel, v) in files {
            let mut o = match v {
                Value::Object(o) => o,
                _ => Map::new(),
            };
            o.insert("path".into(), json!(rel));
            o.insert("rel".into(), json!(rel));
            o.insert("symbols".into(), json!([]));
            for k in ["imports", "exports"] {
                if !truthy(o.get(k)) {
                    o.insert(k.into(), json!([]));
                }
            }
            recs.insert(rel, o);
        }
        for s in symbols {
            if let Some(Value::Array(a)) = s.get("file").and_then(Value::as_str).and_then(|f| recs.get_mut(f)).and_then(|o| o.get_mut("symbols")) {
                a.push(s);
            }
        }
        for o in recs.values_mut() {
            if let Some(Value::Array(a)) = o.get_mut("symbols") {
                a.sort_by(|x, y| match (f64_of(x.get("lineStart")), f64_of(y.get("lineStart"))) {
                    (Some(p), Some(q)) => p.partial_cmp(&q).unwrap_or(std::cmp::Ordering::Equal),
                    _ => std::cmp::Ordering::Equal,
                });
            }
        }
        // JS keys the packages Map by name (last write wins, first position kept); a nameless one sits under `undefined`.
        let mut by_name: IndexMap<String, (Value, Value)> = IndexMap::new();
        for p in packages {
            let key = p.get("name").cloned().unwrap_or(Value::Null);
            let id = if p.get("name").is_some() { js_string(p.get("name")) } else { "\u{0}undefined".to_string() };
            by_name.insert(id, (key, p));
        }
        Ok((recs.into_iter().map(|(k, o)| (k, Value::Object(o))).collect(), by_name.into_values().collect()))
    }

    /// Everything known about the workspace, rehydrated into the shape the index works with in
    /// memory. Symbols arrive grouped by file.
    pub fn load(&self) -> Result<Loaded> {
        let (files, packages) = self.load_raw()?;
        Ok(Loaded {
            files: files.iter().map(|(rel, v)| FileRecord::from_json(rel, v)).collect(),
            packages: packages.into_iter().map(|(_, p)| p).collect(),
        })
    }

    /// Write-through for a batch of files, in ONE transaction. Fault-isolated: one transaction means
    /// one bad row aborts every file in it, so a failed batch is retried a file at a time, and only
    /// the file that cannot be written is left out (its old rows stay, and it comes back in `failed`).
    pub fn save_files(&self, saves: &[FileRecord], removals: &[String]) -> Result<SaveOutcome> {
        let rows: Vec<Value> = saves.iter().map(|f| f.to_json(false)).collect();
        self.save_files_json(&rows, removals)
    }

    /// saveFiles on JS-shaped records (what the binding hands over): nothing is lost to a typed round trip.
    pub fn save_files_json(&self, saves: &[Value], removals: &[String]) -> Result<SaveOutcome> {
        if saves.is_empty() && removals.is_empty() {
            return Ok(SaveOutcome::default());
        }
        let staged: Vec<Staged> = saves.iter().map(|f| self.stage(f)).collect();
        self.prefetch(&staged);
        let removals: Vec<String> = removals.to_vec();
        let (log, fts, content_fts, drains) = (self.log.clone(), self.fts, self.content_fts, self.drains);
        let rels: Vec<String> = staged.iter().map(|f| f.rel.clone()).collect();
        let outcome = (|| -> Result<SaveOutcome> {
            for f in &staged {
                for w in &f.warnings {
                    log(w);
                }
            }
            // stale symbol keys of every touched file, in one read group
            let stale_reqs: Vec<BatchCall> = removals
                .iter()
                .chain(staged.iter().map(|f| &f.rel))
                .map(|rel| self.env_req("query", vec![json!(SYMBOLS), json!({ "file": rel }), json!({ "index": ["file"], "prefix": [rel] })]))
                .collect();
            let stale: Vec<Vec<Value>> = self
                .db
                .call_batch(false, stale_reqs)?
                .into_iter()
                .map(|r| r.map(|v| v.as_array().map(|hits| hits.iter().filter_map(|h| h.get("key").cloned()).collect()).unwrap_or_default()))
                .collect::<Result<_>>()?;
            let (stale_r, stale_s) = stale.split_at(removals.len());
            let rm = |ty: &str, key: Value| json!({ "action": "remove", "args": [ty, key] });
            let put = |ty: &str, key: Value, v: &Value| json!({ "action": "put", "args": [ty, key, v] });
            // one item per FILE: its removes and puts commit atomically; a failing file fails alone
            let mut items: Vec<(String, bool, Option<String>, Option<BatchCall>)> = Vec::new(); // (rel, logged, bad, request)
            for (rel, st) in removals.iter().zip(stale_r) {
                let mut acts = vec![rm(FILES, json!(rel))];
                acts.extend(st.iter().map(|k| rm(SYMBOLS, k.clone())));
                items.push((rel.clone(), false, None, Some(self.env_req("commit", vec![Value::Array(acts)]))));
            }
            for (f, st) in staged.iter().zip(stale_s) {
                if let Some(m) = &f.bad {
                    items.push((f.rel.clone(), f.logged, Some(m.clone()), None));
                    continue;
                }
                // Stale symbol rows go first, so a symbol deleted from the source does not linger in the graph.
                let mut acts: Vec<Value> = st.iter().map(|k| rm(SYMBOLS, k.clone())).collect();
                acts.push(put(FILES, json!(f.rel), &f.row));
                acts.extend(f.symbols.iter().map(|(k, v)| put(SYMBOLS, json!(k), v)));
                items.push((f.rel.clone(), false, None, Some(self.env_req("commit", vec![Value::Array(acts)]))));
            }
            let reqs: Vec<BatchCall> = items.iter().filter_map(|i| i.3.clone()).collect();
            let mut results = self.db.call_batch(false, reqs)?.into_iter();
            let (mut written, mut failed) = (0usize, Vec::new());
            for (rel, logged, bad, req) in items {
                let r: std::result::Result<(), String> = match (bad, req) {
                    (Some(m), _) => Err(m),
                    (None, Some(_)) => results.next().unwrap_or(Ok(Value::Null)).map(|_| ()).map_err(|e| e.message),
                    (None, None) => Ok(()),
                };
                match r {
                    Ok(()) => written += 1,
                    Err(e) => {
                        let error: String = e.chars().take(300).collect();
                        // an unencodable row was already logged, synchronously, by lib/store.js (when it had a log)
                        if !logged {
                            log(&format!("[okcode] {rel}: not written to the store — {error}"));
                        }
                        failed.push((rel, error));
                    }
                }
            }
            // FTS indexing is asynchronous — waiting for BOTH indexes here is what makes find() read its
            // own writes, where this process drains them.
            if drains {
                if fts {
                    let _ = self.db.call("fts", None, "flush", vec![json!(SYMBOLS), json!(self.env_name)]);
                }
                if content_fts {
                    let _ = self.db.call("fts", None, "flush", vec![json!(FILES), json!(self.env_name)]);
                }
            }
            Ok(SaveOutcome { written, failed })
        })();
        if let (Ok(_), true, true) = (&outcome, self.drains && self.content_fts, self.profiles.lock().map_or(true, |p| p.is_empty())) {
            // indexed, and nothing else will want the text: let go of it
            if let Ok(mut g) = self.stash.lock() {
                for rel in &rels {
                    g.forget(rel);
                }
            }
        }
        outcome
    }

    /// Read, on THIS thread (a save never runs on the JS thread or the db owner), the text of every
    /// indexable file the workspace's text cache no longer holds, and stash it for the resolvers.
    fn prefetch(&self, staged: &[Staged]) {
        if !self.drains || (!self.content_fts && !self.has_profiles()) {
            return;
        }
        let source = self.text_source.read().ok().and_then(|g| g.clone());
        let mut need: Vec<(String, String)> = Vec::new();
        for f in staged {
            if f.row.get("indexed") == Some(&Value::Bool(false)) {
                continue;
            }
            let hash = f.row.get("hash").and_then(Value::as_str).unwrap_or("").to_string();
            if source.as_ref().and_then(|s| s(&f.rel, &hash)).is_some() {
                continue;
            }
            need.push((f.rel.clone(), hash));
        }
        if need.is_empty() {
            return;
        }
        let rels: Vec<String> = need.iter().map(|(r, _)| r.clone()).collect();
        let Ok(got) = self.access.read(&rels) else { return };
        let got: HashMap<String, Vec<u8>> = got.into_iter().collect();
        if let Ok(mut g) = self.stash.lock() {
            for (rel, hash) in need {
                if let Some(buf) = got.get(&rel) {
                    g.put(&rel, &hash, Arc::from(String::from_utf8_lossy(buf).as_ref()));
                }
            }
        }
    }

    /// The rows of one file as they are written (pure; the symbols' names are coerced to strings).
    /// Fields the record does not have stay absent, as `undefined` does in JS.
    fn stage(&self, file: &Value) -> Staged {
        let rel = indexed_text(file.get("rel"));
        if let Some(msg) = file.get("__unencodable").and_then(Value::as_str) {
            return Staged { rel, row: json!({ "indexed": false }), symbols: Vec::new(), warnings: Vec::new(), bad: Some(msg.to_string()), logged: file.get("__logged").and_then(Value::as_bool).unwrap_or(false) };
        }
        let mut row = Map::new();
        row.insert("rel".into(), json!(rel));
        for k in ["hash", "size", "mtime", "lang", "lines"] {
            if let Some(v) = file.get(k) {
                row.insert(k.into(), v.clone());
            }
        }
        row.insert("parsed".into(), json!(truthy(file.get("parsed"))));
        // false = opaque (binary, oversized, secret): hashed, never read — and the resolver refuses to read it too.
        row.insert("indexed".into(), json!(file.get("indexed") != Some(&Value::Bool(false))));
        let or = |k: &str, default: Value| match file.get(k) {
            Some(v) if truthy(Some(v)) => v.clone(),
            _ => default,
        };
        row.insert("reason".into(), or("reason", Value::Null));
        row.insert("imports".into(), or("imports", json!([])));
        row.insert("exports".into(), or("exports", json!([])));
        // Which version of an extension's analyser produced this file's SYMBOLS (re-ingest on mismatch).
        row.insert("analyzerVersion".into(), or("analyzerVersion", Value::Null));
        // Whether the text round-trips byte-for-byte as UTF-8 (a strict write refuses a lossy decode).
        row.insert("lossless".into(), json!(file.get("lossless") != Some(&Value::Bool(false))));
        // `call: true` reference sites: name + line only, kept small.
        let refs: Vec<Value> = match file.get("references") {
            Some(Value::Array(a)) => a
                .iter()
                .filter(|r| r.get("kind").and_then(Value::as_str) == Some("call"))
                .map(|r| {
                    let mut m = Map::new();
                    for k in ["name", "line"] {
                        if let Some(v) = r.get(k) {
                            m.insert(k.into(), v.clone());
                        }
                    }
                    Value::Object(m)
                })
                .collect(),
            _ => Vec::new(),
        };
        row.insert("references".into(), Value::Array(refs));
        let row = Value::Object(row);
        let mut warnings = Vec::new();
        let mut symbols = Vec::new();
        for s in file.get("symbols").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]) {
            let name = indexed_text(s.get("name"));
            if s.get("name").and_then(Value::as_str) != Some(name.as_str()) {
                warnings.push(format!("[okcode] {rel}: symbol name {} is not a string — stored as \"{name}\"", js_string(s.get("name"))));
            }
            let sym_path = if truthy(s.get("path")) { indexed_text(s.get("path")) } else { name.clone() };
            let line_start = s.get("lineStart");
            let int = |k: &str| match s.get(k) {
                Some(Value::Number(n)) if n.as_f64().map_or(false, |f| f.fract() == 0.0) => Value::Number(n.clone()),
                _ => Value::Null,
            };
            let key = sym_key(&rel, &sym_path, &js_string(line_start));
            let keep = |k: &str, default: Value| if truthy(s.get(k)) { s[k].clone() } else { default };
            let mut sym = Map::new();
            sym.insert("file".into(), json!(rel));
            sym.insert("name".into(), json!(name));
            for k in ["kind", "lineStart", "lineEnd"] {
                if let Some(v) = s.get(k) {
                    sym.insert(k.into(), v.clone());
                }
            }
            // Character offsets, so a symbol rehydrated after a restart reads and edits the same region.
            sym.insert("start".into(), int("start"));
            sym.insert("end".into(), int("end"));
            sym.insert("path".into(), json!(sym_path));
            sym.insert("parent".into(), keep("parent", Value::Null));
            sym.insert("signature".into(), keep("signature", Value::Null));
            sym.insert("doc".into(), keep("doc", json!("")));
            sym.insert("exported".into(), json!(truthy(s.get("exported"))));
            symbols.push((key, Value::Object(sym)));
        }
        Staged { rel, row, symbols, warnings, bad: None, logged: false }
    }

    /// The dependency surface. Separate from files and symbols because it has a different lifetime.
    pub fn save_packages(&self, list: &[Value]) -> Result<()> {
        if list.is_empty() {
            return Ok(());
        }
        let acts: Vec<Value> = list.iter().map(|p| json!({ "action": "put", "args": [PACKAGES, js_string(p.get("name")), p] })).collect();
        self.env_call("commit", vec![Value::Array(acts)]).map(|_| ())
    }

    fn search_opts(limit: usize, mode: &str) -> Value {
        json!({ "limit": limit.min(9_007_199_254_740_991), "mode": if mode == "or" { "or" } else { "and" } })
    }

    /// search(query, limit = 20, mode = 'and') → symbol rows with `relevance`; None = FTS unavailable.
    /// The MODE is the caller's, deliberately: the fallback belongs to whoever can see both answers.
    pub fn search_opt(&self, query: &str, limit: usize, mode: &str) -> Option<Vec<Value>> {
        if !self.fts {
            return None;
        }
        let docs = self.db.call("fts", None, "ftsQuery", vec![json!(SYMBOLS), json!(FTS_NAME), json!(query), json!({}), Self::search_opts(limit, mode), json!(self.env_name)]).ok()?;
        Some(
            docs.as_array()?
                .iter()
                .filter_map(|d| {
                    let mut v = d.get("value").filter(|v| !v.is_null())?.clone();
                    if let Value::Object(o) = &mut v {
                        o.insert("relevance".into(), json!(d.get("score").and_then(Value::as_f64).unwrap_or(0.0)));
                    }
                    Some(v)
                })
                .collect(),
        )
    }

    /// search(query, limit, mode): `[]` when the symbol index is unavailable (see `search_opt`).
    pub fn search(&self, query: &str, limit: usize, mode: &str) -> Result<Vec<Value>> {
        Ok(self.search_opt(query, limit, mode).unwrap_or_default())
    }

    /// searchContent(query, limit = 10, { mode = 'and' }) → `[{ file, hash, lang, relevance }]`;
    /// None = no content index. Quoting the matching lines needs the text, which the workspace has.
    pub fn search_content_opt(&self, query: &str, limit: usize, mode: &str) -> Option<Vec<Value>> {
        if !self.content_fts {
            return None;
        }
        let docs = self.db.call("fts", None, "ftsQuery", vec![json!(FILES), json!(FTS_NAME), json!(query), json!({}), Self::search_opts(limit, mode), json!(self.env_name)]).ok()?;
        Some(
            docs.as_array()?
                .iter()
                .filter_map(|d| {
                    let v = d.get("value").filter(|v| !v.is_null())?;
                    Some(json!({
                        "file": d.get("key").cloned().unwrap_or(Value::Null),
                        "hash": v.get("hash").cloned().unwrap_or(Value::Null),
                        "lang": v.get("lang").cloned().unwrap_or(Value::Null),
                        "relevance": d.get("score").and_then(Value::as_f64).unwrap_or(0.0),
                    }))
                })
                .collect(),
        )
    }

    /// searchContent(query, limit = 10, { mode = 'and' }): `[]` when there is no content index.
    pub fn search_content(&self, query: &str, limit: usize, mode: &str) -> Result<Vec<Value>> {
        Ok(self.search_content_opt(query, limit, mode).unwrap_or_default())
    }

    /// grep's ACCELERATOR, never its gate: the rels of every file the content index says holds all of
    /// `literal`'s indexable terms. None whenever that promise cannot be kept: no terms, no content
    /// index, or an index that has not caught up with the files type. Keys only.
    pub fn content_candidates(&self, literal: &str) -> Result<Option<HashSet<String>>> {
        Ok(self.content_candidates_ordered(literal)?.map(|v| v.into_iter().collect()))
    }

    /// `content_candidates` in okdb's key order, de-duplicated (the order a JS `Set` would iterate in).
    pub fn content_candidates_ordered(&self, literal: &str) -> Result<Option<Vec<String>>> {
        if !self.content_fts {
            return Ok(None);
        }
        let stop: Vec<String> = STOPWORDS.iter().map(|s| s.to_string()).collect();
        let terms = okcode_analysis::grep::index_terms(literal, &stop, 2, 64);
        if terms.is_empty() {
            return Ok(None);
        }
        let joined = terms.join(" ");
        let r = (|| -> Result<Option<Vec<String>>> {
            let list = self.db.call("fts", None, "list", vec![json!(FILES), json!(self.env_name)])?;
            let meta = list.as_array().and_then(|l| l.iter().find(|x| x.get("name").and_then(Value::as_str) == Some(FTS_NAME)).cloned());
            match meta {
                Some(m) if m.get("status").and_then(Value::as_str) == Some("ready") && m.get("lag").and_then(Value::as_f64) == Some(0.0) => {}
                _ => return Ok(None),
            }
            let opts = json!({ "limit": 9_007_199_254_740_991u64, "prefix": true, "keysOnly": true });
            let keys = self.db.call("fts", None, "search", vec![json!(FILES), json!(FTS_NAME), json!(joined), opts, json!(self.env_name)])?;
            Ok(Some(keys.as_array().map(|a| a.iter().filter_map(|k| k.as_str().map(String::from)).collect()).unwrap_or_default()))
        })();
        Ok(r.ok().flatten().map(|keys| {
            let mut seen = HashSet::new();
            keys.into_iter().filter(|k| seen.insert(k.clone())).collect()
        }))
    }

    // ── embeddings ─────────────────────────────────────────────────────────

    /// True when some profile is usable or pending (a passive process waiting for the pipeline).
    pub fn has_profiles(&self) -> bool {
        self.profiles.lock().map(|p| p.values().any(|p| p.error.is_none())).unwrap_or(false)
    }

    pub fn add_profile(&self, profile: Value) -> Result<Value> {
        Ok(self.ensure_profile(&profile)?.to_json())
    }

    fn ensure_profile(&self, profile: &Value) -> Result<ProfileState> {
        let name = match profile.get("name") {
            Some(n) if truthy(Some(n)) => js_string(Some(n)),
            _ => return Err(Error::msg("an embedding profile needs a name")),
        };
        let embedder = match profile.get("embedder") {
            Some(e) if e.is_object() => e.clone(),
            _ => return Err(Error::msg(format!("profile \"{name}\" needs an embedder config"))),
        };
        // The vector space this profile embeds into, minus the dims (the provider, not a derived factory
        // type); a bare store profile falls back to its embedder config.
        let parts = match profile.get("identity") {
            Some(i) if truthy(Some(i)) => Parts::from_json(i),
            _ => identity::parts_of(&embedder),
        };
        let model = embedder.get("model").filter(|m| truthy(Some(m))).or_else(|| embedder.get("type").filter(|t| truthy(Some(t)))).map(|m| js_string(Some(m)));
        let mut st = ProfileState {
            name: name.clone(),
            model,
            parts,
            dims: None,
            identity: None,
            pipeline: None,
            scoped: None,
            embedder_name: None,
            error: None,
            pending: None,
            spec: profile.clone(),
            gen: next_gen(),
        };
        self.put_profile(&st);
        self.attach(&mut st);
        self.put_profile_if_current(&st);
        Ok(st)
    }

    fn put_profile(&self, st: &ProfileState) {
        if let Ok(mut p) = self.profiles.lock() {
            p.insert(st.name.clone(), st.clone());
        }
    }

    /// Store `st` only while it is still the state `addProfile` made for its name.
    fn put_profile_if_current(&self, st: &ProfileState) {
        if let Ok(mut p) = self.profiles.lock() {
            if p.get(&st.name).map_or(false, |cur| cur.gen == st.gen) {
                p.insert(st.name.clone(), st.clone());
            }
        }
    }

    fn snapshot_profiles(&self) -> Vec<ProfileState> {
        self.profiles.lock().map(|p| p.values().cloned().collect()).unwrap_or_default()
    }

    /// One attempt to bring a profile up — create/repair where engines run, attach-only elsewhere.
    /// Concurrent callers for one profile share the in-flight attempt (no lock is held while it runs).
    fn attach(&self, st: &mut ProfileState) {
        let (attempt, leader) = {
            let mut m = self.attaching.lock().unwrap_or_else(|e| e.into_inner());
            match m.get(&(st.name.clone(), st.gen)) {
                Some(a) => (a.clone(), false),
                None => {
                    let a = Arc::new(Attempt { done: std::sync::Condvar::new(), outcome: Mutex::new(None) });
                    m.insert((st.name.clone(), st.gen), a.clone());
                    (a, true)
                }
            }
        };
        if !leader {
            let mut g = attempt.outcome.lock().unwrap_or_else(|e| e.into_inner());
            while g.is_none() {
                g = attempt.done.wait(g).unwrap_or_else(|e| e.into_inner());
            }
            if let Some(done) = g.clone() {
                *st = done;
            }
            return;
        }
        let mut work = st.clone();
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.attach_outcome(&mut work)));
        if ran.is_err() {
            work.error = Some("attaching the embedding profile panicked".into());
            work.pending = None;
        }
        *attempt.outcome.lock().unwrap_or_else(|e| e.into_inner()) = Some(work.clone());
        self.attaching.lock().unwrap_or_else(|e| e.into_inner()).remove(&(st.name.clone(), st.gen));
        attempt.done.notify_all();
        *st = work;
    }

    fn attach_outcome(&self, st: &mut ProfileState) {
        match self.attach_once(st) {
            Ok(Attach::Done) => {
                st.pending = None;
                st.error = None;
            }
            Ok(Attach::Wait(why)) => {
                st.pending = Some(why);
                st.error = None;
                st.pipeline = None;
                st.scoped = None;
            }
            Err(e) => {
                st.error = Some(e.message.clone());
                st.pending = None;
                self.log(&format!("[okcode] embedding profile {} unavailable: {}", st.name, e.message));
            }
        }
    }

    /// A pending profile (passive process) tries again: cheap — durable record reads, no model call.
    fn retry_pending(&self) {
        for mut st in self.snapshot_profiles().into_iter().filter(|p| p.pending.is_some()) {
            self.attach(&mut st);
            self.put_profile_if_current(&st);
        }
    }

    /// The okdb features the store uses for embeddings (embeddings, engines, pipelines) go
    /// through `DbHost::call(scope, env, method, args) -> JSON`. These are all
    /// the store's calls; env is Some only where listed; args/results are JSON, absent = null):
    ///
    /// - `engines.apiCall(type, name, method, args[]) -> {"value": v}` or `{"__absent": true}` when the
    ///   engine is missing or has no api/method (JS `engine.api?.[method]?.(...args)`). Used for the
    ///   embedder (`health`, `dims`) and the indexer (`stats`, `flush`).
    /// - `engines.getEngine(type, name) -> null | {"hasApi": bool, "isRunning": bool, "docEntry": {..}}`
    /// - `engines.engineStart(type, name)`, `engines.restart(type, name)`,
    ///   `engines.patchDeclaration(type, name, patch)` (patch e.g. `{"config": {..}}`)
    /// - `embeddings.resolveModelDims(type, model) -> int | null`
    /// - `embeddings.createEmbedder(name, spec, {}, envName)`, `embeddings.createPipeline(name, cfg)`
    /// - `embeddings.ensureTypeEnv(envName, type)` (JS `_ensureTypeEnv`)
    /// - `embeddings.durableStats(scoped)` (JS `_durableIndexerStats`)
    /// - `embeddings.search(scoped, query: string | number[], {"limit": n}) -> [{key, score, ..}]`
    ///   (JS `search(scoped).search(query, opts)`)
    /// - `embeddings.describeChunk(scoped, file, chunkHash, row | null)`; may fail `FIELD_RESOLVER_PENDING`
    /// - `pipelines.listRecords()`, `pipelines.get(name)` (-> `{engines: [..]}`), `pipelines.getRecord(name)`
    ///   (all three with `env` = the store's env name)
    ///
    /// A method the db does not offer MUST fail with `Error.code == "OKCODE_NOT_IMPLEMENTED"` (and only
    /// then): the store reads that as "feature absent" (JS feature guards). Any other error is a failure.
    fn emb(&self, scope: &str, env: Option<&str>, method: &str, args: Vec<Value>) -> Result<Value> {
        self.db.call(scope, env, method, args)
    }

    /// The dimension a previous run already committed to, read off the pipeline record it left behind.
    fn dims_from_records(&self, parts: &Parts) -> Option<Value> {
        let recs = self.emb("pipelines", Some(&self.env_name), "listRecords", vec![]).ok()?;
        let names: Vec<String> = recs
            .as_array()?
            .iter()
            .map(|r| {
                let n = r.get("value").and_then(|v| v.get("name")).filter(|n| truthy(Some(n)));
                match n.or_else(|| r.get("key").filter(|k| truthy(Some(k)))) {
                    Some(n) => js_string(Some(n)),
                    None => String::new(),
                }
            })
            .collect();
        identity::find_pipeline(&names, parts, &Value::Null).map(|(_, d)| json!(d))
    }

    /// The member engines (type@name) a pipeline record names that have no engine record.
    fn missing_members(&self, pipeline: &str) -> Vec<String> {
        let Ok(view) = self.emb("pipelines", Some(&self.env_name), "get", vec![json!(pipeline)]) else { return Vec::new() };
        view.get("engines")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter(|e| e.get("exists") == Some(&Value::Bool(false)))
                    .map(|e| format!("{}@{}", js_string(e.get("type")), js_string(e.get("name"))))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// `engine.api.<method>(args)` through the engines facade: Some(value) or None when the engine has no api.
    fn api_call(&self, ty: &str, name: &str, method: &str, args: Vec<Value>) -> Result<Option<Value>> {
        let r = match self.emb("engines", None, "apiCall", vec![json!(ty), json!(name), json!(method), Value::Array(args)]) {
            // a passive process (engines role off): no live engine here, as JS's getRunningEngine() → null
            Err(e) if e.code.as_deref() == Some("ENGINE_SYSTEM_NOT_STARTED") => return Ok(None),
            r => r?,
        };
        if r.get("__absent").is_some() {
            return Ok(None);
        }
        Ok(Some(r.get("value").cloned().unwrap_or(Value::Null)))
    }

    fn get_engine(&self, ty: &str, name: &str) -> Option<Value> {
        self.emb("engines", None, "getEngine", vec![json!(ty), json!(name)]).ok().filter(|v| !v.is_null())
    }

    fn attach_once(&self, st: &mut ProfileState) -> Result<Attach> {
        let profile = st.spec.clone();
        let parts = st.parts.clone();
        // A host's own embed function is told WHICH workspace it embeds for: the id rides in the embedder
        // config — the one thing okdb hands a factory — which is not part of the vector space's fingerprint.
        let custom = parts.r#type == "custom";
        let mut embedder = profile["embedder"].clone();
        if custom {
            if let Value::Object(o) = &mut embedder {
                o.insert("workspace".into(), json!(self.id));
            }
        }
        let creates = self.creates;
        let env_name = &self.env_name;

        let mut dims: Option<Value> = [profile.get("dims"), embedder.get("dims")].into_iter().flatten().find(|d| truthy(Some(d))).cloned();
        if dims.is_none() {
            let model = if parts.model.is_empty() { embedder.get("model").cloned().unwrap_or(Value::Null) } else { json!(parts.model) };
            dims = match self.emb("embeddings", None, "resolveModelDims", vec![json!(parts.r#type), model]) {
                Ok(d) => Some(d).filter(|d| truthy(Some(d))),
                Err(e) if not_implemented(&e) => None,
                Err(e) => return Err(e),
            };
        }
        if dims.is_none() {
            dims = self.dims_from_records(&parts);
        }
        let mut embedder_ref: Option<Value> = None;
        if dims.is_none() {
            // A passive process must not probe the model (that starts an engine): the dims arrive with
            // the indexing process's record.
            if !creates {
                return Ok(Attach::Wait("waiting for an indexing process to create the pipeline".into()));
            }
            // Dimensionality is the model's to state, not ours to assume. Start the embedder on its own
            // (probing it) and hand the same engine to the pipeline. Named after the space too: a url
            // change must probe the NEW endpoint, not reuse an engine still pointed at the old one.
            let space = identity::short_hash(&serde_json::to_string(&json!([parts.r#type, parts.endpoint, parts.model]))?);
            let emb_name = format!("{env_name}:emb_{}_{space}", identity::slug(&st.name));
            let engine = match self.get_engine("embedder", &emb_name) {
                Some(e) => e,
                None => self.emb("embeddings", None, "createEmbedder", vec![json!(emb_name), embedder.clone(), json!({}), json!(env_name)])?,
            };
            let has_api = engine.get("hasApi") != Some(&Value::Bool(false));
            let health = if has_api { self.api_call("embedder", &emb_name, "health", vec![])? } else { None };
            let api_dims = if has_api { self.api_call("embedder", &emb_name, "dims", vec![]).ok().flatten() } else { None };
            dims = api_dims.filter(|d| truthy(Some(d))).or_else(|| health.as_ref().and_then(|h| h.get("dims")).filter(|d| truthy(Some(d))).cloned());
            if dims.is_none() {
                return Err(Error::msg(format!("could not learn the dimensions of {} — pass dims", st.model.clone().unwrap_or_else(|| "null".into()))));
            }
            embedder_ref = Some(json!({ "name": emb_name }));
        }
        let dims = dims.unwrap();
        let pipeline = identity::pipeline_name(&parts, &dims).map_err(Error::msg)?;
        let scoped = format!("{env_name}:{pipeline}");
        st.dims = Some(dims.clone());
        st.pipeline = Some(pipeline.clone());
        st.scoped = Some(scoped);
        st.identity = identity::identity_of(&parts, &dims);

        let rec_of = || -> Result<Option<Value>> { Ok(Some(self.emb("pipelines", Some(env_name), "getRecord", vec![json!(pipeline)])?).filter(|r| !r.is_null())) };
        let mut existing = rec_of()?;
        if existing.is_none() && !creates {
            return Ok(Attach::Wait("waiting for an indexing process to create the pipeline".into()));
        }
        // A record is not proof of a working pipeline: one whose member engines are gone never indexes.
        // Where engines run, hand it back to createPipeline, which re-creates the missing members.
        if existing.is_some() && creates {
            let missing = self.missing_members(&pipeline);
            if !missing.is_empty() {
                self.log(&format!("[okcode] pipeline {pipeline} is missing {} — repairing it", missing.join(", ")));
                existing = None;
            }
        }
        if existing.is_none() {
            let mut cfg = profile.get("pipeline").and_then(Value::as_object).cloned().unwrap_or_default();
            cfg.insert("source_type".into(), json!(FILES));
            cfg.insert("source_env".into(), json!(env_name));
            cfg.insert("field".into(), json!(PREPARED));
            cfg.insert("chunk".into(), json!({ "strategy": chunkmod::STRATEGY, "size": chunkmod::CHUNK }));
            cfg.insert("dims".into(), dims.clone());
            let emb_cfg = match embedder_ref {
                Some(r) => r,
                None => {
                    let mut e = embedder.as_object().cloned().unwrap_or_default();
                    e.insert("dims".into(), dims.clone());
                    Value::Object(e)
                }
            };
            cfg.insert("embedder".into(), emb_cfg);
            self.emb("embeddings", None, "createPipeline", vec![json!(pipeline), Value::Object(cfg)])?;
        }
        let rec = rec_of()?;
        st.embedder_name = rec
            .as_ref()
            .and_then(|r| r.get("engines"))
            .and_then(Value::as_array)
            .and_then(|a| a.iter().find(|e| e.get("role").and_then(Value::as_str) == Some("embedder")))
            .and_then(|e| e.get("name"))
            .map(|n| js_string(Some(n)));
        // The embedder is a lifecycle-skipped member of the pipeline record, so bringing a pipeline back
        // up may start the indexer but not the thing it embeds with. Start it explicitly wherever engines run.
        if creates {
            if let Some(en) = st.embedder_name.clone() {
                if let Some(eng) = self.get_engine("embedder", &en) {
                    // A pipeline created before the embedder config carried its workspace: patched in, then
                    // restarted so the running instance has it. The fingerprint does not move.
                    let cfg = eng.get("docEntry").and_then(|d| d.get("value")).and_then(|v| v.get("config")).cloned();
                    if let (true, Some(Value::Object(cfg))) = (custom, cfg) {
                        if cfg.get("workspace").and_then(Value::as_str) != Some(self.id.as_str()) {
                            let mut c = cfg.clone();
                            c.insert("workspace".into(), json!(self.id));
                            let patched = self.emb("engines", None, "patchDeclaration", vec![json!("embedder"), json!(en), json!({ "config": c })]);
                            match patched.and_then(|_| self.emb("engines", None, "restart", vec![json!("embedder"), json!(en)])) {
                                Ok(_) => {}
                                Err(err) => self.log(&format!("[okcode] could not name workspace {} on embedder {en}: {}", self.id, err.message)),
                            }
                        }
                    }
                    if let Some(eng) = self.get_engine("embedder", &en) {
                        if eng.get("isRunning") != Some(&Value::Bool(true)) {
                            let _ = self.emb("engines", None, "engineStart", vec![json!("embedder"), json!(en)]);
                        }
                    }
                }
            }
        }
        // okdb reads a pipeline's durable doc status from its vector sub-env, which a process opens only
        // for envs it saw at boot or touched since.
        if !creates {
            let _ = self.emb("embeddings", None, "ensureTypeEnv", vec![json!(env_name), json!(FILES)]);
        }
        Ok(Attach::Done)
    }

    fn profile_for(&self, name: Option<&str>) -> Option<ProfileState> {
        let p = self.profiles.lock().ok()?;
        if p.is_empty() {
            return None;
        }
        match name {
            None => p.values().find(|p| p.error.is_none() && p.pending.is_none()).or_else(|| p.values().find(|p| p.error.is_none())).cloned(),
            Some(n) => p.get(n).cloned(),
        }
    }

    /// Per profile: name, model, pipeline, dims, and the indexer's own progress (live where this
    /// process runs it, durable otherwise).
    pub fn profiles(&self) -> Result<Vec<Value>> {
        self.retry_pending();
        let mut out = Vec::new();
        for p in self.snapshot_profiles() {
            let mut status = Value::Null;
            if let Some(scoped) = &p.scoped {
                status = match self.api_call("indexer", scoped, "stats", vec![]) {
                    Ok(Some(v)) => v,
                    Ok(None) => match self.emb("embeddings", None, "durableStats", vec![json!(scoped)]) {
                        Ok(v) => v,
                        Err(e) if not_implemented(&e) => Value::Null,
                        Err(e) => json!({ "error": e.message }),
                    },
                    Err(err) => json!({ "error": err.message }),
                };
            }
            let nz = |s: &str| if s.is_empty() { Value::Null } else { json!(s) };
            out.push(json!({
                "name": p.name,
                "model": p.model,
                "type": nz(&p.parts.r#type),
                "endpoint": nz(&p.parts.endpoint),
                "pipeline": p.pipeline,
                "dims": p.dims,
                "identity": p.identity,
                "error": p.error,
                "pending": p.pending,
                "status": status,
            }));
        }
        Ok(out)
    }

    /// Wait for a profile's indexer to drain what it has been handed. For a test, a CLI run or a
    /// deliberate rebuild — never a read path.
    pub fn settle(&self, profile: Option<&str>) -> Result<Value> {
        let list: Vec<ProfileState> = match profile {
            Some(n) => self.profile_for(Some(n)).into_iter().collect(),
            None => self.snapshot_profiles(),
        };
        for p in list {
            if let Some(scoped) = &p.scoped {
                // an indexer that is not running here has nothing to flush
                self.api_call("indexer", scoped, "flush", vec![])?;
            }
        }
        Ok(Value::Null)
    }

    /// Semantic search in one profile's pipeline. Returns FILE locations
    /// `[{ file, hash, score, chunkHash, start, end }]` with start/end as offsets into the file's text
    /// as it was when embedded (the chunk manifest's offsets, minus the path stamp). With `text`,
    /// each hit also carries the chunk's text.
    pub fn ask(&self, query: &Query, o: &AskOptions) -> Result<Value> {
        self.retry_pending();
        let p = match self.profile_for(o.profile.as_deref()) {
            Some(p) => p,
            None => {
                return Err(Error::code(
                    "OKCODE_NO_EMBEDDINGS",
                    match &o.profile {
                        None => "no embedding profile is configured for this workspace — ask() is unavailable".to_string(),
                        Some(n) => format!("no embedding profile named \"{n}\""),
                    },
                ))
            }
        };
        if let Some(e) = &p.error {
            return Err(Error::code("OKCODE_NO_EMBEDDINGS", format!("embedding profile \"{}\" is unavailable: {e}", p.name)));
        }
        if let Some(w) = &p.pending {
            // Not an error in the profile — the vectors are not there YET. Same code, so a caller's "no
            // semantic eye" fallback covers it; `pending` tells the two apart.
            return Err(Error::code("OKCODE_NO_EMBEDDINGS", format!("embedding profile \"{}\" is pending: {w}", p.name)).with("pending", Value::Bool(true)));
        }
        if let Some(v) = &query.vector {
            let dims = p.dims.as_ref().and_then(identity::dims_of);
            if Some(v.len() as u64) != dims {
                return Err(Error::code(
                    "OKCODE_DIMS_MISMATCH",
                    format!("embedding profile \"{}\" holds {}-dim vectors; the query vector has {}", p.name, p.dims.as_ref().map_or("null".into(), |d| js_string(Some(d))), v.len()),
                ));
            }
            if let Some(id) = query.identity.as_ref().filter(|i| !i.is_empty()) {
                if Some(id) != p.identity.as_ref() {
                    return Err(Error::code(
                        "OKCODE_IDENTITY_MISMATCH",
                        format!("embedding profile \"{}\" is {}; the query vector is from {id}", p.name, p.identity.clone().unwrap_or_else(|| "null".into())),
                    ));
                }
            }
        }
        let scoped = p.scoped.clone().unwrap_or_default();
        let q = match (&query.vector, &query.text) {
            (Some(v), _) => Value::Array(v.iter().map(|x| json!(*x as f64)).collect()),
            (None, t) => json!(t),
        };
        let raw = self.emb("embeddings", None, "search", vec![json!(scoped), q, json!({ "limit": o.limit.saturating_mul(3) })])?;
        // Best chunk per file.
        let mut best: IndexMap<String, (Option<String>, f64)> = IndexMap::new();
        for r in raw.as_array().cloned().unwrap_or_default() {
            let key = if truthy(r.get("key")) { js_string(r.get("key")) } else { String::new() };
            let mut it = key.split('\t');
            let file = it.next().unwrap_or("").to_string();
            let chunk_hash = it.next().filter(|h| !h.is_empty()).map(String::from);
            if file.is_empty() {
                continue;
            }
            let score = f64_of(r.get("score")).unwrap_or(f64::NAN);
            if let Some(prior) = best.get(&file) {
                if prior.1 >= score {
                    continue;
                }
            }
            best.insert(file, (chunk_hash, score));
        }
        let mut ranked: Vec<(String, Option<String>, f64)> = best.into_iter().map(|(f, (h, s))| (f, h, s)).collect();
        ranked.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        ranked.truncate(o.limit);
        let files: Vec<String> = ranked.iter().map(|r| r.0.clone()).collect();
        let rows: Vec<Option<Value>> = self
            .db
            .call_batch(false, files.iter().map(|f| self.env_req("get", vec![json!(FILES), json!(f)])).collect())?
            .into_iter()
            .map(|r| r.map(|v| Some(v).filter(|v| !v.is_null())))
            .collect::<Result<_>>()?;
        // Every describeChunk in ONE owner hop (rows the file left the graph for are skipped).
        let live: Vec<usize> = (0..ranked.len()).filter(|i| rows[*i].is_some()).collect();
        let calls: Vec<crate::db::BatchCall> = live
            .iter()
            .map(|&i| {
                let (file, chunk_hash, _) = &ranked[i];
                crate::db::BatchCall {
                    scope: "embeddings".into(),
                    env: None,
                    method: "describeChunk".into(),
                    args: vec![json!(scoped), json!(file), json!(chunk_hash), if o.text { rows[i].clone().unwrap() } else { Value::Null }],
                }
            })
            .collect();
        let metas: Vec<Option<Value>> = match self.db.call_batch(false, calls) {
            Ok(rs) => rs.into_iter().map(|r| r.ok().filter(|m| !m.is_null())).collect(),
            Err(_) => vec![None; live.len()],
        };
        let mut metas = metas.into_iter();
        let mut out = Vec::new();
        for ((file, chunk_hash, score), row) in ranked.into_iter().zip(rows) {
            let Some(row) = row else { continue }; // the file left the graph since it was embedded
            let meta = metas.next().flatten();
            // No chunk record: the vector is orphaned (the file was re-chunked since). Drop it.
            let Some(meta) = meta else { continue };
            let off = chunkmod::mark_length(&file) as f64;
            let rel_off = |k: &str| match meta.get(k).and_then(Value::as_f64) {
                Some(n) => json!((n - off).max(0.0)),
                None => Value::Null,
            };
            let mut hit = Map::new();
            hit.insert("file".into(), json!(file));
            hit.insert("hash".into(), row.get("hash").cloned().unwrap_or(Value::Null));
            hit.insert("score".into(), json!(score));
            hit.insert("chunkHash".into(), json!(chunk_hash));
            hit.insert("start".into(), rel_off("start"));
            hit.insert("end".into(), rel_off("end"));
            if o.text {
                if let Some(t) = meta.get("text").filter(|t| t.is_string()) {
                    hit.insert("text".into(), t.clone());
                }
            }
            out.push(Value::Object(hit));
        }
        Ok(Value::Array(out))
    }

    /// drop(): removeEnvironment(envName) — rows, full-text indexes, pipelines and their vectors.
    /// The store is unusable afterwards.
    pub fn drop_env(&self) -> Result<()> {
        self.drop_env_result().map(|_| ())
    }

    fn drop_env_result(&self) -> Result<Value> {
        self.db.call("db", None, "removeEnvironment", vec![json!(self.env_name)])
    }
}

// ── JS dispatch (the binding's store wrapper calls these) ─────────────────

impl Store {
    /// Every store.js API method by name with JSON args → JSON result (load/saveFiles/… shapes as
    /// in JS: load → `{ files: [[rel, record]…], packages: [[name, pkg]…] }`, the JS layer rebuilds Maps;
    /// search/searchContent/contentCandidates → an array or null; vectors travel as number arrays).
    pub fn call(&self, method: &str, args: &[Value]) -> Result<Value> {
        let a = |i: usize| args.get(i).unwrap_or(&Value::Null);
        let limit = |i: usize, default: usize| match a(i) {
            Value::Null => default,
            v => clamp_count(js_to_number(v)),
        };
        match method {
            "load" => {
                let (files, packages) = self.load_raw()?;
                let files: Vec<Value> = files.into_iter().map(|(rel, row)| json!([rel, row])).collect();
                let packages: Vec<Value> = packages.into_iter().map(|(k, p)| json!([k, p])).collect();
                Ok(json!({ "files": files, "packages": packages }))
            }
            "saveFiles" => {
                let saves: &[Value] = a(0).as_array().map(Vec::as_slice).unwrap_or(&[]);
                let removals: Vec<String> = a(1).as_array().map(|r| r.iter().map(|x| js_string(Some(x))).collect()).unwrap_or_default();
                let out = self.save_files_json(saves, &removals)?;
                Ok(json!({
                    "written": out.written,
                    "failed": out.failed.iter().map(|(rel, error)| json!({"rel": rel, "error": error})).collect::<Vec<_>>(),
                }))
            }
            "savePackages" => {
                self.save_packages(a(0).as_array().map(Vec::as_slice).unwrap_or(&[]))?;
                Ok(Value::Null)
            }
            "search" => {
                let mode = a(2).as_str().unwrap_or("and").to_string();
                Ok(self.search_opt(&js_string(Some(a(0))), limit(1, 20), &mode).map_or(Value::Null, Value::Array))
            }
            "searchContent" => {
                let mode = a(2).get("mode").and_then(Value::as_str).unwrap_or("and").to_string();
                Ok(self.search_content_opt(&js_string(Some(a(0))), limit(1, 10), &mode).map_or(Value::Null, Value::Array))
            }
            "contentCandidates" => Ok(self.content_candidates_ordered(a(0).as_str().unwrap_or(""))?.map_or(Value::Null, |s| json!(s))),
            "hasProfiles" => Ok(json!(self.has_profiles())),
            "addProfile" => self.add_profile(a(0).clone()),
            "profiles" => Ok(Value::Array(self.profiles()?)),
            "settle" => self.settle(a(0).get("profile").and_then(Value::as_str).filter(|p| !p.is_empty())),
            "ask" => {
                let q = as_query(a(0))?;
                let o = a(1);
                let opts = AskOptions {
                    profile: o.get("profile").and_then(Value::as_str).map(String::from),
                    limit: match o.get("limit") {
                        Some(v) if !v.is_null() => clamp_count(js_to_number(v)),
                        _ => 8,
                    },
                    text: truthy(o.get("text")),
                };
                self.ask(&q, &opts)
            }
            "drop" => self.drop_env_result(),
            "setTextSource" => Ok(Value::Null), // NOT PORTED: a JS-supplied text source cannot be called from the owner thread without blocking on JS; the native workspace binds its own text cache
            other => Err(Error::msg(format!("store has no method {other}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_names() {
        assert_eq!(env_name_for("abc").unwrap(), "okcode_abc");
        let n = env_name_for("App").unwrap();
        assert!(n.starts_with("okcode_app_") && n.len() == "okcode_app_".len() + 8, "{n}");
        assert_ne!(env_name_for("a.b").unwrap(), env_name_for("a_b").unwrap());
        assert!(env_name_for("").is_err());
        assert!(env_name_for("!!!").unwrap().starts_with("okcode_ws_"));
    }

    #[test]
    fn terms_and_quotes() {
        assert_eq!(query_terms("Where is the budget_computed? a"), vec!["budget_computed"]);
        let q = quote_lines("foo bar\n  nothing\nbar baz foo\n", "foo bar", 3);
        assert_eq!(q.iter().map(|x| (x.line, x.n)).collect::<Vec<_>>(), vec![(1, 2), (3, 2)]);
        assert_eq!(q[0].text, "foo bar");
    }

    #[test]
    fn queries() {
        assert!(as_query(&json!(5)).is_err());
        assert_eq!(as_query(&json!("hi")).unwrap().text.as_deref(), Some("hi"));
        assert!(as_query(&json!({"vector": []})).is_err());
        assert!(as_query(&json!({"text": "  "})).is_err());
        assert_eq!(as_query(&json!({"vector": [1, 2]})).unwrap().vector.unwrap(), vec![1.0, 2.0]);
        assert_eq!(as_query(&json!({"text": "x", "vector": 3})).unwrap_err().code.as_deref(), Some("OKCODE_BAD_QUERY"));
    }

    #[test]
    fn indexed() {
        assert_eq!(indexed_text(Some(&json!(12))), "12");
        assert_eq!(indexed_text(None), "");
        assert_eq!(indexed_text(Some(&json!({"$re": "a+", "flags": "g"}))), "/a+/g");
        assert_eq!(sym_key("a.js", "f", "3"), "a.js::f::3");
    }
}
