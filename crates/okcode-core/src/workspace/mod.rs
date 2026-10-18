//! The workspace (lib/workspace.js#openWorkspace).
//!
//! One `Workspace` struct whose methods are split by area, one file each (`impl Workspace`
//! blocks), plus this file, `state.rs` (shared state) and `api.rs` (the JS-facing dispatch):
//!
//! | file        | what                                                                          |
//! |-------------|-------------------------------------------------------------------------------|
//! | scan.rs     | compileGitignore, ingest/readMany/ensureTexts/verify/listTree/fullScan/       |
//! |             | refresh/flush, api sync/flush/scanWarnings/stats/close                        |
//! | lookup.rs   | carve/cut/split/label/parseRange, ownerOf/windowAt/fileOf,                    |
//! |             | resolve/textOfFresh/sliceSymbol/analysisOf, api structure/find/               |
//! |             | files/symbols/outline/read/readFile/readAt                                    |
//! | grep.rs     | indexReady/warmUp/grepUniverse/narrow/grepTexts, grepAll, api grep/glob       |
//! | library.rs  | normPath/libraryPath/listLibrary/…/outlineLibrary, isLibraryPath,             |
//! |             | the unindexed (`index: 'none'`) paths of every verb                           |
//! | edit.rs     | classifyCommit, targets/palette/moduleKind/commit/readBack,                   |
//! |             | api editBatch/edit/writeAt/writeWholeFile/syntaxCheck                         |
//! | graph.rs    | cappedList, mentionsOf, edges, api packages/package/ask/mentions/refs/        |
//! |             | importers                                                                     |
//!
//! Each area keeps its own state in an `XState` struct (in its file) and its helpers in its file.
pub mod api;
pub mod edit;
pub mod graph;
pub mod grep;
pub mod library;
pub mod lookup;
pub mod scan;
pub mod state;

use crate::error::{Error, Result};
use crate::store::Store;
use okcode_analysis::access::Access;
use serde::Deserialize;
use serde_json::Value;
use std::sync::{Arc, RwLock};

/// workspace.js DEFAULTS (options merged over them).
#[derive(Clone, Debug, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Options {
    pub file_read_lines: usize,
    pub window_lines: usize,
    pub window_max_chars: usize,
    pub outline_max_lines: usize,
    pub read_batch_files: usize,
    pub read_batch_bytes: usize,
    pub text_cache_max_bytes: usize,
    pub auto_flush: bool,
    /// null = the facade's default skip list
    pub skip: Option<Vec<String>>,
    pub library_files: usize,
    pub library_parse_files: usize,
    pub library_scans: usize,
    /// 'full' | 'none'
    pub index: String,
    /// null = TEXT_MAX (indexed) / NONE_TEXT_MAX (unindexed)
    pub text_max: Option<u64>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            file_read_lines: 600,
            window_lines: 12,
            window_max_chars: 1200,
            outline_max_lines: 80,
            read_batch_files: 200,
            read_batch_bytes: 8 * 1024 * 1024,
            text_cache_max_bytes: 24 * 1024 * 1024,
            auto_flush: true,
            skip: None,
            library_files: 20000,
            library_parse_files: 2000,
            library_scans: 4,
            index: "full".into(),
            text_max: None,
        }
    }
}

pub const NONE_TEXT_MAX: u64 = 32 * 1024 * 1024;
pub const MIN_PIECE_LINES: usize = 3;
pub const MENTIONS_REASON_CAP: usize = 8;

/// Which optional facade methods exist (JS: `typeof access.commit === 'function'`, …).
#[derive(Clone, Copy, Debug, Default)]
pub struct Caps {
    pub commit: bool,
    pub lock: bool,
    pub remove: bool,
    pub exec: bool,
    pub files_containing: bool,
}

impl Caps {
    /// What lib/access/local-fs.js provides.
    pub const LOCAL_FS: Caps = Caps { commit: true, lock: true, remove: true, exec: true, files_containing: false };
}

pub struct Workspace {
    pub id: String,
    pub access: Arc<dyn Access>,
    pub caps: Caps,
    pub store: Option<Arc<Store>>,
    pub opts: Options,
    /// index: 'none'
    pub unindexed: bool,
    pub read_only: bool,
    pub text_max: u64,
    /// files.size > 0 at open (indexReady)
    pub warm_at_start: bool,
    /// the shared index (state.rs)
    pub ix: RwLock<state::Index>,
    // per-area state, each owned by its file
    pub scan: scan::ScanState,
    pub lookup: lookup::LookupState,
    pub grep: grep::GrepState,
    pub library: library::LibraryState,
    pub edit: edit::EditState,
    pub graph: graph::GraphState,
}

impl Workspace {
    /// openWorkspace({ id, access, store, options }): validate, merge
    /// options, load the store into the index and hand it the text source. No walk here.
    pub fn open(id: &str, access: Arc<dyn Access>, caps: Caps, store: Option<Arc<Store>>, options: Value) -> Result<Arc<Workspace>> {
        if id.is_empty() {
            return Err(Error::msg("openWorkspace needs an id"));
        }
        let opts: Options = if options.is_null() { Options::default() } else { serde_json::from_value(options)? };
        if opts.index != "full" && opts.index != "none" {
            return Err(Error::msg(format!("openWorkspace: index must be 'full' or 'none' (got {:?})", opts.index)));
        }
        let unindexed = opts.index == "none";
        let store = if unindexed { None } else { store };
        let read_only = unindexed || !caps.commit;
        let text_max = opts.text_max.unwrap_or(if unindexed { NONE_TEXT_MAX } else { okcode_analysis::TEXT_MAX });
        let mut ix = state::Index { cache_max_bytes: opts.text_cache_max_bytes, has_store: store.is_some(), ..Default::default() };
        if let Some(st) = &store {
            let loaded = st.load()?;
            for r in loaded.files {
                ix.files.insert(r.rel.clone(), r);
            }
            for p in loaded.packages {
                if let Some(n) = p.get("name").and_then(Value::as_str) {
                    ix.packages.insert(n.to_string(), p.clone());
                }
            }
        }
        let warm_at_start = !ix.files.is_empty();
        let ws = Arc::new(Workspace {
            id: id.to_string(),
            access,
            caps,
            store: store.clone(),
            opts,
            unindexed,
            read_only,
            text_max,
            warm_at_start,
            ix: RwLock::new(ix),
            scan: Default::default(),
            lookup: Default::default(),
            grep: Default::default(),
            library: Default::default(),
            edit: Default::default(),
            graph: Default::default(),
        });
        if let Some(st) = &store {
            let weak = Arc::downgrade(&ws);
            st.set_text_source(Some(Arc::new(move |rel: &str, hash: &str| {
                let ws = weak.upgrade()?;
                let ix = ws.ix.read().ok()?;
                let f = ix.files.get(rel)?;
                (f.hash.as_deref() == Some(hash)).then(|| f.content.clone()).flatten()
            })));
        }
        Ok(ws)
    }

    /// `index` property ('full' | 'none')
    pub fn index_mode(&self) -> &str {
        &self.opts.index
    }

    pub fn read_ix(&self) -> std::sync::RwLockReadGuard<'_, state::Index> {
        self.ix.read().unwrap_or_else(|e| e.into_inner())
    }
    pub fn write_ix(&self) -> std::sync::RwLockWriteGuard<'_, state::Index> {
        self.ix.write().unwrap_or_else(|e| e.into_inner())
    }
}

