//! The local facade: std::fs against a folder on this machine (lib/access/local-fs.js).

use crate::common::*;
use crate::*;
use std::collections::HashSet;
use std::ffi::CString;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn tmp_name() -> String {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let s = SEQ.fetch_add(1, Ordering::Relaxed);
    format!(".okcode-tmp-{}-{:012x}", std::process::id(), (n as u64) ^ (s << 48) ^ s.wrapping_mul(0x9E37_79B9_7F4A_7C15))
}

fn readable(p: &Path) -> bool {
    match CString::new(p.as_os_str().as_encoded_bytes()) {
        // SAFETY: valid NUL-terminated path.
        Ok(c) => unsafe { libc::access(c.as_ptr(), libc::R_OK) == 0 },
        Err(_) => false,
    }
}

fn row_of(rel: String, m: &fs::Metadata) -> FileRow {
    FileRow { path: rel, size: m.len(), mtime: mtime_of(m.mtime(), m.mtime_nsec()) }
}

fn is_enoent(e: &std::io::Error) -> bool {
    matches!(e.raw_os_error(), Some(libc::ENOENT) | Some(libc::ENOTDIR))
}

#[derive(Clone)]
pub struct LocalFs {
    base: PathBuf,
    root: String,
}

impl LocalFs {
    pub fn new(root: impl AsRef<Path>) -> Result<LocalFs, String> {
        let root = root.as_ref();
        if root.as_os_str().is_empty() {
            return Err("localFs(root): root is required".into());
        }
        let base = std::path::absolute(root).map_err(|e| e.to_string())?;
        let root = base.to_string_lossy().into_owned();
        Ok(LocalFs { base, root })
    }

    fn abs(&self, rel: &str) -> Result<PathBuf, String> {
        let mut p = self.base.clone();
        for seg in self.check_path(rel)?.split('/') {
            p.push(seg);
        }
        Ok(p)
    }

    fn walk(&self, dir: &Path, rel_dir: &str, skip: &HashSet<&str>, cap: usize, out: &mut Vec<FileRow>) {
        if out.len() > cap {
            return;
        }
        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        let mut subdirs: Vec<(PathBuf, String)> = vec![];
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let rel = if rel_dir.is_empty() { name.clone() } else { format!("{rel_dir}/{name}") };
            let full = e.path();
            let ft = match e.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            if ft.is_dir() {
                if !skip.contains(name.as_str()) && !PRUNE_DOT_DIRS.contains(&name.as_str()) {
                    subdirs.push((full, rel));
                }
                continue;
            }
            if !ft.is_file() || !readable(&full) {
                continue;
            }
            if let Ok(m) = fs::metadata(&full) {
                if m.is_file() {
                    out.push(row_of(rel, &m));
                }
            }
        }
        for (full, rel) in subdirs {
            self.walk(&full, &rel, skip, cap, out);
        }
    }
}

impl Access for LocalFs {
    fn kind(&self) -> &str {
        "localFs"
    }
    fn dialect(&self) -> Dialect {
        Dialect::Posix
    }
    fn root(&self) -> Option<&str> {
        Some(&self.root)
    }
    fn check_path(&self, p: &str) -> Result<String, String> {
        check_path(p, Dialect::Posix)
    }

    fn list(&self, o: &ListOpts) -> Result<ListOut, String> {
        let skip: HashSet<&str> = o.skip.iter().map(|s| s.as_str()).collect();
        let cap = if o.limit > 0 { o.limit } else { usize::MAX };
        let mut out: Vec<FileRow> = vec![];
        let mut start_abs = self.base.clone();
        let mut start_rel = String::new();
        if let Some(u) = o.under.as_deref().filter(|s| !s.is_empty()) {
            start_rel = self.check_path(u)?;
            start_abs = self.abs(&start_rel)?;
            let (real, real_base) = match (fs::canonicalize(&start_abs), fs::canonicalize(&self.base)) {
                (Ok(a), Ok(b)) => (a, b),
                _ => return Ok(ListOut::default()), // not there
            };
            if real != real_base && !real.starts_with(&real_base) {
                return Err(format!("workspace path escapes the root (symlink): {}", serde_json::to_string(u).unwrap_or_default()));
            }
            let lst = fs::symlink_metadata(&start_abs).ok();
            // A symlinked directory that stays inside the root is walked.
            let dir = lst.as_ref().map_or(false, |l| l.is_dir() || (l.file_type().is_symlink() && start_abs.is_dir()));
            if !dir {
                if let Some(l) = lst.filter(|l| l.is_file()) {
                    if readable(&start_abs) {
                        if let Ok(m) = fs::metadata(&start_abs) {
                            let _ = l;
                            out.push(row_of(start_rel, &m));
                        }
                    }
                }
                return Ok(ListOut { rows: out, truncated: false });
            }
        }
        self.walk(&start_abs, &start_rel, &skip, cap, &mut out);
        if out.len() > cap {
            out.truncate(cap);
            return Ok(ListOut { rows: out, truncated: true });
        }
        Ok(ListOut { rows: out, truncated: false })
    }

    fn stat(&self, paths: &[String], hash: bool) -> Result<Vec<(String, StatRow)>, String> {
        let files = paths.iter().map(|p| self.abs(p)).collect::<Result<Vec<_>, _>>()?;
        Ok(paths
            .iter()
            .zip(files)
            .map(|(p, file)| {
                let row = (|| {
                    // stat BEFORE hashing: a racing write then yields a newer hash
                    // against an older mtime, which the next stat corrects.
                    let m = fs::metadata(&file).ok()?;
                    if !m.is_file() {
                        return None;
                    }
                    let h = if hash { Some(sha1_upper(&fs::read(&file).ok()?)) } else { None };
                    Some(StatRow::Found { size: m.len(), mtime: mtime_of(m.mtime(), m.mtime_nsec()), hash: h })
                })();
                (p.clone(), row.unwrap_or(StatRow::Missing))
            })
            .collect())
    }

    fn read(&self, paths: &[String]) -> Result<Vec<(String, Vec<u8>)>, String> {
        let files = paths.iter().map(|p| self.abs(p)).collect::<Result<Vec<_>, _>>()?;
        Ok(paths
            .iter()
            .zip(files)
            .filter_map(|(p, file)| {
                let m = fs::metadata(&file).ok()?;
                if !m.is_file() {
                    return None;
                }
                Some((p.clone(), fs::read(&file).ok()?))
            })
            .collect())
    }

    fn commit(&self, p: &str, bytes: &[u8], o: &CommitOpts) -> Option<Result<CommitResult, String>> {
        Some(self.abs(p).map(|file| commit_at(&file, bytes, o)))
    }

    fn remove(&self, paths: &[String]) -> Result<(), String> {
        let files = paths.iter().map(|p| self.abs(p)).collect::<Result<Vec<_>, _>>()?;
        for f in files {
            let _ = fs::remove_file(f);
        }
        Ok(())
    }

    fn lock(&self, lock_path: &str, id: &str) -> Result<LockOutcome, String> {
        self.check_path(lock_path)?;
        let r = acquire_lock(self, &local_liveness, &self_identity(), lock_path, id);
        let file = self.abs(lock_path)?;
        lock_outcome(r, move || {
            let _ = fs::remove_file(file);
        })
    }

    fn exec(&self, command: &str) -> Result<ExecOut, String> {
        match std::process::Command::new("/bin/sh").arg("-c").arg(command).current_dir(&self.base).output() {
            Ok(o) => {
                let out = String::from_utf8_lossy(&o.stdout).into_owned();
                if o.status.success() {
                    Ok(ExecOut { ok: true, out })
                } else {
                    let mut all = format!("{}{}", out, String::from_utf8_lossy(&o.stderr));
                    if all.is_empty() {
                        all = format!("Command failed: {command}");
                    }
                    Ok(ExecOut { ok: false, out: all })
                }
            }
            Err(e) => Ok(ExecOut { ok: false, out: e.to_string() }),
        }
    }
}

/// Publish BYTES atomically or refuse: a completed temp file in the target's
/// directory is published by link(2) for create (atomic fail-if-exists) or
/// rename(2) for replace (never across devices). The temp file is always removed.
fn commit_at(file: &Path, bytes: &[u8], o: &CommitOpts) -> CommitResult {
    let dir = file.parent().unwrap_or(Path::new("."));
    let want = sha1_upper(bytes);

    let lst = match fs::symlink_metadata(file) {
        Ok(m) => Some(m),
        Err(e) if is_enoent(&e) => None,
        Err(e) => return CommitResult::err(Outcome::NoAtomic, e.to_string()),
    };
    if lst.as_ref().map_or(false, |l| l.file_type().is_symlink()) {
        return CommitResult::of(Outcome::Symlink);
    }
    if o.create {
        if lst.is_some() {
            return CommitResult::of(Outcome::Exists);
        }
        if let Err(e) = fs::create_dir_all(dir) {
            return CommitResult::err(Outcome::NoAtomic, e.to_string());
        }
    } else if !lst.as_ref().map_or(false, |l| l.is_file()) {
        return CommitResult::of(Outcome::Missing);
    }

    let tmp = dir.join(tmp_name());
    let r = (|| {
        let w = fs::OpenOptions::new().write(true).create_new(true).mode(0o666).open(&tmp).and_then(|mut f| {
            f.write_all(bytes)?;
            f.flush()
        });
        if let Err(e) = w {
            return CommitResult::err(Outcome::NoAtomic, e.to_string());
        }
        // Mode bits survive an edit (the executable bit, notably).
        if !o.create {
            if let Some(l) = &lst {
                if let Err(e) = fs::set_permissions(&tmp, fs::Permissions::from_mode(l.mode() & 0o7777)) {
                    return CommitResult::err(Outcome::NoAtomic, e.to_string());
                }
            }
        }
        if o.create {
            return match fs::hard_link(&tmp, file) {
                Ok(()) => CommitResult::ok(want.clone()),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => CommitResult::of(Outcome::Exists),
                Err(e) => {
                    if fs::symlink_metadata(file).is_ok() {
                        CommitResult::of(Outcome::Exists)
                    } else {
                        // No hard links here: never fall back to a clobbering rename.
                        CommitResult::err(Outcome::NoAtomic, e.to_string())
                    }
                }
            };
        }
        // Re-hash the destination immediately before the rename (the narrowest
        // window this primitive can achieve; a non-cooperating writer can still land in it).
        let cur = match fs::symlink_metadata(file) {
            Ok(c) => c,
            Err(_) => return CommitResult::of(Outcome::Missing),
        };
        if cur.file_type().is_symlink() {
            return CommitResult::of(Outcome::Symlink);
        }
        if !cur.is_file() {
            return CommitResult::of(Outcome::Missing);
        }
        let current = match fs::read(file) {
            Ok(b) => sha1_upper(&b),
            Err(_) => return CommitResult::of(Outcome::Missing),
        };
        if let Some(eh) = o.expected_hash.as_ref().filter(|h| !h.is_empty()) {
            if current != eh.to_uppercase() {
                return CommitResult { current: Some(current), ..CommitResult::of(Outcome::Stale) };
            }
        }
        match fs::metadata(&tmp) {
            Ok(t) if t.dev() != cur.dev() => {
                return CommitResult::err(Outcome::NoAtomic, "temp and target on different devices")
            }
            Err(e) => return CommitResult::err(Outcome::NoAtomic, e.to_string()),
            _ => {}
        }
        match fs::rename(&tmp, file) {
            Ok(()) => CommitResult::ok(want.clone()),
            Err(e) => CommitResult::err(Outcome::NoAtomic, e.to_string()),
        }
    })();
    let _ = fs::remove_file(&tmp);
    r
}
