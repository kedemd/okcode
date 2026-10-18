//! The workspace's shared mutable state and its primitive mutations.
//!
//! `Index` is the workspace's in-memory index: the records, packages, generation counter,
//! pending/flushing writes, store errors, scan warnings and the bounded LRU text cache. It sits behind ONE `RwLock` in `Workspace::ix`.
//!
//! Locking rules (every file of the workspace):
//!   1. Never hold `ix` across an access-facade call, a store call or a parse of many files:
//!      snapshot what you need, drop the guard, do the I/O, re-lock to apply.
//!   2. Mutations that JS does in ingestOne / drop / cacheContent go through the methods below so
//!      the LRU accounting and `generation` stay right.
//!   3. An area's own state lives in its own `XState` struct (its file), on `Workspace`, each
//!      behind its own lock. Lock order when both are needed: area state → `ix`.
use crate::record::FileRecord;
use indexmap::IndexMap;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

/// rel -> 'save' | 'remove' (last op wins); flushed as one transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingOp {
    Save,
    Remove,
}

#[derive(Default)]
pub struct Index {
    /// rel -> record (JS `files` Map; insertion order kept)
    pub files: IndexMap<String, FileRecord>,
    /// name -> package surface
    pub packages: IndexMap<String, Value>,
    /// bumped on every ingest/drop from any path
    pub generation: u64,
    /// ms since epoch of the last completed tree walk (0 = never)
    pub last_scan_at: f64,
    pub pending: IndexMap<String, PendingOp>,
    /// rel -> in-flight count: handed to the store, write not yet settled
    pub flushing: HashMap<String, u32>,
    pub pending_packages: Vec<Value>,
    /// rel -> error: files the store refused to write
    pub store_errors: IndexMap<String, String>,
    /// rel -> reason: files the last walk found but could not take in
    pub scan_warnings: IndexMap<String, String>,
    /// rels whose record holds `content`, least recently touched first -> byte length
    pub content_lru: IndexMap<String, usize>,
    pub cached_bytes: usize,
    /// opts.textCacheMaxBytes (copied here so eviction needs no Options)
    pub cache_max_bytes: usize,
    /// a store is attached (ingest/drop mark `pending`)
    pub has_store: bool,
}

pub fn sha1_upper(buf: &[u8]) -> String {
    use sha1::{Digest, Sha1};
    let d = Sha1::digest(buf);
    d.iter().map(|b| format!("{b:02X}")).collect()
}

impl Index {
    pub fn by_rel(&self, rel: &str) -> Option<&FileRecord> {
        self.files.get(rel)
    }

    /// cacheContent(record, text)
    pub fn cache_content(&mut self, rel: &str, text: Arc<str>) {
        if let Some(had) = self.content_lru.shift_remove(rel) {
            self.cached_bytes -= had;
        }
        let bytes = text.len();
        let Some(rec) = self.files.get_mut(rel) else { return };
        rec.content = Some(text);
        self.content_lru.insert(rel.to_string(), bytes);
        self.cached_bytes += bytes;
        self.evict_over_budget(rel);
    }

    /// forgetContent(rel) — accounting only (the caller drops/clears the record)
    pub fn forget_content(&mut self, rel: &str) {
        if let Some(had) = self.content_lru.shift_remove(rel) {
            self.cached_bytes -= had;
        }
    }

    /// evictContentOverBudget(keep)
    pub fn evict_over_budget(&mut self, keep: &str) {
        if self.cached_bytes <= self.cache_max_bytes {
            return;
        }
        let order: Vec<(String, usize)> = self.content_lru.iter().map(|(k, v)| (k.clone(), *v)).collect();
        for (rel, bytes) in order {
            if self.cached_bytes <= self.cache_max_bytes {
                break;
            }
            if rel == keep {
                continue;
            }
            if let Some(r) = self.files.get_mut(&rel) {
                r.content = None;
            }
            self.content_lru.shift_remove(&rel);
            self.cached_bytes -= bytes;
        }
    }

    /// ingestOne(row, buf) after the parse: install the record (decode + extract happen OUTSIDE the
    /// lock, see `prepare_ingest`). Returns the generation after the bump.
    pub fn install(&mut self, rec: FileRecord, text: Option<Arc<str>>) -> u64 {
        let rel = rec.rel.clone();
        let indexed = rec.indexed;
        if self.files.contains_key(&rel) {
            self.forget_content(&rel);
        }
        self.files.insert(rel.clone(), rec);
        if let (Some(t), true) = (text, indexed) {
            self.cache_content(&rel, t);
        }
        self.generation += 1;
        if self.has_store {
            self.pending.insert(rel, PendingOp::Save);
        }
        self.generation
    }

    /// drop(rel)
    pub fn drop_file(&mut self, rel: &str) -> bool {
        if !self.files.contains_key(rel) {
            return false;
        }
        self.forget_content(rel);
        self.files.shift_remove(rel);
        self.generation += 1;
        if self.has_store {
            self.pending.insert(rel.to_string(), PendingOp::Remove);
        }
        true
    }
}

/// The lock-free half of ingestOne: decode + extract + hash. `buf = None` = opaque (row.hash kept).
/// Returns the record and the decoded text to cache.
pub fn prepare_ingest(rel: &str, size: u64, mtime: Option<f64>, row_hash: Option<String>, buf: Option<&[u8]>) -> (FileRecord, Option<Arc<str>>) {
    let (text, lossless) = match buf {
        Some(b) => {
            let (t, l) = okcode_analysis::text::text::decode(b);
            (Some(t), l)
        }
        None => (None, true),
    };
    let parsed = crate::host_analysis::extract(rel, text.as_deref());
    let hash = match buf {
        Some(b) => Some(sha1_upper(b)),
        None => row_hash,
    };
    let rec = FileRecord::from_extract(rel, size, mtime, hash, parsed, lossless);
    (rec, text.map(Arc::from))
}
