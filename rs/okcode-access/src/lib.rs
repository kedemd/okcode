//! okcode-access: the access facade (docs/ACCESS.md), ported from src/access/*.js.
//!
//! okcode never touches a filesystem itself; every byte in or out of a
//! workspace goes through an [`Access`] facade. The contract is the JS one with
//! two differences: the trait is SYNC (`Send + Sync`; a remote transport blocks
//! on its own thread, and a Node host bridges its async facades onto this), and
//! `Buffer` is `Vec<u8>`. Hashes are uppercase hex SHA-1; paths are
//! workspace-relative, `/`-separated, never escaping the root.
//!
//! Required: `list`, `stat`, `read`, `check_path`. Optional (default
//! "unsupported"): `commit` (None = read-only), `lock`, `remove`, `exec`,
//! `files_containing` (Ok(None) = "cannot answer, read them all").

pub mod common;
pub mod fn_access;
pub mod local_fs;
pub mod shell;

pub use common::{check_path, Dialect, Liveness, DEFAULT_SKIP, PRUNE_DOT_DIRS};
pub use fn_access::{fn_access, memory, FnAccess, MemEntry};
pub use local_fs::LocalFs;
pub use shell::{local_runner, Runner, Shell, ShellDialect};

use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq)]
pub struct FileRow {
    pub path: String,
    pub size: u64,
    pub mtime: f64,
}

#[derive(Clone, Debug)]
pub struct ListOpts {
    pub skip: Vec<String>,
    pub under: Option<String>,
    /// 0 = no limit.
    pub limit: usize,
}

impl Default for ListOpts {
    fn default() -> Self {
        ListOpts { skip: DEFAULT_SKIP.iter().map(|s| s.to_string()).collect(), under: None, limit: 0 }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ListOut {
    pub rows: Vec<FileRow>,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StatRow {
    Missing,
    Found { size: u64, mtime: f64, hash: Option<String> },
}

#[derive(Clone, Debug, Default)]
pub struct CommitOpts {
    pub expected_hash: Option<String>,
    pub create: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Stale,
    Exists,
    Missing,
    Symlink,
    NoAtomic,
    ResponseLost,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Stale => "stale",
            Outcome::Exists => "exists",
            Outcome::Missing => "missing",
            Outcome::Symlink => "symlink",
            Outcome::NoAtomic => "noatomic",
            Outcome::ResponseLost => "response-lost",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CommitResult {
    pub outcome: Outcome,
    pub hash: Option<String>,
    pub current: Option<String>,
    pub error: Option<String>,
}

impl CommitResult {
    pub fn of(outcome: Outcome) -> Self {
        CommitResult { outcome, hash: None, current: None, error: None }
    }
    pub fn ok(hash: String) -> Self {
        CommitResult { hash: Some(hash), ..Self::of(Outcome::Ok) }
    }
    pub fn err(outcome: Outcome, error: impl Into<String>) -> Self {
        CommitResult { error: Some(error.into()), ..Self::of(outcome) }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExecOut {
    pub ok: bool,
    pub out: String,
}

/// A held advisory lock. `release` is best effort; dropping without releasing
/// leaves the lock file in place (as in JS).
pub struct LockGuard {
    release: Box<dyn FnOnce() + Send>,
}

impl LockGuard {
    pub fn new(release: impl FnOnce() + Send + 'static) -> Self {
        LockGuard { release: Box::new(release) }
    }
    pub fn release(self) {
        (self.release)()
    }
}

pub enum LockOutcome {
    Held(LockGuard),
    /// `{ ok: false, reason }`
    Failed(String),
}

fn unsupported<T>(what: &str) -> Result<T, String> {
    Err(format!("{what} is not supported by this facade"))
}

pub trait Access: Send + Sync {
    fn kind(&self) -> &str;
    fn dialect(&self) -> Dialect;
    fn root(&self) -> Option<&str>;
    /// Lexical path check in this facade's dialect; returns the cleaned path.
    fn check_path(&self, p: &str) -> Result<String, String>;
    fn list(&self, opts: &ListOpts) -> Result<ListOut, String>;
    /// Rows in request order.
    fn stat(&self, paths: &[String], hash: bool) -> Result<Vec<(String, StatRow)>, String>;
    /// Present files only, in request order.
    fn read(&self, paths: &[String]) -> Result<Vec<(String, Vec<u8>)>, String>;

    /// None = read-only facade.
    fn commit(&self, _path: &str, _bytes: &[u8], _opts: &CommitOpts) -> Option<Result<CommitResult, String>> {
        None
    }
    fn lock(&self, _path: &str, _id: &str) -> Result<LockOutcome, String> {
        unsupported("lock")
    }
    fn remove(&self, _paths: &[String]) -> Result<(), String> {
        unsupported("remove")
    }
    fn exec(&self, _command: &str) -> Result<ExecOut, String> {
        unsupported("exec")
    }
    /// Ok(None) = the facade cannot answer; read every file.
    fn files_containing(&self, _paths: &[String], _text: &str, _ignore_case: bool) -> Result<Option<HashSet<String>>, String> {
        Ok(None)
    }
}

/// No goldens exist for this crate (the contract is covered by tests/contract.rs).
pub fn golden_call(name: &str, _args: &[serde_json::Value]) -> Result<serde_json::Value, String> {
    Err(format!("okcode-access has no golden functions (asked for {name})"))
}
