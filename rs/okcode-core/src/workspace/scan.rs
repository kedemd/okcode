//! Scan, freshness, cache and flush. OWNER: slice `perf` (wave 3; wave 2: `scan`).
//!
//! JS (src/workspace.js): 123-178 (sha1, OWN_TRANSIENT, DOT_PRUNE_RE, compileGitignore),
//! 433-1020 (ingestOne/drop via state.rs, readMany, ensureTexts, verify, resultSetCheck, movedOf,
//! flagMoved, listTree, fullScan, refresh, ensureOpen, doFlush, flush, scheduleFlush),
//! api: refresh, sync, flush, scanWarnings, stats, close (2377-2387, 4053-4093).
//!
//! Concurrency: the API is sync. `refresh` serialises walks with a mutex + condvar (JS: the shared
//! `scanning` promise). Persistence runs on one flusher thread per workspace fed by a FIFO channel
//! (JS: `flushChain`); the thread owns only `Arc<Store>` and the batch it was handed, so it never
//! touches the index. Outcomes of background batches are parked in `FlushShared::results` and applied
//! to the index (flushing / storeErrors) by the next scan-slice entry point (`settle_flushes`).
use super::state::{prepare_ingest, PendingOp};
use super::Workspace;
use crate::error::{Error, Result};
use crate::record::FileRecord;
use crate::store::{SaveOutcome, Store};
use okcode_analysis::access::{ListOpts, StatRow, PRUNE_DOT_DIRS};
use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};

// ── pure helpers ────────────────────────────────────────────────────────

/// okcode's own transient files never become part of the graph.
fn own_transient() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(^|/)\.okcode-tmp-|\.okcode-lock$").unwrap())
}

/// DOT_PRUNE_RE: the facades' fixed non-source dot-directory list, anywhere above a file.
fn dot_prune() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        let alt: Vec<String> = PRUNE_DOT_DIRS.iter().map(|d| d.replace('.', "\\.")).collect();
        Regex::new(&format!("(^|/)({})/", alt.join("|"))).unwrap()
    })
}

/// The bounded parse pool: OKCODE_PARSE_THREADS, default min(4, available_parallelism).
fn parse_pool() -> &'static rayon::ThreadPool {
    static P: OnceLock<rayon::ThreadPool> = OnceLock::new();
    P.get_or_init(|| {
        let n = std::env::var("OKCODE_PARSE_THREADS")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or_else(|| std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(4));
        let n = n.min(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).max(4)).min(64);
        rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            // QuickJS may use 4 MiB (rt.set_max_stack_size) and the extractors recurse: match the old
            // libuv threads (~8 MiB) or more, never rayon's 2 MiB default.
            .stack_size(16 << 20)
            .thread_name(|i| format!("okcode-parse-{i}"))
            .build()
            .or_else(|_| rayon::ThreadPoolBuilder::new().num_threads(1).stack_size(16 << 20).build())
            .expect("parse pool")
    })
}

fn now_ms() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as f64).unwrap_or(0.0)
}

/// `String(err.message).slice(0, 300)`
fn clip(msg: &str) -> String {
    msg.chars().take(300).collect()
}

/// compileGitignore(text): the pragmatic gitignore subset (no `!`, no `**`, no classes).
pub struct Gitignore {
    matchers: Vec<Regex>,
}

impl Gitignore {
    pub fn compile(text: &str) -> Gitignore {
        let mut matchers = Vec::new();
        for line in text.split('\n') {
            let l = line.trim_matches(|c: char| c.is_whitespace() || c == '\u{FEFF}');
            if l.is_empty() || l.starts_with('#') || l.starts_with('!') {
                continue;
            }
            let body = l.strip_suffix('/').unwrap_or(l);
            let anchored = body.contains('/');
            let stripped = body.strip_prefix('/').unwrap_or(body);
            let mut part = String::new();
            for c in stripped.chars() {
                match c {
                    '.' | '+' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']' | '\\' => {
                        part.push('\\');
                        part.push(c);
                    }
                    '*' => part.push_str("[^/]*"),
                    _ => part.push(c),
                }
            }
            let src = if anchored { format!("^{part}(?:/|$)") } else { format!("(?:^|/){part}(?:/|$)") };
            // DELIBERATE divergence: JS `new RegExp` would throw and fail the whole scan; a pattern the
            // engine cannot express is simply not excluded here.
            if let Ok(re) = Regex::new(&src) {
                matchers.push(re);
            }
        }
        Gitignore { matchers }
    }
    pub fn matches(&self, rel: &str) -> bool {
        self.matchers.iter().any(|re| re.is_match(rel))
    }
}

// ── slice state ─────────────────────────────────────────────────────────

struct Batch {
    saves: Vec<FileRecord>,
    removals: Vec<String>,
    packages: Vec<Value>,
    done: Option<mpsc::Sender<Settled>>,
    /// order in which batches were gathered
    seq: u64,
    /// flusher-side request: gather what is pending now (coalesced scheduleFlush)
    wake: bool,
}

/// What a store write came to, to be applied to the index by whoever drains it.
struct Settled {
    /// saves' rels then removals (JS `batch`)
    rels: Vec<String>,
    n_saves: usize,
    n_removals: usize,
    outcome: Option<SaveOutcome>,
    seq: u64,
}

#[derive(Default)]
struct FlushShared {
    results: Mutex<Vec<Settled>>,
    seq: AtomicU64,
    /// a scheduleFlush wake-up is queued (JS `flushTimer`)
    requested: AtomicBool,
}

#[derive(Default)]
struct ScanFlags {
    /// the open-time walk has been started (JS `opened`)
    opened: bool,
    /// a walk is running (JS `scanning`)
    scanning: bool,
}

/// Slice-owned state: the in-flight scan, `opened`, the flusher thread.
#[derive(Default)]
pub struct ScanState {
    flags: Mutex<ScanFlags>,
    cv: Condvar,
    flush: Arc<FlushShared>,
    flusher: Mutex<Option<mpsc::Sender<Batch>>>,
    /// cache of analyzerVersionFor per path shape
    versions: Mutex<HashMap<String, Option<String>>>,
    /// Weak handle to the owning workspace, set once by `Workspace::open` (architect-owned mod.rs).
    /// Without it warm_up and coalesced scheduleFlush degrade (no background walk; immediate gather).
    pub weak: Arc<OnceLock<Weak<Workspace>>>,
}

/// Clears `scanning` (and `opened` on failure) however the walk ends.
struct ScanGuard<'a> {
    ws: &'a Workspace,
    ok: bool,
}
impl Drop for ScanGuard<'_> {
    fn drop(&mut self) {
        let mut f = self.ws.scan.flags.lock().unwrap_or_else(|e| e.into_inner());
        f.scanning = false;
        if !self.ok {
            f.opened = false; // the next use tries again
        }
        self.ws.scan.cv.notify_all();
    }
}

fn flusher_loop(store: Arc<Store>, shared: Arc<FlushShared>, ws: Arc<OnceLock<Weak<Workspace>>>, rx: mpsc::Receiver<Batch>) {
    for b in rx {
        if b.wake {
            // one transaction per burst: clear the flag first so later schedules queue a new wake-up
            shared.requested.store(false, Ordering::SeqCst);
            let Some(w) = ws.get().and_then(|w| w.upgrade()) else { continue };
            let Some(nb) = w.gather_batch(None) else { continue };
            drop(w);
            flush_one(&store, &shared, nb);
        } else {
            flush_one(&store, &shared, b);
        }
    }
}

fn flush_one(store: &Arc<Store>, shared: &Arc<FlushShared>, b: Batch) {
    let n_saves = b.saves.len();
    let n_removals = b.removals.len();
    let rels: Vec<String> = b.saves.iter().map(|f| f.rel.clone()).chain(b.removals.iter().cloned()).collect();
    if !b.packages.is_empty() {
        // ordered with saveFiles on this FIFO, off the read path (JS: inside doFlush)
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| store.save_packages(&b.packages)));
    }
    let outcome = if n_saves + n_removals == 0 {
        None
    } else {
        // The store is a cache; a failed (or panicking) write is a cold start, not a crash.
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| store.save_files(&b.saves, &b.removals))).ok().and_then(|r| r.ok())
    };
    let s = Settled { rels, n_saves, n_removals, outcome, seq: b.seq };
    match b.done {
        Some(tx) => {
            let _ = tx.send(s);
        }
        None => shared.results.lock().unwrap_or_else(|e| e.into_inner()).push(s),
    }
}

// ── option / result types ───────────────────────────────────────────────

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RefreshOpts {
    pub force: bool,
    pub rewrite: bool,
    pub sync: bool,
    pub rehash: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SyncOpts {
    pub force: bool,
}

/// verify() → `{ checked, changed, gone, unverified? }`
#[derive(Clone, Debug, Default)]
pub struct VerifyOut {
    pub checked: usize,
    pub changed: Vec<String>,
    pub gone: Vec<String>,
    /// the facade could not be reached (best-effort mode)
    pub unverified: bool,
}

/// A facade list row (path, size, mtime).
#[derive(Clone, Debug)]
pub struct Row {
    pub path: String,
    pub size: u64,
    pub mtime: Option<f64>,
    pub hash: Option<String>,
}

/// What fullScan needs to remember about the row this index already holds.
struct Prior {
    size: u64,
    mtime: Option<f64>,
    hash: Option<String>,
    indexed: bool,
    analyzer_version: Option<String>,
}

impl Workspace {
    // ── small helpers ────────────────────────────────────────────────────

    fn stat_map(&self, paths: &[String], hash: bool) -> std::result::Result<HashMap<String, StatRow>, String> {
        Ok(self.access.stat(paths, hash)?.into_iter().collect())
    }

    /// analyzerVersionFor(path), cached by path shape (the part after the first dot of the basename).
    fn analyzer_version(&self, path: &str) -> Option<String> {
        let base = path.rsplit('/').next().unwrap_or(path);
        let key = base.find('.').map(|i| base[i..].to_ascii_lowercase());
        if let Some(k) = &key {
            if let Some(v) = self.scan.versions.lock().unwrap_or_else(|e| e.into_inner()).get(k) {
                return v.clone();
            }
        }
        let v = okcode_analysis::analyzer_version_for(path);
        if let Some(k) = key {
            self.scan.versions.lock().unwrap_or_else(|e| e.into_inner()).insert(k, v.clone());
        }
        v
    }

    fn warn_into_index(&self, rel: &str, reason: &str) {
        self.write_ix().scan_warnings.insert(rel.to_string(), clip(reason));
    }

    // ── ingest (lock-free parse, then install under the lock) ────────────

    /// ingestOne(row, buf) → the installed record's rel; marks pending when a store is attached.
    pub fn ingest_one(&self, row: &Row, buf: Option<&[u8]>) -> String {
        let (rec, text) = prepare_ingest(&row.path, row.size, row.mtime, row.hash.clone(), buf);
        let rel = rec.rel.clone();
        self.write_ix().install(rec, text);
        rel
    }

    /// drop(rel)
    pub fn drop_file(&self, rel: &str) -> bool {
        self.write_ix().drop_file(rel)
    }

    /// readMany(rels, sizeOf, { onFileError }) — bounded batches; with `on_file_error`, a refused
    /// batch is retried file by file.
    pub fn read_many(
        &self,
        rels: &[String],
        size_of: &dyn Fn(&str) -> u64,
        on_file_error: Option<&dyn Fn(&str, &str)>,
    ) -> Result<HashMap<String, Vec<u8>>> {
        let mut out: HashMap<String, Vec<u8>> = HashMap::new();
        let mut batch: Vec<String> = Vec::new();
        let mut bytes: u64 = 0;
        let files_cap = self.opts.read_batch_files;
        let bytes_cap = self.opts.read_batch_bytes as u64;
        let mut go = |batch: &mut Vec<String>, bytes: &mut u64| -> Result<()> {
            if batch.is_empty() {
                return Ok(());
            }
            let cur = std::mem::take(batch);
            *bytes = 0;
            match self.access.read(&cur) {
                Ok(got) => out.extend(got),
                Err(err) => {
                    let Some(on_err) = on_file_error else { return Err(Error::msg(err)) };
                    let mut got: Vec<(String, Vec<u8>)> = Vec::new();
                    let mut failed: Vec<(String, String)> = Vec::new();
                    for rel in &cur {
                        match self.access.read(std::slice::from_ref(rel)) {
                            Ok(g) => got.extend(g),
                            Err(e) => failed.push((rel.clone(), e)),
                        }
                    }
                    if failed.len() == cur.len() {
                        return Err(Error::msg(err));
                    }
                    for (rel, e) in &failed {
                        on_err(rel, e);
                    }
                    out.extend(got);
                }
            }
            Ok(())
        };
        for rel in rels {
            batch.push(rel.clone());
            bytes += size_of(rel);
            if batch.len() >= files_cap || bytes >= bytes_cap {
                go(&mut batch, &mut bytes)?;
            }
        }
        go(&mut batch, &mut bytes)?;
        Ok(out)
    }

    /// ensureTexts(list): make sure these records carry their text (re-ingest what moved, drop what vanished).
    pub fn ensure_texts(&self, rels: &[String]) -> Result<()> {
        let mut seen = HashSet::new();
        let mut need: Vec<String> = Vec::new();
        let mut sizes: HashMap<String, u64> = HashMap::new();
        {
            let ix = self.read_ix();
            for rel in rels {
                if !seen.insert(rel.as_str()) {
                    continue;
                }
                if let Some(f) = ix.files.get(rel) {
                    if f.content.is_none() && f.indexed && !okcode_analysis::is_secret(rel) {
                        need.push(rel.clone());
                        sizes.insert(rel.clone(), f.size);
                    }
                }
            }
        }
        if need.is_empty() {
            return Ok(());
        }
        let got = self.read_many(&need, &|p| sizes.get(p).copied().unwrap_or(0), None)?;
        let mut changed = false;
        for rel in &need {
            let Some(buf) = got.get(rel) else {
                self.drop_file(rel); // JS schedules a flush only after a re-ingest
                continue;
            };
            let h = crate::workspace::state::sha1_upper(buf);
            let same = self.read_ix().files.get(rel).map(|c| c.hash.as_deref() == Some(h.as_str()));
            match same {
                Some(true) => {
                    let (text, lossless) = okcode_analysis::text::text::decode(buf);
                    let mut ix = self.write_ix();
                    ix.cache_content(rel, Arc::from(text));
                    if let Some(cur) = ix.files.get_mut(rel) {
                        cur.lossless = lossless;
                    }
                }
                _ => {
                    // mtime unknown for these bytes: None makes the next verify re-check
                    // rather than trust a pairing it never observed.
                    self.ingest_one(&Row { path: rel.clone(), size: buf.len() as u64, mtime: None, hash: None }, Some(buf));
                    changed = true;
                }
            }
        }
        if changed {
            self.schedule_flush();
        }
        Ok(())
    }

    /// verify(paths, { strict }) — verify-on-read (stat first, read only what moved).
    pub fn verify(&self, paths: &[String], strict: bool) -> Result<VerifyOut> {
        let mut seen = HashSet::new();
        let want: Vec<String> = paths.iter().filter(|p| !p.is_empty() && seen.insert(p.as_str())).cloned().collect();
        if want.is_empty() {
            return Ok(VerifyOut::default());
        }
        self.settle_flushes();
        let fail = |err: String, partial: VerifyOut| -> Result<VerifyOut> {
            if strict {
                Err(Error::msg(format!("could not verify against the files: {err}")))
            } else {
                Ok(VerifyOut { unverified: true, ..partial })
            }
        };
        let lite = match self.stat_map(&want, false) {
            Ok(l) => l,
            Err(e) => return fail(e, VerifyOut::default()),
        };
        let mut gone: Vec<String> = Vec::new();
        let mut changed: Vec<String> = Vec::new();
        let mut need_hash: Vec<(String, u64, f64)> = Vec::new();
        let mut need_read: Vec<(String, u64, f64)> = Vec::new();
        {
            let mut ix = self.write_ix();
            for p in &want {
                let (size, mtime) = match lite.get(p) {
                    Some(StatRow::Found { size, mtime, .. }) => (*size, *mtime),
                    _ => {
                        if ix.drop_file(p) {
                            gone.push(p.clone());
                        }
                        continue;
                    }
                };
                let textual = okcode_analysis::is_textual(p, Some(size));
                if let Some(prior) = ix.files.get(p) {
                    if prior.size == size
                        && prior.mtime == Some(mtime)
                        && (if textual { prior.content.is_some() } else { !prior.indexed })
                    {
                        continue;
                    }
                }
                if textual { &mut need_read } else { &mut need_hash }.push((p.clone(), size, mtime));
            }
        }
        if !need_hash.is_empty() {
            let ps: Vec<String> = need_hash.iter().map(|x| x.0.clone()).collect();
            let hashed = match self.stat_map(&ps, true) {
                Ok(h) => h,
                Err(e) => return fail(e, VerifyOut { checked: want.len(), changed, gone, unverified: false }),
            };
            for (p, _, _) in &need_hash {
                let (size, mtime, hash) = match hashed.get(p) {
                    Some(StatRow::Found { size, mtime, hash }) => (*size, *mtime, hash.clone()),
                    _ => {
                        if self.drop_file(p) {
                            gone.push(p.clone());
                        }
                        continue;
                    }
                };
                {
                    let mut ix = self.write_ix();
                    if let Some(prior) = ix.files.get_mut(p) {
                        if prior.hash == hash && !prior.indexed {
                            prior.size = size;
                            prior.mtime = Some(mtime);
                            continue;
                        }
                    }
                }
                self.ingest_one(&Row { path: p.clone(), size, mtime: Some(mtime), hash }, None);
                changed.push(p.clone());
            }
        }
        if !need_read.is_empty() {
            let ps: Vec<String> = need_read.iter().map(|x| x.0.clone()).collect();
            let sizes: HashMap<&str, u64> = need_read.iter().map(|x| (x.0.as_str(), x.1)).collect();
            let bufs = match self.read_many(&ps, &|p| sizes.get(p).copied().unwrap_or(0), None) {
                Ok(b) => b,
                Err(e) => return fail(e.message, VerifyOut { checked: want.len(), changed, gone, unverified: false }),
            };
            for (p, size, mtime) in &need_read {
                let Some(buf) = bufs.get(p) else {
                    // Vanished between the stat and the read, or unreadable: nothing to quote.
                    if self.drop_file(p) {
                        gone.push(p.clone());
                    }
                    continue;
                };
                let h = crate::workspace::state::sha1_upper(buf);
                let same = self.read_ix().files.get(p).map(|r| r.hash.as_deref() == Some(h.as_str()) && r.indexed).unwrap_or(false);
                if same {
                    let (text, lossless) = okcode_analysis::text::text::decode(buf);
                    let mut ix = self.write_ix();
                    ix.cache_content(p, Arc::from(text));
                    if let Some(prior) = ix.files.get_mut(p) {
                        prior.lossless = lossless;
                        prior.size = *size;
                        prior.mtime = Some(*mtime);
                    }
                    continue;
                }
                self.ingest_one(&Row { path: p.clone(), size: *size, mtime: Some(*mtime), hash: None }, Some(buf));
                changed.push(p.clone());
            }
        }
        if !changed.is_empty() || !gone.is_empty() {
            self.schedule_flush();
        }
        Ok(VerifyOut { checked: want.len(), changed, gone, unverified: false })
    }

    /// resultSetCheck(paths) → `{ changed, unverified? }`
    pub fn result_set_check(&self, paths: &[String]) -> Result<Value> {
        let mut seen = HashSet::new();
        let want: Vec<String> = paths.iter().filter(|p| !p.is_empty() && seen.insert(p.as_str())).cloned().collect();
        if want.is_empty() {
            return Ok(json!({"changed": false}));
        }
        let Some(moved) = self.moved_of_list(&want) else { return Ok(json!({"changed": false, "unverified": true})) };
        if moved.is_empty() {
            return Ok(json!({"changed": false}));
        }
        let r = self.verify(&moved, false)?;
        Ok(json!({"changed": !r.changed.is_empty() || !r.gone.is_empty()}))
    }

    /// movedOf as a list; None = the facade could not be reached.
    pub fn moved_of_list(&self, want: &[String]) -> Option<Vec<String>> {
        let lite = self.stat_map(want, false).ok()?;
        let ix = self.read_ix();
        Some(
            want.iter()
                .filter(|p| match lite.get(*p) {
                    Some(StatRow::Found { size, mtime, .. }) => match ix.files.get(*p) {
                        Some(prior) => prior.size != *size || prior.mtime != Some(*mtime),
                        None => true,
                    },
                    _ => true,
                })
                .cloned()
                .collect(),
        )
    }

    /// movedOf(want) → array of paths, or null when the facade could not be reached.
    pub fn moved_of(&self, want: &[String]) -> Result<Value> {
        Ok(match self.moved_of_list(want) {
            Some(v) => json!(v),
            None => Value::Null,
        })
    }

    /// flagMoved(rows): mark listing rows (`{ file }`) that moved (`moved`), vanished (`gone`) or could
    /// not be checked (`unverified`). One metadata stat; nothing read or re-ingested.
    pub fn flag_moved(&self, mut rows: Value) -> Result<Value> {
        let Some(arr) = rows.as_array_mut() else { return Ok(rows) };
        if arr.is_empty() {
            return Ok(rows);
        }
        let files: Vec<String> = arr.iter().map(|r| r.get("file").and_then(Value::as_str).unwrap_or("").to_string()).collect();
        let lite = match self.stat_map(&files, false) {
            Ok(l) => l,
            Err(_) => {
                for r in arr.iter_mut() {
                    if let Some(o) = r.as_object_mut() {
                        o.insert("unverified".into(), json!(true));
                    }
                }
                return Ok(rows);
            }
        };
        let ix = self.read_ix();
        for (r, f) in arr.iter_mut().zip(&files) {
            let Some(o) = r.as_object_mut() else { continue };
            match lite.get(f) {
                Some(StatRow::Found { size, mtime, .. }) => {
                    let moved = match ix.files.get(f) {
                        Some(prior) => prior.size != *size || prior.mtime != Some(*mtime),
                        None => true,
                    };
                    if moved {
                        o.insert("moved".into(), json!(true));
                    }
                }
                _ => {
                    o.insert("gone".into(), json!(true));
                }
            }
        }
        drop(ix);
        Ok(rows)
    }

    /// listTree(warn) — the facade walk with skip/dot-prune/gitignore/own-transient filters
    /// (`warn` gets the rel of each file the facade cannot address).
    pub fn list_tree(&self, warn: &dyn Fn(&str)) -> Result<Vec<Row>> {
        self.list_tree_with(&|rel, _| warn(rel))
    }

    /// listTree with the reason: `warn(rel, why)`.
    pub fn list_tree_with(&self, warn: &dyn Fn(&str, &str)) -> Result<Vec<Row>> {
        let mut lo = ListOpts::default();
        if let Some(skip) = &self.opts.skip {
            lo.skip = skip.clone();
        }
        let listed = self.access.list(&lo).map_err(Error::msg)?;
        let gi_text = match self.access.read(&[".gitignore".to_string()]) {
            Ok(got) => got.into_iter().find(|(p, _)| p == ".gitignore").map(|(_, b)| String::from_utf8_lossy(&b).into_owned()),
            Err(_) => None,
        };
        let gitignored = Gitignore::compile(gi_text.as_deref().unwrap_or(""));
        let mut out = Vec::new();
        for r in listed.rows {
            if dot_prune().is_match(&r.path) || own_transient().is_match(&r.path) || gitignored.matches(&r.path) {
                continue;
            }
            // A name the facade cannot address is skipped with a warning before it can reach a
            // batched stat or read and fail every other file with it.
            if let Err(e) = self.access.check_path(&r.path) {
                warn(&r.path, &e);
                continue;
            }
            out.push(Row { path: r.path, size: r.size, mtime: Some(r.mtime), hash: None });
        }
        Ok(out)
    }

    /// fullScan({ rewrite, rehash }) → `{ scanned, reparsed, removed, pendingWrites, skipped? }`
    pub fn full_scan(&self, rewrite: bool, rehash: bool) -> Result<Value> {
        self.write_ix().scan_warnings.clear();
        let warn = |rel: &str, why: &str| self.warn_into_index(rel, why);
        let rows = self.list_tree_with(&warn)?;
        let mut seen: HashSet<String> = HashSet::new();
        let mut need_hash: Vec<Row> = Vec::new();
        let mut to_read: Vec<Row> = Vec::new();
        let mut untouched: Vec<(Row, Option<String>, bool, Option<String>)> = Vec::new(); // row, prior.hash, prior.indexed, prior.analyzerVersion
        {
            let ix = self.read_ix();
            for row in &rows {
                seen.insert(row.path.clone());
                let prior = ix.files.get(&row.path);
                if let Some(p) = prior {
                    if !rewrite && !rehash && p.size == row.size && p.mtime == row.mtime {
                        untouched.push((row.clone(), p.hash.clone(), p.indexed, p.analyzer_version.clone()));
                        continue;
                    }
                }
                if prior.is_none() || rewrite {
                    if okcode_analysis::is_textual(&row.path, Some(row.size)) {
                        to_read.push(row.clone());
                    } else {
                        need_hash.push(row.clone());
                    }
                    continue;
                }
                need_hash.push(row.clone());
            }
        }
        let mut hashed: HashMap<String, StatRow> = HashMap::new();
        if !need_hash.is_empty() {
            let ps: Vec<String> = need_hash.iter().map(|r| r.path.clone()).collect();
            hashed = self.stat_map(&ps, true).map_err(Error::msg)?;
        }
        let mut opaque: Vec<Row> = Vec::new();
        for row in &need_hash {
            let (size, mtime, hash) = match hashed.get(&row.path) {
                Some(StatRow::Found { size, mtime, hash }) => (*size, *mtime, hash.clone()),
                _ => {
                    seen.remove(&row.path); // vanished between the walk and the hash
                    continue;
                }
            };
            let prior: Option<Prior> = self.read_ix().files.get(&row.path).map(|p| Prior {
                size: p.size,
                mtime: p.mtime,
                hash: p.hash.clone(),
                indexed: p.indexed,
                analyzer_version: p.analyzer_version.clone(),
            });
            let wants_text = okcode_analysis::is_textual(&row.path, Some(size));
            let unchanged = !rewrite && prior.as_ref().is_some_and(|p| p.hash == hash);
            // A third way the rules can move under a file that never did: an analyser's version.
            let version = if wants_text { self.analyzer_version(&row.path) } else { None };
            let restale = unchanged
                && prior.as_ref().is_some_and(|p| wants_text != p.indexed || version.as_ref().is_some_and(|v| !v.is_empty() && p.analyzer_version.as_ref() != Some(v)));
            if unchanged && !restale {
                if let Some(p) = self.write_ix().files.get_mut(&row.path) {
                    p.size = size;
                    p.mtime = Some(mtime);
                }
                continue;
            }
            if wants_text {
                to_read.push(Row { path: row.path.clone(), size, mtime: Some(mtime), hash: None });
            } else {
                opaque.push(Row { path: row.path.clone(), size, mtime: Some(mtime), hash });
            }
        }
        // Rows whose (size, mtime) did not move still need the analyzerVersion re-stale check.
        for (row, p_hash, p_indexed, p_version) in untouched {
            let wants_text = okcode_analysis::is_textual(&row.path, Some(row.size));
            let version = if wants_text { self.analyzer_version(&row.path) } else { None };
            let restale = wants_text != p_indexed || version.as_ref().is_some_and(|v| !v.is_empty() && p_version.as_ref() != Some(v));
            if !restale {
                continue;
            }
            if wants_text {
                to_read.push(row);
            } else {
                opaque.push(Row { hash: p_hash, ..row });
            }
        }
        // Only textual files are READ; the rest are recorded from the scan row alone.
        for row in &opaque {
            self.ingest_one(row, None);
        }
        // Read, parse and write in bounded batches, so one batch's rows, text and in-flight
        // transaction are ever live at once.
        let batch_n = if self.opts.read_batch_files == 0 { 200 } else { self.opts.read_batch_files };
        for chunk in to_read.chunks(batch_n) {
            let sizes: HashMap<&str, u64> = chunk.iter().map(|r| (r.path.as_str(), r.size)).collect();
            let ps: Vec<String> = chunk.iter().map(|r| r.path.clone()).collect();
            let bodies = self.read_many(&ps, &|p| sizes.get(p).copied().unwrap_or(0), Some(&warn))?;
            let mut jobs: Vec<(&Row, &Vec<u8>)> = Vec::with_capacity(chunk.len());
            for row in chunk {
                match bodies.get(&row.path) {
                    Some(buf) => jobs.push((row, buf)),
                    None => {
                        seen.remove(&row.path); // vanished or unreadable since the walk
                    }
                }
            }
            // Parse (decode + extract + hash) on the bounded pool, install in list order.
            let prepared: Vec<_> = parse_pool().install(|| {
                use rayon::prelude::*;
                jobs.par_iter().map(|(row, buf)| prepare_ingest(&row.path, row.size, row.mtime, None, Some(buf.as_slice()))).collect()
            });
            let mut ix = self.write_ix();
            for (rec, text) in prepared {
                ix.install(rec, text);
            }
            drop(ix);
            if self.store.is_some() {
                self.flush()?;
            }
        }
        let mut removed = 0usize;
        let keys: Vec<String> = self.read_ix().files.keys().cloned().collect();
        for key in keys {
            if seen.contains(&key) {
                continue;
            }
            if self.drop_file(&key) {
                removed += 1;
            }
        }
        let mut ix = self.write_ix();
        ix.last_scan_at = now_ms();
        let mut out = json!({
            "scanned": rows.len(),
            "reparsed": opaque.len() + to_read.len(),
            "removed": removed,
            "pendingWrites": ix.pending.len(),
        });
        if !ix.scan_warnings.is_empty() {
            out["skipped"] = json!(ix.scan_warnings.len());
        }
        Ok(out)
    }

    /// ensureOpen = refresh() (the first-use walk; later calls answer `cached`).
    pub fn ensure_open(&self) -> Result<Value> {
        self.settle_flushes();
        self.refresh(RefreshOpts::default())
    }

    /// indexReady(): warm at start, a walk finished, or opened and not scanning.
    pub fn index_ready(&self) -> bool {
        if self.warm_at_start || self.read_ix().last_scan_at > 0.0 {
            return true;
        }
        let f = self.scan.flags.lock().unwrap_or_else(|e| e.into_inner());
        f.opened && !f.scanning
    }

    /// warmUp(): start the walk in the background if nothing has, and return at once (JS does not
    /// wait: a cold grep or glob must not queue behind the first walk).
    /// Called once by `Workspace::open` right after `Arc::new(Workspace{..})`:
    /// `ws.scan.bind(&ws)` (needed for warm_up and coalesced flushes).
    pub fn bind_ws(ws: &Arc<Workspace>) {
        let _ = ws.scan.weak.set(Arc::downgrade(ws));
    }

    pub fn warm_up(&self) {
        // Claim synchronously (JS sets opened/scanning before its first await) so close() waits
        // for the walk and a second warm_up does not start another.
        {
            let mut f = self.scan.flags.lock().unwrap_or_else(|e| e.into_inner());
            if f.opened || f.scanning || self.unindexed {
                return;
            }
            f.opened = true;
            f.scanning = true;
        }
        let ws = self.scan.weak.get().and_then(|w| w.upgrade());
        let Some(ws) = ws else {
            // no owned handle to run on: release the claim; the first ensure_open walks instead
            let mut f = self.scan.flags.lock().unwrap_or_else(|e| e.into_inner());
            f.opened = false;
            f.scanning = false;
            self.scan.cv.notify_all();
            return;
        };
        std::thread::spawn(move || {
            let _ = ws.refresh_claimed(RefreshOpts::default());
        });
    }

    // ── persistence ──────────────────────────────────────────────────────

    /// Apply parked outcomes of background batches to the index (flushing, storeErrors).
    pub fn settle_flushes(&self) -> usize {
        let parked: Vec<Settled> = std::mem::take(&mut *self.scan.flush.results.lock().unwrap_or_else(|e| e.into_inner()));
        let n = parked.len();
        for s in parked {
            self.apply_settled(&s);
        }
        n
    }

    fn apply_settled(&self, s: &Settled) -> Value {
        let failed: Vec<(String, String)> = s.outcome.as_ref().map(|o| o.failed.clone()).unwrap_or_default();
        let bad: HashSet<&str> = failed.iter().map(|(r, _)| r.as_str()).collect();
        {
            let mut ix = self.write_ix();
            for rel in &s.rels {
                match ix.flushing.get(rel).copied() {
                    Some(n) if n > 1 => {
                        ix.flushing.insert(rel.clone(), n - 1);
                    }
                    _ => {
                        ix.flushing.remove(rel);
                    }
                }
            }
            for rel in &s.rels {
                if !bad.contains(rel.as_str()) {
                    ix.store_errors.shift_remove(rel);
                }
            }
            for (rel, err) in &failed {
                ix.store_errors.insert(rel.clone(), err.clone());
            }
        }
        let written = s.outcome.as_ref().map(|o| o.written).unwrap_or(s.n_saves + s.n_removals);
        json!({"written": written, "failed": failed.iter().map(|(r, e)| json!({"rel": r, "error": e})).collect::<Vec<_>>()})
    }

    fn flusher(&self) -> Option<mpsc::Sender<Batch>> {
        let store = self.store.clone()?;
        let mut g = self.scan.flusher.lock().unwrap_or_else(|e| e.into_inner());
        if g.is_none() {
            let (tx, rx) = mpsc::channel();
            let shared = Arc::clone(&self.scan.flush);
            let ws = Arc::clone(&self.scan.weak);
            std::thread::Builder::new().name("okcode-flush".into()).spawn(move || flusher_loop(store, shared, ws, rx)).ok()?;
            *g = Some(tx);
        }
        g.clone()
    }

    /// The first half of doFlush: packages go out, `pending` becomes a batch and its rels are marked
    /// in flight. None = nothing pending.
    fn gather_batch(&self, done: Option<mpsc::Sender<Settled>>) -> Option<Batch> {
        self.store.as_ref()?;
        let mut ix = self.write_ix();
        if ix.pending.is_empty() && ix.pending_packages.is_empty() {
            return None;
        }
        let packages = std::mem::take(&mut ix.pending_packages);
        let pending = std::mem::take(&mut ix.pending);
        let mut saves = Vec::new();
        let mut removals = Vec::new();
        for (rel, op) in pending {
            match op {
                PendingOp::Remove => removals.push(rel),
                PendingOp::Save => {
                    if let Some(f) = ix.files.get(&rel) {
                        saves.push(f.clone());
                    }
                }
            }
        }
        // Out of `pending` but not yet searchable in the store: grep's pre-filter must not take
        // these rows' absence as an answer.
        for rel in saves.iter().map(|f| f.rel.clone()).chain(removals.iter().cloned()) {
            *ix.flushing.entry(rel).or_insert(0) += 1;
        }
        let seq = self.scan.flush.seq.fetch_add(1, Ordering::SeqCst);
        Some(Batch { saves, removals, packages, done, seq, wake: false })
    }

    /// doFlush: write what is pending and wait for it (and every earlier batch) to settle.
    pub fn do_flush(&self) -> Result<Value> {
        if self.store.is_none() {
            return Ok(json!({"written": 0}));
        }
        self.settle_flushes();
        let (tx, rx) = mpsc::channel();
        // The sender first: nothing is taken out of `pending` unless it can be delivered.
        let Some(sender) = self.flusher() else { return Ok(json!({"written": 0})) };
        let batch = self.gather_batch(Some(tx.clone()));
        let had = batch.is_some();
        let batch = batch.unwrap_or_else(|| Batch {
            saves: Vec::new(),
            removals: Vec::new(),
            packages: Vec::new(),
            done: Some(tx),
            seq: self.scan.flush.seq.fetch_add(1, Ordering::SeqCst),
            wake: false,
        });
        let my_seq = batch.seq;
        if let Err(mpsc::SendError(b)) = sender.send(batch) {
            self.requeue(b);
            return Ok(json!({"written": 0}));
        }
        // FIFO: when this batch (or the barrier) answers, every earlier one has settled.
        let settled = rx.recv().map_err(|_| Error::msg("flusher stopped"))?;
        // Apply in gather order: older parked outcomes, then ours; newer ones stay parked.
        let mut parked: Vec<Settled> = std::mem::take(&mut *self.scan.flush.results.lock().unwrap_or_else(|e| e.into_inner()));
        parked.sort_by_key(|s| s.seq);
        let (older, newer): (Vec<Settled>, Vec<Settled>) = parked.into_iter().partition(|s| s.seq < my_seq);
        for s in &older {
            self.apply_settled(s);
        }
        let out = if had { Some(self.apply_settled(&settled)) } else { None };
        if !newer.is_empty() {
            self.scan.flush.results.lock().unwrap_or_else(|e| e.into_inner()).extend(newer);
        }
        match out {
            Some(v) if settled.n_saves + settled.n_removals > 0 => Ok(v),
            _ => Ok(json!({"written": 0})),
        }
    }

    /// scheduleFlush: hand what is pending to the flusher thread without waiting. A burst of calls
    /// queues one wake-up; the flusher gathers once (JS: one `flushTimer`).
    pub fn schedule_flush(&self) {
        if self.store.is_none() || !self.opts.auto_flush {
            return;
        }
        let Some(tx) = self.flusher() else { return };
        if self.scan.weak.get().is_some() {
            if self.scan.flush.requested.swap(true, Ordering::SeqCst) {
                return;
            }
            let wake = Batch { saves: Vec::new(), removals: Vec::new(), packages: Vec::new(), done: None, seq: 0, wake: true };
            if tx.send(wake).is_err() {
                self.scan.flush.requested.store(false, Ordering::SeqCst);
            }
            return;
        }
        if let Some(batch) = self.gather_batch(None) {
            if let Err(mpsc::SendError(b)) = tx.send(batch) {
                self.requeue(b);
            }
        }
    }

    /// A batch that could not be delivered goes back to `pending`, with its in-flight marks undone.
    fn requeue(&self, b: Batch) {
        let mut ix = self.write_ix();
        for rel in b.saves.iter().map(|f| &f.rel).chain(b.removals.iter()) {
            match ix.flushing.get(rel).copied() {
                Some(n) if n > 1 => {
                    ix.flushing.insert(rel.clone(), n - 1);
                }
                _ => {
                    ix.flushing.remove(rel);
                }
            }
        }
        for f in b.saves {
            ix.pending.entry(f.rel).or_insert(PendingOp::Save);
        }
        for rel in b.removals {
            ix.pending.entry(rel).or_insert(PendingOp::Remove);
        }
        ix.pending_packages.extend(b.packages);
    }

    // ── api ─────────────────────────────────────────────────────────────

    pub fn refresh(&self, o: RefreshOpts) -> Result<Value> {
        // Nothing to walk: an unindexed workspace is read fresh on every call.
        if self.unindexed {
            return Ok(json!({"scanned": 0, "reparsed": 0, "removed": 0, "unindexed": true}));
        }
        let explicit = o.force || o.sync || o.rewrite || o.rehash;
        {
            let mut f = self.scan.flags.lock().unwrap_or_else(|e| e.into_inner());
            if f.scanning {
                while f.scanning {
                    f = self.scan.cv.wait(f).unwrap_or_else(|e| e.into_inner());
                }
                if !explicit {
                    drop(f);
                    return Ok(json!({"scanned": self.read_ix().files.len(), "reparsed": 0, "cached": true}));
                }
            }
            if !explicit && f.opened {
                drop(f);
                return Ok(json!({"scanned": self.read_ix().files.len(), "reparsed": 0, "cached": true}));
            }
            f.opened = true;
            f.scanning = true;
        }
        self.refresh_claimed(o)
    }

    /// The walk of a caller that already holds the `scanning` claim.
    fn refresh_claimed(&self, o: RefreshOpts) -> Result<Value> {
        let mut guard = ScanGuard { ws: self, ok: false };
        let result = self.full_scan(o.rewrite, o.rehash);
        guard.ok = result.is_ok();
        drop(guard);
        let result = result?;
        // An open or a sync is an explicit sync point: its results are persisted before it returns.
        if self.store.is_some() {
            self.flush()?;
        }
        Ok(result)
    }

    pub fn sync(&self, o: SyncOpts) -> Result<Value> {
        self.refresh(RefreshOpts { sync: true, rehash: o.force, ..Default::default() })
    }

    pub fn flush(&self) -> Result<Value> {
        self.do_flush()
    }

    /// scanWarnings() → `[{ file, reason }]`
    pub fn scan_warnings(&self) -> Value {
        let ix = self.read_ix();
        Value::Array(ix.scan_warnings.iter().map(|(f, r)| json!({"file": f, "reason": r})).collect())
    }

    pub fn stats(&self) -> Result<Value> {
        self.ensure_open()?;
        let ix = self.read_ix();
        let mut by_lang = serde_json::Map::new();
        let (mut symbols, mut with_symbols, mut searchable, mut opaque, mut failed_parse, mut cached) = (0usize, 0, 0, 0, 0, 0);
        for f in ix.files.values() {
            let n = by_lang.get(&f.lang).and_then(Value::as_u64).unwrap_or(0);
            by_lang.insert(f.lang.clone(), json!(n + 1));
            symbols += f.symbols.len();
            if !f.symbols.is_empty() {
                with_symbols += 1;
            }
            if f.indexed {
                searchable += 1;
            } else {
                opaque += 1;
            }
            if f.lang == "javascript" && !f.parsed {
                failed_parse += 1;
            }
            if f.content.is_some() {
                cached += 1;
            }
        }
        let kind = self.access.kind();
        Ok(json!({
            "id": self.id,
            "access": if kind.is_empty() { Value::Null } else { json!(kind) },
            "store": self.store.as_ref().map(|s| json!(s.env_name)).unwrap_or(Value::Null),
            "index": self.opts.index,
            "lastScanAt": ix.last_scan_at as i64,
            "files": ix.files.len(),
            "symbols": symbols,
            "withSymbols": with_symbols,
            "searchable": searchable,
            "opaque": opaque,
            "failedParse": failed_parse,
            "cachedText": cached,
            "pendingWrites": ix.pending.len(),
            "unsaved": ix.store_errors.len(),
            "skipped": ix.scan_warnings.len(),
            "byLang": Value::Object(by_lang),
        }))
    }

    pub fn close(&self) -> Result<Value> {
        // A walk a grep or glob started in the background finishes before the store is let go.
        {
            let mut f = self.scan.flags.lock().unwrap_or_else(|e| e.into_inner());
            while f.scanning {
                f = self.scan.cv.wait(f).unwrap_or_else(|e| e.into_inner());
            }
        }
        self.flush()?;
        if let Some(st) = &self.store {
            st.set_text_source(None);
        }
        Ok(Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gitignore_subset() {
        let g = Gitignore::compile("# c\nnode_modules/\n*.log\n/build\nsrc/gen\n!keep\n");
        assert!(g.matches("a/node_modules/x.js"));
        assert!(g.matches("x/y.log"));
        assert!(g.matches("build/o"));
        assert!(!g.matches("a/build/o"));
        assert!(g.matches("src/gen/a.ts"));
        assert!(!g.matches("lib/src/gen/a.ts"));
        assert!(!g.matches("keep"));
        assert!(!Gitignore::compile("").matches("a"));
    }

    #[test]
    fn filters() {
        assert!(dot_prune().is_match(".git/config"));
        assert!(dot_prune().is_match("a/.cache/x"));
        assert!(!dot_prune().is_match(".github/workflows/a.yml"));
        assert!(own_transient().is_match("a/.okcode-tmp-123"));
        assert!(own_transient().is_match("a.okcode-lock"));
    }
}
