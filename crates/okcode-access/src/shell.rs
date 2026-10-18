//! The shell facade (lib/access/shell.js): every byte goes through a shell
//! script (bash or PowerShell) over a host-supplied transport
//! `run(script, stdin) -> stdout`. The scripts in scripts/ are included verbatim.
//! Only bash is tested; the PowerShell path is implemented but unexercised.

use crate::common::*;
use crate::*;
use base64::engine::general_purpose::{GeneralPurpose, STANDARD};
use base64::engine::{DecodePaddingMode, GeneralPurposeConfig};
use base64::Engine;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock};

/// Executes `script` with `stdin` on the target; Err on a non-zero exit.
pub type Runner = Arc<dyn Fn(&str, &[u8]) -> Result<String, String> + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellDialect {
    Bash,
    PowerShell,
}

macro_rules! sh {
    ($d:literal, $f:literal) => {
        include_str!(concat!("../scripts/", $d, "/", $f))
    };
}

/// Full-line comments and blank lines stripped; parts joined; trailing newline.
fn compose(parts: &[&str]) -> String {
    let strip = |s: &str| {
        s.split('\n').filter(|l| !l.trim_start().starts_with('#') && !l.trim().is_empty()).collect::<Vec<_>>().join("\n")
    };
    parts.iter().map(|p| strip(p)).collect::<Vec<_>>().join("\n") + "\n"
}

fn scripts(d: ShellDialect) -> &'static HashMap<&'static str, String> {
    static BASH: OnceLock<HashMap<&'static str, String>> = OnceLock::new();
    static PS: OnceLock<HashMap<&'static str, String>> = OnceLock::new();
    match d {
        ShellDialect::Bash => BASH.get_or_init(|| {
            let (root, helpers) = (sh!("bash", "_root.sh"), sh!("bash", "_helpers.sh"));
            HashMap::from([
                ("list", compose(&[root, helpers, sh!("bash", "list.sh")])),
                ("stat", compose(&[root, helpers, sh!("bash", "stat.sh")])),
                ("read", compose(&[root, helpers, sh!("bash", "read.sh")])),
                ("readEach", compose(&[root, sh!("bash", "read-each.sh")])),
                ("remove", compose(&[root, sh!("bash", "remove.sh")])),
                ("commit", compose(&[root, helpers, sh!("bash", "commit.sh")])),
                ("liveness", compose(&[root, sh!("bash", "liveness.sh")])),
                ("exec", compose(&[root, sh!("bash", "exec.sh")])),
                ("grep", compose(&[root, sh!("bash", "grep.sh")])),
            ])
        }),
        ShellDialect::PowerShell => PS.get_or_init(|| {
            let root = sh!("powershell", "_root.ps1");
            HashMap::from([
                ("list", compose(&[root, sh!("powershell", "list.ps1")])),
                ("stat", compose(&[root, sh!("powershell", "stat.ps1")])),
                ("read", compose(&[root, sh!("powershell", "read.ps1")])),
                ("remove", compose(&[root, sh!("powershell", "remove.ps1")])),
                ("commit", compose(&[root, sh!("powershell", "commit.ps1")])),
                ("liveness", compose(&[root, sh!("powershell", "liveness.ps1")])),
                ("exec", compose(&[root, sh!("powershell", "exec.ps1")])),
            ])
        }),
    }
}

// Node's Buffer.from(x, 'base64') is lenient about padding; so is this.
fn b64_engine() -> GeneralPurpose {
    GeneralPurpose::new(&base64::alphabet::STANDARD, GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent).with_decode_allow_trailing_bits(true))
}

fn b64_decode_ws(raw: &str) -> Vec<u8> {
    let clean: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    b64_engine().decode(clean.as_bytes()).unwrap_or_default()
}

fn nul_fields(raw: &str) -> Vec<String> {
    String::from_utf8_lossy(&b64_decode_ws(raw)).split('\0').map(|s| s.to_string()).collect()
}

fn lines(raw: &str) -> impl Iterator<Item = &str> {
    raw.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l))
}

/// "<40 hex> <space or *><path>" from `sha1sum -z`.
fn parse_sha_line(s: &str) -> Option<(&str, &str)> {
    let b = s.as_bytes();
    if b.len() < 42 || !b[..40].iter().all(|c| c.is_ascii_hexdigit()) || (b[40] != b' ') || (b[41] != b' ' && b[41] != b'*') {
        return None;
    }
    Some((&s[..40], &s[42..]))
}

type Rows = HashMap<usize, StatRow>;

fn parse_stat_records(raw: &str, clean: &[String], hash: bool) -> Rows {
    let t = nul_fields(raw);
    let mut meta: HashMap<&str, &str> = HashMap::new();
    let mut hashes: HashMap<&str, &str> = HashMap::new();
    let mut i = 0;
    while i < t.len() {
        if (t[i] == "S" || t[i] == "H") && i + 2 < t.len() + 0 {
            let m = if t[i] == "S" { &mut meta } else { &mut hashes };
            m.insert(&t[i + 1], &t[i + 2]);
            i += 3;
            continue;
        }
        if let Some((h, p)) = parse_sha_line(&t[i]) {
            hashes.insert(p, h);
        }
        i += 1;
    }
    let mut rows = Rows::new();
    for (i, rel) in clean.iter().enumerate() {
        let key = format!("./{rel}");
        let sm = meta.get(key.as_str());
        let row = match sm {
            Some(sm) if !(hash && !hashes.contains_key(key.as_str())) => {
                let mut it = sm.split(' ');
                let size = it.next().and_then(|x| x.parse::<f64>().ok()).unwrap_or(f64::NAN);
                let mtime = it.next().and_then(|x| x.parse::<f64>().ok()).unwrap_or(f64::NAN);
                StatRow::Found {
                    size: size as u64,
                    mtime,
                    hash: if hash { Some(hashes[key.as_str()].to_uppercase()) } else { None },
                }
            }
            _ => StatRow::Missing,
        };
        rows.insert(i, row);
    }
    rows
}

fn parse_stat_lines(raw: &str, _clean: &[String], hash: bool) -> Rows {
    let mut rows = Rows::new();
    for line in lines(raw) {
        let mut it = line.split_whitespace();
        let i = match it.next().and_then(|x| x.parse::<usize>().ok()) {
            Some(i) => i,
            None => continue,
        };
        match it.next() {
            None | Some("-") => {
                rows.insert(i, StatRow::Missing);
            }
            Some(size) => {
                let mtime = it.next().and_then(|x| x.parse::<f64>().ok()).unwrap_or(f64::NAN);
                let h = if hash { Some(it.next().unwrap_or("undefined").to_uppercase()) } else { None };
                rows.insert(i, StatRow::Found { size: size.parse::<f64>().unwrap_or(0.0) as u64, mtime, hash: h });
            }
        }
    }
    rows
}

/// Batched bash read: base64(NUL pairs of path,size) "@DATA" base64(bytes).
/// None when the sizes do not add up.
fn parse_read_batched(raw: &str, clean: &[String]) -> Option<HashMap<usize, Vec<u8>>> {
    let at = if raw.starts_with("@DATA\n") { 0 } else { raw.find("\n@DATA\n").map_or(0, |i| i + 1) };
    let head = nul_fields(&raw[..at]);
    let data = b64_decode_ws(raw.get(at + 6..).unwrap_or(""));
    let mut by_path: HashMap<&str, &[u8]> = HashMap::new();
    let mut off = 0usize;
    let mut i = 0;
    while i + 1 < head.len() {
        let size: usize = head[i + 1].parse().ok()?;
        if off + size > data.len() {
            return None;
        }
        by_path.insert(&head[i], &data[off..off + size]);
        off += size;
        i += 2;
    }
    if off != data.len() {
        return None;
    }
    Some(clean.iter().enumerate().filter_map(|(i, rel)| by_path.get(format!("./{rel}").as_str()).map(|b| (i, b.to_vec()))).collect())
}

/// Per-file read answer: "@i" then base64 lines; a "!" line voids the entry.
fn parse_read_each(raw: &str) -> HashMap<usize, Vec<u8>> {
    let mut chunks: BTreeMap<usize, Vec<&str>> = BTreeMap::new();
    let mut cur: Option<usize> = None;
    for line in lines(raw) {
        let t = line.trim();
        if let Some(n) = t.strip_prefix('@') {
            cur = n.parse().ok();
            if let Some(c) = cur {
                chunks.insert(c, vec![]);
            }
        } else if t == "!" {
            if let Some(c) = cur {
                chunks.remove(&c);
            }
            cur = None;
        } else if let (Some(c), false) = (cur, t.is_empty()) {
            chunks.get_mut(&c).map(|v| v.push(t));
        }
    }
    chunks.into_iter().map(|(i, parts)| (i, b64_decode_ws(&parts.join("")))).collect()
}

/// Spawn bash / pwsh directly, stdin piped, stdout collected; Err on non-zero exit.
pub fn local_runner(dialect: ShellDialect) -> Runner {
    let (bin, args): (String, fn(&str) -> Vec<String>) = match dialect {
        ShellDialect::Bash => ("bash".into(), |s| vec!["--noprofile".into(), "--norc".into(), "-c".into(), s.into()]),
        ShellDialect::PowerShell => (
            if on_path("pwsh") { "pwsh".into() } else { "powershell.exe".into() },
            |s| vec!["-NoProfile".into(), "-NonInteractive".into(), "-Command".into(), s.into()],
        ),
    };
    Arc::new(move |script: &str, stdin: &[u8]| {
        let mut child = Command::new(&bin)
            .args(args(script))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        let mut si = child.stdin.take().expect("piped stdin");
        let payload = stdin.to_vec();
        // Writer on its own thread so a large payload cannot deadlock against stdout.
        let w = std::thread::spawn(move || {
            let _ = si.write_all(&payload); // a script that exits early closes its stdin
        });
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        let _ = w.join();
        if out.status.success() {
            return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
        }
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() { format!("{bin} exited {:?}", out.status.code()) } else { err })
    })
}

fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH").map_or(false, |p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
}

/// POSIX single-quote escaping, for transports that pass the script through a remote shell.
pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

struct Inner {
    root: String,
    run: Runner,
    dialect: ShellDialect,
}

#[derive(Clone)]
pub struct Shell {
    inner: Arc<Inner>,
}

impl Shell {
    pub fn new(root: &str, run: Runner, dialect: ShellDialect) -> Result<Shell, String> {
        if root.is_empty() {
            return Err("shell({ root }): root is required".into());
        }
        Ok(Shell { inner: Arc::new(Inner { root: root.into(), run, dialect }) })
    }

    fn path_dialect(&self) -> Dialect {
        match self.inner.dialect {
            ShellDialect::Bash => Dialect::Posix,
            ShellDialect::PowerShell => Dialect::Windows,
        }
    }

    fn hash_case(&self, h: &str) -> String {
        match self.inner.dialect {
            ShellDialect::Bash => h.to_lowercase(),
            ShellDialect::PowerShell => h.to_uppercase(),
        }
    }

    fn frame(&self, fields: &[String], tail: Option<&str>) -> Vec<u8> {
        let mut out = Vec::new();
        match self.inner.dialect {
            ShellDialect::Bash => {
                for f in fields {
                    out.extend_from_slice(f.as_bytes());
                    out.push(0);
                }
                if let Some(t) = tail {
                    out.extend_from_slice(t.as_bytes());
                }
            }
            ShellDialect::PowerShell => {
                for f in fields {
                    out.extend_from_slice(STANDARD.encode(f.as_bytes()).as_bytes());
                    out.push(b'\n');
                }
                if let Some(t) = tail {
                    out.extend_from_slice(t.as_bytes());
                    out.push(b'\n');
                }
            }
        }
        out
    }

    fn call(&self, method: &str, fields: &[String], tail: Option<&str>) -> Result<String, String> {
        let script = scripts(self.inner.dialect)
            .get(method)
            .ok_or_else(|| format!("no {method} script for this dialect"))?;
        let mut all = vec![self.inner.root.clone()];
        all.extend_from_slice(fields);
        (self.inner.run)(script, &self.frame(&all, tail))
    }

    fn check_all(&self, paths: &[String]) -> Result<Vec<String>, String> {
        paths.iter().map(|p| self.check_path(p)).collect()
    }

    /// Whether PID is alive ON THE TARGET, plus the target's hostname. alive None = unknown.
    pub fn liveness(&self, pid: &str) -> Liveness {
        let raw = match self.call("liveness", &[pid.to_string()], None) {
            Ok(r) => r,
            Err(_) => return Liveness { host: None, alive: None, start_time: None },
        };
        let (mut host, mut alive, mut start_time) = (None, None, None);
        for line in lines(&raw) {
            let t = line.trim();
            if let Some(h) = t.strip_prefix("HOST ") {
                host = Some(h.trim().to_string());
            } else if t == "DEAD" {
                alive = Some(false);
            } else if t.starts_with("ALIVE") {
                alive = Some(true);
                start_time = t.split_whitespace().nth(1).filter(|s| *s != "-").map(|s| s.to_string());
            }
        }
        Liveness { host, alive, start_time }
    }
}

impl Access for Shell {
    fn kind(&self) -> &str {
        "shell"
    }
    fn dialect(&self) -> Dialect {
        self.path_dialect()
    }
    fn root(&self) -> Option<&str> {
        Some(&self.inner.root)
    }
    fn check_path(&self, p: &str) -> Result<String, String> {
        check_path(p, self.path_dialect())
    }

    fn list(&self, o: &ListOpts) -> Result<ListOut, String> {
        let sub = match o.under.as_deref().filter(|s| !s.is_empty()) {
            Some(u) => self.check_path(u)?,
            None => String::new(),
        };
        let mut fields = vec![sub, o.limit.to_string()];
        fields.extend(o.skip.iter().cloned());
        let raw = self.call("list", &fields, None)?;
        let parts = nul_fields(&raw);
        let mut rows = vec![];
        let mut i = 0;
        while i + 2 < parts.len() {
            rows.push(FileRow {
                path: parts[i].clone(),
                size: parts[i + 1].parse::<f64>().unwrap_or(0.0) as u64,
                mtime: parts[i + 2].parse::<f64>().unwrap_or(f64::NAN),
            });
            i += 3;
        }
        if o.limit > 0 && rows.len() > o.limit {
            rows.truncate(o.limit);
            return Ok(ListOut { rows, truncated: true });
        }
        Ok(ListOut { rows, truncated: false })
    }

    fn stat(&self, paths: &[String], hash: bool) -> Result<Vec<(String, StatRow)>, String> {
        let clean = self.check_all(paths)?;
        if paths.is_empty() {
            return Ok(vec![]);
        }
        let mut fields = vec![if hash { "1" } else { "0" }.to_string()];
        fields.extend(clean.iter().cloned());
        let raw = self.call("stat", &fields, None)?;
        let mut rows = match self.inner.dialect {
            ShellDialect::Bash => parse_stat_records(&raw, &clean, hash),
            ShellDialect::PowerShell => parse_stat_lines(&raw, &clean, hash),
        };
        Ok(paths.iter().enumerate().map(|(i, p)| (p.clone(), rows.remove(&i).unwrap_or(StatRow::Missing))).collect())
    }

    fn read(&self, paths: &[String]) -> Result<Vec<(String, Vec<u8>)>, String> {
        let clean = self.check_all(paths)?;
        if paths.is_empty() {
            return Ok(vec![]);
        }
        let raw = self.call("read", &clean, None)?;
        let batched = raw.contains("\n@DATA\n") || raw.starts_with("@DATA\n");
        let mut got = if batched { parse_read_batched(&raw, &clean) } else { Some(parse_read_each(&raw)) };
        // A mis-sized batch (a file changed between find and cat) is re-read one file at a time.
        if got.is_none() {
            got = Some(parse_read_each(&self.call("readEach", &clean, None)?));
        }
        let mut got = got.unwrap();
        Ok(paths.iter().enumerate().filter_map(|(i, p)| got.remove(&i).map(|b| (p.clone(), b))).collect())
    }

    fn remove(&self, paths: &[String]) -> Result<(), String> {
        let clean = self.check_all(paths)?;
        if clean.is_empty() {
            return Ok(());
        }
        self.call("remove", &clean, None).map(|_| ())
    }

    /// `response-lost` is NOT a refusal: the transport failed after sending (or
    /// answered something unparseable), so the publish may have happened.
    fn commit(&self, p: &str, bytes: &[u8], o: &CommitOpts) -> Option<Result<CommitResult, String>> {
        let rel = match self.check_path(p) {
            Ok(r) => r,
            Err(e) => return Some(Err(e)),
        };
        let fields = vec![
            if o.create { "create" } else { "replace" }.to_string(),
            rel,
            o.expected_hash.as_deref().filter(|h| !h.is_empty()).map_or("-".to_string(), |h| self.hash_case(h)),
            self.hash_case(&sha1_upper(bytes)),
        ];
        let raw = match self.call("commit", &fields, Some(&STANDARD.encode(bytes))) {
            Ok(r) => r,
            Err(e) => return Some(Ok(CommitResult::err(Outcome::ResponseLost, e.chars().take(500).collect::<String>()))),
        };
        let line = lines(&raw).map(|l| l.trim()).filter(|l| !l.is_empty()).last().unwrap_or("");
        let mut it = line.split_whitespace();
        let tag = it.next().unwrap_or("");
        let arg = it.collect::<Vec<_>>().join(" ");
        Some(Ok(match tag {
            "OK" => CommitResult::ok(arg.to_uppercase()),
            "STALE" => CommitResult { current: Some(arg.to_uppercase()), ..CommitResult::of(Outcome::Stale) },
            "EXISTS" => CommitResult::of(Outcome::Exists),
            "MISSING" => CommitResult::of(Outcome::Missing),
            "SYMLINK" => CommitResult::of(Outcome::Symlink),
            "NOATOMIC" => {
                if arg.is_empty() {
                    CommitResult::of(Outcome::NoAtomic)
                } else {
                    CommitResult::err(Outcome::NoAtomic, arg)
                }
            }
            "CORRUPT" => CommitResult::err(Outcome::ResponseLost, "payload corrupted in transit; nothing was written"),
            _ => CommitResult::err(Outcome::ResponseLost, format!("unrecognised commit response: {}", line.chars().take(200).collect::<String>())),
        }))
    }

    /// The lock file records the CALLER's identity; liveness is checked on the TARGET.
    fn lock(&self, lock_path: &str, id: &str) -> Result<LockOutcome, String> {
        self.check_path(lock_path)?;
        let me = self_identity();
        let r = acquire_lock(self, &|pid| self.liveness(pid), &me, lock_path, id);
        let this = self.clone();
        let lp = lock_path.to_string();
        lock_outcome(r, move || {
            let _ = this.remove(&[lp]);
        })
    }

    /// TRUST BOUNDARY: runs a caller-supplied command in the root on the target.
    fn exec(&self, command: &str) -> Result<ExecOut, String> {
        let raw = match self.call("exec", &[], Some(&STANDARD.encode(command.as_bytes()))) {
            Ok(r) => r,
            Err(e) => return Ok(ExecOut { ok: false, out: e }),
        };
        let mut rc: Option<i64> = None;
        let mut section: Option<&str> = None;
        let (mut out, mut err) = (String::new(), String::new());
        for line in lines(&raw) {
            let t = line.trim();
            if let Some(n) = t.strip_prefix("RC ") {
                rc = n.trim().parse().ok();
            } else if t == "@OUT" {
                section = Some("OUT");
            } else if t == "@ERR" {
                section = Some("ERR");
            } else if !t.is_empty() {
                match section {
                    Some("OUT") => out.push_str(t),
                    Some("ERR") => err.push_str(t),
                    _ => {}
                }
            }
        }
        let dec = |s: &str| String::from_utf8_lossy(&b64_decode_ws(s)).into_owned();
        let stdout = dec(&out);
        if rc == Some(0) {
            return Ok(ExecOut { ok: true, out: stdout });
        }
        Ok(ExecOut { ok: false, out: stdout + &dec(&err) })
    }

    /// Optional grep pre-filter, decided ON THE TARGET. Ok(None) means "read them all".
    fn files_containing(&self, paths: &[String], text: &str, ignore_case: bool) -> Result<Option<HashSet<String>>, String> {
        if !scripts(self.inner.dialect).contains_key("grep") || text.is_empty() || text.contains(['\0', '\n']) {
            return Ok(None);
        }
        if ignore_case && !text.is_ascii() {
            return Ok(None); // C-locale -i folds ASCII only
        }
        let clean = self.check_all(paths)?;
        if clean.is_empty() {
            return Ok(Some(HashSet::new()));
        }
        let by_clean: HashMap<&str, &String> = clean.iter().map(|c| c.as_str()).zip(paths.iter()).collect();
        let mut fields = vec![if ignore_case { "i" } else { "-" }.to_string(), text.to_string()];
        fields.extend(clean.iter().cloned());
        let raw = match self.call("grep", &fields, None) {
            Ok(r) => r,
            Err(_) => return Ok(None),
        };
        let at = match raw.find("@OK\n") {
            Some(a) => a,
            None => return Ok(None),
        };
        if !raw[..at].trim().is_empty() {
            return Ok(None);
        }
        let mut out = HashSet::new();
        for n in nul_fields(&raw[at + 4..]).into_iter().filter(|s| !s.is_empty()) {
            // Every name must be one we asked about; anything else means a mis-parse.
            match by_clean.get(n.strip_prefix("./").unwrap_or(&n)) {
                Some(p) => {
                    out.insert((*p).clone());
                }
                None => return Ok(None),
            }
        }
        Ok(Some(out))
    }
}
