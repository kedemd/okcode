//! Read-only facades built from host callbacks or an in-memory map
//! (lib/access/fn.js). No commit/lock/exec/remove: a workspace over one refuses edits.

use crate::common::*;
use crate::*;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

pub type ListFn = Box<dyn Fn(&ListOpts) -> Result<Vec<FileRow>, String> + Send + Sync>;
pub type ReadFn = Box<dyn Fn(&[String]) -> Result<Vec<(String, Vec<u8>)>, String> + Send + Sync>;
pub type StatFn = Box<dyn Fn(&[String], bool) -> Result<Vec<(String, StatRow)>, String> + Send + Sync>;

pub struct FnAccess {
    kind: String,
    root: Option<String>,
    list: ListFn,
    read: ReadFn,
    stat: Option<StatFn>,
}

/// `fn({ list, read, stat? })`: stat, when absent, comes from one list() (plus a
/// read for hashes).
pub fn fn_access(list: ListFn, read: ReadFn, stat: Option<StatFn>) -> FnAccess {
    FnAccess { kind: "fn".into(), root: None, list, read, stat }
}

impl FnAccess {
    pub fn with_kind(mut self, kind: &str, root: Option<String>) -> Self {
        self.kind = kind.into();
        self.root = root;
        self
    }

    fn addressable(&self, p: &str) -> bool {
        check_path(p, Dialect::Posix).map_or(false, |c| c == p)
    }

    fn list_all(&self, opts: &ListOpts) -> Result<Vec<FileRow>, String> {
        Ok((self.list)(opts)?.into_iter().filter(|r| self.addressable(&r.path)).collect())
    }
}

pub struct MemEntry {
    pub bytes: Vec<u8>,
    pub mtime: f64,
}

/// `memory(map)`: read live, so a host may add or replace entries between calls.
pub fn memory(map: Arc<Mutex<HashMap<String, MemEntry>>>) -> FnAccess {
    let m1 = map.clone();
    let m2 = map;
    fn_access(
        Box::new(move |_| {
            let g = m1.lock().map_err(|e| e.to_string())?;
            Ok(g.iter().map(|(p, e)| FileRow { path: p.clone(), size: e.bytes.len() as u64, mtime: e.mtime }).collect())
        }),
        Box::new(move |paths| {
            let g = m2.lock().map_err(|e| e.to_string())?;
            Ok(paths.iter().filter_map(|p| g.get(p).map(|e| (p.clone(), e.bytes.clone()))).collect())
        }),
        None,
    )
    .with_kind("memory", None)
}

impl Access for FnAccess {
    fn kind(&self) -> &str {
        &self.kind
    }
    fn dialect(&self) -> Dialect {
        Dialect::Posix
    }
    fn root(&self) -> Option<&str> {
        self.root.as_deref()
    }
    fn check_path(&self, p: &str) -> Result<String, String> {
        check_path(p, Dialect::Posix)
    }

    fn list(&self, o: &ListOpts) -> Result<ListOut, String> {
        let base = match o.under.as_deref().filter(|s| !s.is_empty()) {
            Some(u) => Some(self.check_path(u)?),
            None => None,
        };
        let names: HashSet<&str> = o.skip.iter().map(|s| s.as_str()).collect();
        let host_opts = ListOpts { under: base.clone(), skip: vec![], limit: 0 };
        let mut rows = self.list_all(&host_opts)?;
        rows.sort_by(|a, b| a.path.cmp(&b.path));
        let mut out = ListOut::default();
        for r in rows {
            if let Some(b) = &base {
                if r.path != *b && !r.path.starts_with(&format!("{b}/")) {
                    continue;
                }
            }
            // Skip names and the FIXED prune list apply BELOW `under`, never to its own segments.
            let below: &str = match &base {
                Some(b) if r.path != *b => &r.path[b.len() + 1..],
                Some(_) => "",
                None => &r.path,
            };
            let segs: Vec<&str> = below.split('/').collect();
            if segs[..segs.len() - 1].iter().any(|d| names.contains(d) || PRUNE_DOT_DIRS.contains(d)) {
                continue;
            }
            out.rows.push(r);
            if o.limit > 0 && out.rows.len() >= o.limit {
                out.truncated = true;
                break;
            }
        }
        Ok(out)
    }

    fn stat(&self, paths: &[String], hash: bool) -> Result<Vec<(String, StatRow)>, String> {
        let want = paths.iter().map(|p| self.check_path(p)).collect::<Result<Vec<_>, _>>()?;
        let meta: HashMap<String, StatRow> = match &self.stat {
            Some(f) => f(&want, hash)?.into_iter().collect(),
            None => {
                let all: HashMap<String, FileRow> =
                    self.list_all(&ListOpts { skip: vec![], under: None, limit: 0 })?.into_iter().map(|r| (r.path.clone(), r)).collect();
                want.iter()
                    .map(|p| {
                        let row = all.get(p).map_or(StatRow::Missing, |r| StatRow::Found {
                            size: r.size,
                            mtime: r.mtime,
                            hash: None,
                        });
                        (p.clone(), row)
                    })
                    .collect()
            }
        };
        let need_hash: Vec<String> = if hash {
            want.iter()
                .filter(|p| matches!(meta.get(*p), Some(StatRow::Found { hash: None, .. })))
                .cloned()
                .collect()
        } else {
            vec![]
        };
        let bodies: HashMap<String, Vec<u8>> =
            if need_hash.is_empty() { HashMap::new() } else { (self.read)(&need_hash)?.into_iter().collect() };
        Ok(want
            .iter()
            .map(|p| {
                let row = match meta.get(p) {
                    Some(StatRow::Found { size, mtime, hash: h }) => {
                        if hash {
                            match h.clone().or_else(|| bodies.get(p).map(|b| sha1_upper(b))) {
                                Some(h) => StatRow::Found { size: *size, mtime: *mtime, hash: Some(h) },
                                None => StatRow::Missing,
                            }
                        } else {
                            StatRow::Found { size: *size, mtime: *mtime, hash: None }
                        }
                    }
                    _ => StatRow::Missing,
                };
                (p.clone(), row)
            })
            .collect())
    }

    fn read(&self, paths: &[String]) -> Result<Vec<(String, Vec<u8>)>, String> {
        let checked = paths.iter().map(|p| self.check_path(p)).collect::<Result<Vec<_>, _>>()?;
        (self.read)(&checked)
    }
}
