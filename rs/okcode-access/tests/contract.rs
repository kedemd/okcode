//! A port of ~15 key checks of test/access-contract.test.js, run against LocalFs
//! and against Shell over a local bash (spawned per call, script + stdin).

use okcode_access::common::sha1_upper;
use okcode_access::*;
use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

type Make = fn(&Path) -> Box<dyn Access>;

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}
fn s(x: &str) -> String {
    x.to_string()
}
fn v(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|x| s(x)).collect()
}
fn sha(b: impl AsRef<[u8]>) -> String {
    sha1_upper(b.as_ref())
}

// A fresh workspace per test; the root is itself a DOT directory on purpose.
struct Ws {
    top: PathBuf,
    root: PathBuf,
}
impl Ws {
    fn new(files: &[(&str, &[u8])]) -> Ws {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let top = std::env::temp_dir().join(format!("okcode-access-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed)));
        let root = top.join(".ws-root");
        fs::create_dir_all(&root).unwrap();
        let w = Ws { top, root };
        for (p, c) in files {
            w.write(p, c);
        }
        w
    }
    fn abs(&self, p: &str) -> PathBuf {
        self.root.join(p)
    }
    fn write(&self, p: &str, c: &[u8]) {
        let a = self.abs(p);
        fs::create_dir_all(a.parent().unwrap()).unwrap();
        fs::write(a, c).unwrap();
    }
    fn chmod(&self, p: &str, m: u32) {
        fs::set_permissions(self.abs(p), fs::Permissions::from_mode(m)).unwrap();
    }
}
impl Drop for Ws {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.top);
    }
}

fn sorted(mut x: Vec<String>) -> Vec<String> {
    x.sort();
    x
}
fn paths(f: &dyn Access, o: &ListOpts) -> Vec<String> {
    f.list(o).unwrap().rows.into_iter().map(|r| r.path).collect()
}
fn found(r: &StatRow) -> (u64, f64, Option<String>) {
    match r {
        StatRow::Found { size, mtime, hash } => (*size, *mtime, hash.clone()),
        StatRow::Missing => panic!("expected found, got missing"),
    }
}
fn co(expected: Option<&str>, create: bool) -> CommitOpts {
    CommitOpts { expected_hash: expected.map(s), create }
}
fn commit(f: &dyn Access, p: &str, b: &[u8], o: CommitOpts) -> CommitResult {
    f.commit(p, b, &o).expect("commit supported").expect("commit ok")
}

fn t_list(make: Make) {
    let w = Ws::new(&[
        ("a.js", b"a"), (".env", b"x"), ("src/b.js", b"bb"), ("src/deep/c.js", b"ccc"),
        ("src/dist", b"a FILE named like a skip dir is still listed"),
        ("node_modules/x.js", b"x"), ("src/node_modules/y.js", b"y"), ("build/z.js", b"z"),
        (".git/HEAD", b"ref"), ("src/.cache/d.js", b"d"), ("locked.txt", b"secret"),
        (".turbo/cookie", b"pruned"), (".github/workflows/ci.yml", b"src"),
        (".storybook/main.js", b"src"), ("app/.well-known/security.txt", b"src"),
    ]);
    symlink("a.js", w.abs("link.js")).unwrap();
    w.chmod("locked.txt", 0);
    let f = make(&w.root);
    let rows = f.list(&ListOpts::default()).unwrap().rows;
    let mut expected = v(&[".env", "a.js", "src/b.js", "src/deep/c.js", "src/dist", ".github/workflows/ci.yml", ".storybook/main.js", "app/.well-known/security.txt"]);
    if is_root() {
        expected.push(s("locked.txt"));
    }
    assert_eq!(sorted(rows.iter().map(|r| r.path.clone()).collect()), sorted(expected));
    assert_eq!(rows.iter().find(|r| r.path == "src/deep/c.js").unwrap().size, 3);
    let custom = paths(&*f, &ListOpts { skip: v(&["src"]), ..Default::default() });
    assert!(custom.contains(&s("node_modules/x.js")) && custom.contains(&s("build/z.js")));
    assert!(!custom.iter().any(|p| p.starts_with("src/")));
    // under: skip names apply BELOW it, so node_modules/<pkg> is reachable; limit truncates.
    let under = paths(&*f, &ListOpts { under: Some(s("node_modules")), ..Default::default() });
    assert_eq!(under, v(&["node_modules/x.js"]));
    let lim = f.list(&ListOpts { limit: 2, ..Default::default() }).unwrap();
    assert!(lim.truncated && lim.rows.len() == 2);
    w.chmod("locked.txt", 0o644);
}

fn t_stat(make: Make) {
    let w = Ws::new(&[("a.js", b"hello"), ("dir/b.js", b"world!")]);
    symlink("a.js", w.abs("link.js")).unwrap();
    symlink("nowhere.js", w.abs("broken.js")).unwrap();
    let f = make(&w.root);
    let req = v(&["a.js", "dir/b.js", "nope.js", "dir", "broken.js", "link.js"]);
    let plain = f.stat(&req, false).unwrap();
    assert_eq!(plain.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>(), req);
    let m: HashMap<_, _> = plain.into_iter().collect();
    assert_eq!(found(&m["a.js"]).0, 5);
    assert!(found(&m["a.js"]).2.is_none());
    for p in ["nope.js", "dir", "broken.js"] {
        assert_eq!(m[p], StatRow::Missing, "{p}");
    }
    assert_eq!(found(&m["link.js"]).0, 5, "a link to a regular file is followed");
    let h: HashMap<_, _> = f.stat(&v(&["a.js", "dir/b.js", "nope.js"]), true).unwrap().into_iter().collect();
    assert_eq!(found(&h["a.js"]).2.unwrap(), sha("hello"));
    assert_eq!(found(&h["dir/b.js"]).2.unwrap(), sha("world!"));
    assert_eq!(h["nope.js"], StatRow::Missing);
    assert!(f.stat(&[], false).unwrap().is_empty());
    if !is_root() {
        w.write("locked.txt", b"secret");
        w.chmod("locked.txt", 0);
        let g = f.stat(&v(&["locked.txt"]), true).unwrap();
        assert_eq!(g[0].1, StatRow::Missing, "unreadable cannot be hashed");
        w.chmod("locked.txt", 0o644);
    }
}

fn t_mtime(make: Make) {
    let w = Ws::new(&[("a.js", b"x"), ("src/b.js", b"y")]);
    let f = make(&w.root);
    let ps = v(&["a.js", "src/b.js"]);
    let s1: HashMap<_, _> = f.stat(&ps, false).unwrap().into_iter().collect();
    let s2: HashMap<_, _> = f.stat(&ps, true).unwrap().into_iter().collect();
    let l1: HashMap<_, _> = f.list(&ListOpts::default()).unwrap().rows.into_iter().map(|r| (r.path.clone(), r)).collect();
    let l2: HashMap<_, _> = f.list(&ListOpts::default()).unwrap().rows.into_iter().map(|r| (r.path.clone(), r)).collect();
    for p in &ps {
        assert_eq!(found(&s1[p]).1, found(&s2[p]).1);
        assert_eq!(l1[p].mtime, l2[p].mtime);
        assert_eq!(l1[p].mtime, found(&s1[p]).1, "list and stat agree for an unchanged file");
    }
    let t = std::time::UNIX_EPOCH + std::time::Duration::new(981_000_000, 500_000_000);
    fs::OpenOptions::new().write(true).open(w.abs("a.js")).unwrap().set_modified(t).unwrap();
    let s3 = f.stat(&v(&["a.js"]), false).unwrap();
    assert_ne!(found(&s3[0].1).1, found(&s1["a.js"]).1);
}

fn t_read(make: Make) {
    let all: Vec<u8> = (0..=255u8).collect();
    let mut binary = all.clone();
    binary.extend_from_slice(&[0xff, 0xfe, 0x00, 0x0a, 0x0d, 0x0a, 0x80]);
    binary.extend_from_slice(&all);
    let text = "const dash = \"\u{2014}\"; // em dash\r\nnext line\n".as_bytes();
    let w = Ws::new(&[("bin.dat", &binary), ("text.js", text), ("empty.txt", b""), ("dir/x.js", b"x")]);
    let mut req = v(&["text.js", "nope.js", "bin.dat", "dir", "empty.txt"]);
    if !is_root() {
        w.write("locked.txt", b"secret");
        w.chmod("locked.txt", 0);
        req.push(s("locked.txt"));
    }
    let f = make(&w.root);
    let got = f.read(&req).unwrap();
    assert_eq!(got.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(), ["text.js", "bin.dat", "empty.txt"]);
    assert_eq!(got[0].1, text);
    assert_eq!(got[1].1, binary);
    assert!(got[2].1.is_empty());
    assert!(f.read(&[]).unwrap().is_empty());
    if !is_root() {
        w.chmod("locked.txt", 0o644);
    }
}

fn t_commit_create(make: Make) {
    let w = Ws::new(&[("old.js", b"incumbent")]);
    let f = make(&w.root);
    let bytes = "fresh \u{2014} content\n".as_bytes();
    assert_eq!(commit(&*f, "new/deeper/file.js", bytes, co(None, true)), CommitResult::ok(sha(bytes)));
    assert_eq!(fs::read(w.abs("new/deeper/file.js")).unwrap(), bytes);
    assert_eq!(commit(&*f, "new/deeper/file.js", b"loser", co(None, true)).outcome, Outcome::Exists);
    assert_eq!(fs::read(w.abs("new/deeper/file.js")).unwrap(), bytes, "the winner is untouched");
    assert_eq!(commit(&*f, "old.js", b"x", co(None, true)).outcome, Outcome::Exists);
    assert_eq!(fs::read_to_string(w.abs("old.js")).unwrap(), "incumbent");
    assert_eq!(commit(&*f, "empty.txt", b"", co(None, true)), CommitResult::ok(sha("")));
    assert_eq!(fs::metadata(w.abs("empty.txt")).unwrap().len(), 0);
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).unwrap().flatten() {
        if e.file_type().unwrap().is_dir() { walk(&e.path(), out) } else { out.push(e.path()) }
    }
}

fn t_commit_replace(make: Make) {
    let w = Ws::new(&[("a.js", b"v1"), ("tool.sh", b"#!/bin/sh\necho 1\n"), ("real.js", b"real")]);
    w.chmod("tool.sh", 0o754);
    symlink("real.js", w.abs("link.js")).unwrap();
    let f = make(&w.root);
    assert_eq!(commit(&*f, "a.js", b"v2", co(Some(&sha("v1")), false)), CommitResult::ok(sha("v2")));
    assert_eq!(fs::read_to_string(w.abs("a.js")).unwrap(), "v2");
    let stale = commit(&*f, "a.js", b"v3", co(Some(&sha("v1")), false));
    assert_eq!(stale, CommitResult { current: Some(sha("v2")), ..CommitResult::of(Outcome::Stale) });
    assert_eq!(fs::read_to_string(w.abs("a.js")).unwrap(), "v2");
    let lower = sha("v2").to_lowercase();
    assert_eq!(commit(&*f, "a.js", b"v3", co(Some(&lower), false)).outcome, Outcome::Ok, "expectedHash is case-insensitive");
    assert_eq!(commit(&*f, "a.js", b"v4", co(None, false)), CommitResult::ok(sha("v4")));
    assert_eq!(commit(&*f, "gone.js", b"x", co(Some(&sha("x")), false)), CommitResult::of(Outcome::Missing));
    assert!(!w.abs("gone.js").exists());
    assert!(f.commit(".", b"x", &co(None, false)).unwrap().is_err());
    assert_eq!(commit(&*f, "link.js", b"hijack", co(Some(&sha("real")), false)), CommitResult::of(Outcome::Symlink));
    assert_eq!(commit(&*f, "link.js", b"hijack", co(None, true)).outcome, Outcome::Symlink);
    assert_eq!(fs::read_to_string(w.abs("real.js")).unwrap(), "real");
    assert!(fs::symlink_metadata(w.abs("link.js")).unwrap().file_type().is_symlink());
    let script = b"#!/bin/sh\necho 2\n";
    assert_eq!(commit(&*f, "tool.sh", script, co(Some(&sha("#!/bin/sh\necho 1\n")), false)).outcome, Outcome::Ok);
    assert_eq!(fs::metadata(w.abs("tool.sh")).unwrap().permissions().mode() & 0o7777, 0o754);
    assert_eq!(fs::read(w.abs("tool.sh")).unwrap(), script);
    let mut all = vec![];
    walk(&w.root, &mut all);
    let left: Vec<_> = all.iter().filter(|p| p.file_name().unwrap().to_string_lossy().starts_with(".okcode-")).collect();
    assert!(left.is_empty(), "temp files left: {left:?}");
}

fn t_escape(make: Make) {
    let w = Ws::new(&[("a.js", b"a")]);
    fs::write(w.top.join("outside.txt"), "outside").unwrap();
    let f = make(&w.root);
    for bad in ["../outside.txt", "/etc/passwd", "a/../../outside.txt", "..", "", "x\0y"] {
        let b = vec![s(bad)];
        assert!(f.stat(&b, false).is_err(), "stat {bad:?}");
        assert!(f.read(&[s("a.js"), s(bad)]).is_err(), "read {bad:?}");
        assert!(f.commit(bad, b"x", &co(None, true)).unwrap().is_err(), "commit {bad:?}");
        assert!(f.remove(&b).is_err(), "remove {bad:?}");
    }
    assert!(f.lock("../x.lock", "op").is_err());
    assert_eq!(fs::read_to_string(w.top.join("outside.txt")).unwrap(), "outside");
}

fn t_weird_names(make: Make) {
    let names = v(&["sp ace/it's \"q\" $HOME `id` ;x.js", "ünï — cödé/файл.js", "-dash.js", "new\nline.js", "glob*?[ab].js"]);
    let w = Ws::new(&[]);
    let f = make(&w.root);
    let body = |n: &str| format!("content of {n}");
    for n in &names {
        let r = commit(&*f, n, body(n).as_bytes(), co(None, true));
        assert_eq!(r.outcome, Outcome::Ok, "{n:?}: {r:?}");
        assert_eq!(fs::read_to_string(w.abs(n)).unwrap(), body(n));
    }
    assert_eq!(sorted(paths(&*f, &ListOpts::default())), sorted(names.clone()));
    let st: HashMap<_, _> = f.stat(&names, true).unwrap().into_iter().collect();
    let rd: HashMap<_, _> = f.read(&names).unwrap().into_iter().collect();
    for n in &names {
        assert_eq!(found(&st[n]).2.unwrap(), sha(body(n)), "{n:?}");
        assert_eq!(rd[n], body(n).as_bytes(), "{n:?}");
    }
    assert_eq!(commit(&*f, &names[0], b"v2", co(Some(&sha(body(&names[0]))), false)).outcome, Outcome::Ok);
    f.remove(&names).unwrap();
    for n in &names {
        assert!(!w.abs(n).exists(), "{n:?}");
    }
}

fn t_posix_backslash(make: Make) {
    let names = v(&["d:\\test\\snake\\index.html", "C:/x/y.js", "..\\x.txt", "a\\..\\..\\b.txt"]);
    let w = Ws::new(&[]);
    let f = make(&w.root);
    for n in &names {
        let r = commit(&*f, n, format!("content of {n}").as_bytes(), co(None, true));
        assert_eq!(r.outcome, Outcome::Ok, "{n:?}: {r:?}");
    }
    let siblings: Vec<_> = fs::read_dir(&w.top).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(siblings, [".ws-root"], "every byte landed under the root");
    let top: Vec<_> = sorted(fs::read_dir(&w.root).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect());
    assert_eq!(top, sorted(v(&["..\\x.txt", "C:", "a\\..\\..\\b.txt", "d:\\test\\snake\\index.html"])));
    assert_eq!(sorted(paths(&*f, &ListOpts::default())), sorted(names.clone()));
    f.remove(&names).unwrap();
}

fn t_large(make: Make) {
    let w = Ws::new(&[]);
    let f = make(&w.root);
    let mut x: u64 = 0x1234_5678_9abc_def1;
    let mut rnd = |n: usize| -> Vec<u8> {
        (0..n).map(|_| { x ^= x << 13; x ^= x >> 7; x ^= x << 17; (x >> 24) as u8 }).collect()
    };
    let big = rnd(3 * 1024 * 1024);
    assert_eq!(commit(&*f, "big/blob.bin", &big, co(None, true)), CommitResult::ok(sha(&big)));
    assert_eq!(f.read(&v(&["big/blob.bin"])).unwrap()[0].1, big);
    assert_eq!(found(&f.stat(&v(&["big/blob.bin"]), true).unwrap()[0].1).2.unwrap(), sha(&big));
    let big2 = rnd(2 * 1024 * 1024 + 7);
    assert_eq!(commit(&*f, "big/blob.bin", &big2, co(Some(&sha(&big)), false)), CommitResult::ok(sha(&big2)));
    assert_eq!(fs::read(w.abs("big/blob.bin")).unwrap(), big2);
}

fn dead_pid() -> u32 {
    let mut c = std::process::Command::new("true").spawn().unwrap();
    let pid = c.id();
    c.wait().unwrap();
    pid
}

fn t_lock(make: Make) {
    let w = Ws::new(&[]);
    let f = make(&w.root);
    let host = okcode_access::common::hostname();
    let lock = |id: &str| f.lock(".okcode/edit.lock", id).unwrap();
    let held = match lock("op-1") { LockOutcome::Held(g) => g, LockOutcome::Failed(r) => panic!("{r}") };
    let meta: serde_json::Value = serde_json::from_slice(&fs::read(w.abs(".okcode/edit.lock")).unwrap()).unwrap();
    assert_eq!(meta["pid"], std::process::id());
    assert_eq!(meta["host"], host);
    assert_eq!(meta["id"], "op-1");
    assert!(matches!(lock("op-2"), LockOutcome::Failed(r) if r == "locked"));
    held.release();
    assert!(!w.abs(".okcode/edit.lock").exists());
    match lock("op-3") { LockOutcome::Held(g) => g.release(), LockOutcome::Failed(r) => panic!("{r}") }

    let la = w.abs(".okcode/edit.lock");
    let put = |j: serde_json::Value| fs::write(&la, j.to_string()).unwrap();
    put(serde_json::json!({"pid": dead_pid(), "host": host, "startTime": null, "id": "x"}));
    match lock("op-4") { LockOutcome::Held(g) => g.release(), LockOutcome::Failed(r) => panic!("dead owner not broken: {r}") }
    // Live pid whose start time no longer matches: a reused pid, broken.
    put(serde_json::json!({"pid": std::process::id(), "host": host, "startTime": "1", "id": "x"}));
    match lock("op-5") { LockOutcome::Held(g) => g.release(), LockOutcome::Failed(r) => panic!("reused pid not broken: {r}") }
    // Different host: never broken. Unparseable: cannot prove it is safe.
    put(serde_json::json!({"pid": dead_pid(), "host": "another-machine.invalid", "id": "x"}));
    assert!(matches!(lock("op-6"), LockOutcome::Failed(r) if r == "locked"));
    fs::write(&la, "not json").unwrap();
    assert!(matches!(lock("op-7"), LockOutcome::Failed(r) if r == "locked"));
    assert_eq!(fs::read_to_string(&la).unwrap(), "not json");
}

fn t_exec(make: Make) {
    let w = Ws::new(&[("marker.txt", b"here")]);
    let f = make(&w.root);
    assert_eq!(f.exec("cat marker.txt; echo \" ok\"").unwrap(), ExecOut { ok: true, out: s("here ok\n") });
    let bad = f.exec("echo to-out; echo to-err >&2; exit 3").unwrap();
    assert!(!bad.ok && bad.out.contains("to-out") && bad.out.contains("to-err"), "{bad:?}");
    assert_eq!(f.exec("printf '%s\\n' \"it's \u{2014} fine\"").unwrap(), ExecOut { ok: true, out: s("it's \u{2014} fine\n") });
}

fn t_remove(make: Make) {
    let w = Ws::new(&[("a.js", b"a"), ("d/b.js", b"b"), ("keep.js", b"k")]);
    let f = make(&w.root);
    f.remove(&v(&["a.js", "d/b.js", "nope.js", "d"])).unwrap();
    assert!(!w.abs("a.js").exists() && !w.abs("d/b.js").exists());
    assert!(w.abs("d").exists() && w.abs("keep.js").exists());
    f.remove(&[]).unwrap();
}

macro_rules! contract {
    ($modname:ident, $make:expr, [$($t:ident),*]) => {
        mod $modname {
            use super::*;
            $( #[test] fn $t() { super::$t($make) } )*
        }
    };
}
fn make_local(root: &Path) -> Box<dyn Access> {
    Box::new(LocalFs::new(root).unwrap())
}
fn make_bash(root: &Path) -> Box<dyn Access> {
    Box::new(Shell::new(root.to_str().unwrap(), local_runner(ShellDialect::Bash), ShellDialect::Bash).unwrap())
}
contract!(local_fs, make_local, [t_list, t_stat, t_mtime, t_read, t_commit_create, t_commit_replace, t_escape, t_weird_names, t_posix_backslash, t_large, t_lock, t_exec, t_remove]);
contract!(shell_bash, make_bash, [t_list, t_stat, t_mtime, t_read, t_commit_create, t_commit_replace, t_escape, t_weird_names, t_posix_backslash, t_large, t_lock, t_exec, t_remove]);

// Shell-only: the grep pre-filter, and the three-way PRUNE_DOT_DIRS agreement.
#[test]
fn shell_files_containing() {
    let w = Ws::new(&[("a.txt", b"hello World"), ("b.txt", b"nothing"), ("c.txt", b"say hello")]);
    let f = make_bash(&w.root);
    let ps = v(&["a.txt", "b.txt", "c.txt", "missing.txt"]);
    let got = f.files_containing(&ps, "hello", false).unwrap().unwrap();
    assert_eq!(sorted(got.into_iter().collect()), v(&["a.txt", "c.txt"]));
    let ci = f.files_containing(&ps, "WORLD", true).unwrap().unwrap();
    assert_eq!(sorted(ci.into_iter().collect()), v(&["a.txt"]));
    assert!(f.files_containing(&ps, "a\nb", false).unwrap().is_none());
}

#[test]
fn prune_dot_dirs_agree_with_bash_and_powershell() {
    let sh = include_str!("../../../src/access/scripts/bash/list.sh");
    let line = sh.lines().find(|l| l.starts_with("dotprune=(")).expect("dotprune");
    let names: Vec<&str> = line["dotprune=(".len()..line.rfind(')').unwrap()].split_whitespace().collect();
    assert_eq!(sorted(names.iter().map(|x| x.to_string()).collect()), sorted(PRUNE_DOT_DIRS.iter().map(|x| x.to_string()).collect()));
    let ps = include_str!("../../../src/access/scripts/powershell/list.ps1");
    let i = ps.find("$dotPrune").expect("$dotPrune");
    let body = &ps[ps[i..].find("@(").unwrap() + i + 2..];
    let body = &body[..body.find(')').unwrap()];
    let pn: Vec<String> = body.split('\'').skip(1).step_by(2).map(|x| x.to_string()).collect();
    assert_eq!(sorted(pn), sorted(PRUNE_DOT_DIRS.iter().map(|x| x.to_string()).collect()));
}

#[test]
fn fn_and_memory_are_read_only_and_confined() {
    let map: Arc<Mutex<HashMap<String, MemEntry>>> = Arc::new(Mutex::new(HashMap::new()));
    {
        let mut m = map.lock().unwrap();
        for (p, b) in [("a.js", "aa"), ("src/b.js", "bbb"), ("src/node_modules/x.js", "x"), (".github/ci.yml", "y"), (".git/HEAD", "h"), ("/abs.js", "z")] {
            m.insert(s(p), MemEntry { bytes: b.as_bytes().to_vec(), mtime: 7.0 });
        }
    }
    let f = memory(map.clone());
    assert!(f.commit("a.js", b"x", &co(None, true)).is_none(), "read-only");
    assert_eq!(paths(&f, &ListOpts::default()), v(&[".github/ci.yml", "a.js", "src/b.js"]));
    assert_eq!(paths(&f, &ListOpts { under: Some(s("src/node_modules")), ..Default::default() }), v(&["src/node_modules/x.js"]));
    let lim = f.list(&ListOpts { limit: 1, ..Default::default() }).unwrap();
    assert!(lim.truncated && lim.rows.len() == 1);
    let st = f.stat(&v(&["a.js", "nope.js"]), true).unwrap();
    assert_eq!(st[0].1, StatRow::Found { size: 2, mtime: 7.0, hash: Some(sha("aa")) });
    assert_eq!(st[1].1, StatRow::Missing);
    map.lock().unwrap().insert(s("new.js"), MemEntry { bytes: b"n".to_vec(), mtime: 0.0 });
    assert_eq!(f.read(&v(&["new.js"])).unwrap()[0].1, b"n");
    assert!(f.read(&v(&["../x"])).is_err());
}
