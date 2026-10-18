//! Pieces shared by every facade: path checking, hashing, the mtime number and
//! the advisory-lock algorithm (src/access/common.js).

use crate::{Access, CommitOpts, LockGuard, LockOutcome, Outcome};
use sha1::{Digest, Sha1};

pub const DEFAULT_SKIP: [&str; 8] = ["node_modules", ".git", ".idea", "dist", "build", "coverage", ".next", "vendor"];

/// Fixed list of known non-source dot-directories, pruned below the root. The
/// bash/PowerShell list scripts carry literal copies; tests/contract.rs asserts
/// they agree.
pub const PRUNE_DOT_DIRS: [&str; 24] = [
    ".git", ".hg", ".svn", ".okcode", ".adcode", ".next", ".nuxt", ".svelte-kit", ".turbo", ".cache",
    ".parcel-cache", ".venv", ".tox", ".mypy_cache", ".pytest_cache", ".ruff_cache", ".gradle", ".idea",
    ".vs", ".terraform", ".serverless", ".yarn", ".pnpm-store", ".expo",
];

pub fn sha1_upper(buf: &[u8]) -> String {
    let d = Sha1::digest(buf);
    d.iter().map(|b| format!("{b:02X}")).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    Posix,
    Windows,
}

fn json_str(p: &str) -> String {
    serde_json::to_string(p).unwrap_or_default()
}

/// Workspace-relative, `/`-separated, never escaping the root; lexical.
/// Returns the cleaned path (empty and `.` segments dropped).
pub fn check_path(p: &str, dialect: Dialect) -> Result<String, String> {
    if p.is_empty() {
        return Err(format!("invalid workspace path: {}", json_str(p)));
    }
    if p.contains('\0') {
        return Err(format!("invalid workspace path (NUL): {}", json_str(p)));
    }
    let absolute = match dialect {
        Dialect::Posix => p.starts_with('/'),
        Dialect::Windows => {
            let b = p.as_bytes();
            p.starts_with('/') || p.starts_with('\\') || (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':')
        }
    };
    if absolute {
        return Err(format!("workspace path must be relative: {}", json_str(p)));
    }
    let escapes = match dialect {
        Dialect::Posix => p.split('/').any(|s| s == ".."),
        Dialect::Windows => p.split(['\\', '/']).any(|s| s == ".."),
    };
    if escapes {
        return Err(format!("workspace path escapes the root: {}", json_str(p)));
    }
    let clean = p.split('/').filter(|s| !s.is_empty() && *s != ".").collect::<Vec<_>>().join("/");
    if clean.is_empty() {
        return Err(format!("invalid workspace path: {}", json_str(p)));
    }
    Ok(clean)
}

/// mtime as a number: whole seconds plus the 9-digit nanosecond fraction,
/// formatted as a decimal string then parsed (the path bash's `%T@` takes).
pub fn mtime_of(sec: i64, nsec: i64) -> f64 {
    format!("{}.{:09}", sec, nsec).parse().unwrap_or(sec as f64)
}

/// Start time of a local pid (Linux /proc/<pid>/stat field 22), or None.
pub fn proc_start_time(pid: i64) -> Option<String> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = s.get(s.rfind(')')? + 2..)?;
    rest.split(' ').nth(19).filter(|x| !x.is_empty()).map(|x| x.to_string())
}

pub fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: buf is valid for buf.len() bytes.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    if rc != 0 {
        return String::new();
    }
    let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Liveness {
    pub host: Option<String>,
    /// None = could not be determined (never proof of death).
    pub alive: Option<bool>,
    pub start_time: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Identity {
    pub pid: u32,
    pub host: String,
    pub start_time: Option<String>,
}

pub fn self_identity() -> Identity {
    let pid = std::process::id();
    Identity { pid, host: hostname(), start_time: proc_start_time(pid as i64) }
}

/// Liveness of a pid on THIS machine (localFs).
pub fn local_liveness(pid: &str) -> Liveness {
    let host = Some(hostname());
    let n: i64 = match pid.trim().parse() {
        Ok(n) if n > 0 => n,
        _ => return Liveness { host, alive: None, start_time: None },
    };
    // SAFETY: signal 0 only checks for existence.
    let rc = unsafe { libc::kill(n as libc::pid_t, 0) };
    if rc != 0 {
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::ESRCH) => return Liveness { host, alive: Some(false), start_time: None },
            Some(libc::EPERM) => {} // alive, another user's
            _ => return Liveness { host, alive: None, start_time: None },
        }
    }
    Liveness { host, alive: Some(true), start_time: proc_start_time(n) }
}

fn locked() -> Result<Result<(), String>, String> {
    Ok(Err("locked".into()))
}

/// The advisory-lock algorithm, written once against the facade's own
/// commit/read/remove. A lock is never broken on elapsed time, only when the
/// recorded owner is proven dead on the same host. Ok(Err(reason)) = not
/// acquired. The caller wraps success in a release guard.
pub fn acquire_lock(
    a: &dyn Access,
    liveness: &dyn Fn(&str) -> Liveness,
    me: &Identity,
    lock_path: &str,
    id: &str,
) -> Result<Result<(), String>, String> {
    let meta = serde_json::json!({ "pid": me.pid, "host": me.host, "startTime": me.start_time, "id": id });
    let payload = meta.to_string().into_bytes();
    let create = CommitOpts { create: true, expected_hash: None };
    let commit = || -> Result<crate::CommitResult, String> {
        a.commit(lock_path, &payload, &create).unwrap_or_else(|| Err("lock needs a facade with commit".into()))
    };
    let release = || {
        let _ = a.remove(&[lock_path.to_string()]);
    };

    let first = commit()?;
    if first.outcome == Outcome::Ok {
        return Ok(Ok(()));
    }
    if first.outcome != Outcome::Exists {
        return Ok(Err(first.outcome.as_str().into()));
    }

    let existing: serde_json::Value = match a
        .read(&[lock_path.to_string()])
        .ok()
        .and_then(|got| got.into_iter().next())
        .and_then(|(_, b)| serde_json::from_slice(&b).ok())
    {
        Some(v) => v,
        None => return locked(),
    };
    let obj = match existing.as_object() {
        Some(o) => o,
        None => return locked(),
    };
    let pid_s = match obj.get("pid") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
        None => "undefined".into(),
    };
    let live = liveness(&pid_s);
    let same_host = match (obj.get("host").and_then(|h| h.as_str()), live.host.as_deref()) {
        (Some(x), Some(y)) => x.to_lowercase() == y.to_lowercase(),
        _ => false,
    };
    if !same_host {
        return locked();
    }
    let rec_start = match obj.get("startTime") {
        Some(serde_json::Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(serde_json::Value::Number(n)) => Some(n.to_string()),
        _ => None,
    };
    let same_owner = live.alive == Some(true)
        && (rec_start.is_none() || live.start_time.is_none() || live.start_time == rec_start);
    if live.alive.is_none() || same_owner {
        return locked();
    }

    // Proven dead (or the pid now belongs to another process): break it, retry once.
    release();
    let second = commit()?;
    if second.outcome == Outcome::Ok {
        return Ok(Ok(()));
    }
    Ok(Err(if second.outcome == Outcome::Exists { "locked".into() } else { second.outcome.as_str().into() }))
}

pub fn lock_outcome(r: Result<Result<(), String>, String>, release: impl FnOnce() + Send + 'static) -> Result<LockOutcome, String> {
    Ok(match r? {
        Ok(()) => LockOutcome::Held(LockGuard::new(release)),
        Err(reason) => LockOutcome::Failed(reason),
    })
}
