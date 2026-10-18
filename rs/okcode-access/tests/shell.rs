//! Port of test/access-shell.test.js (shell-facade specifics).

use okcode_access::common::sha1_upper;
use okcode_access::shell::*;
use okcode_access::*;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

fn ws(files: &[(&str, &[u8])]) -> PathBuf {
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let root = std::env::temp_dir().join(format!("okcode-shell-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir_all(&root).unwrap();
    for (p, c) in files {
        let a = root.join(p);
        fs::create_dir_all(a.parent().unwrap()).unwrap();
        fs::write(a, c).unwrap();
    }
    root
}
fn v(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|x| x.to_string()).collect()
}
fn recording() -> (Arc<Mutex<Vec<String>>>, Runner) {
    let calls = Arc::new(Mutex::new(vec![]));
    let local = local_runner(ShellDialect::Bash);
    let c = calls.clone();
    (calls, Arc::new(move |s: &str, i: &[u8]| {
        c.lock().unwrap().push(s.to_string());
        local(s, i)
    }))
}
fn sh(root: &PathBuf, run: Runner) -> Shell {
    Shell::new(root.to_str().unwrap(), run, ShellDialect::Bash).unwrap()
}

#[test]
fn scripts_are_static_per_method() {
    let mut seen: std::collections::HashMap<String, usize> = Default::default();
    for name in ["a b", "it's"] {
        let xjs = format!("{name}/x.js");
        let yjs = format!("{name}/y.js");
        let root = ws(&[(&xjs, b"x")]);
        let (calls, run) = recording();
        let f = sh(&root, run);
        f.list(&ListOpts::default()).unwrap();
        f.stat(&[xjs.clone()], true).unwrap();
        f.read(&[xjs.clone()]).unwrap();
        f.commit(&yjs, b"PAYLOAD-MARKER", &CommitOpts { create: true, expected_hash: None }).unwrap().unwrap();
        f.remove(&[yjs.clone()]).unwrap();
        f.exec("echo hi").unwrap();
        if let LockOutcome::Held(g) = f.lock("z.lock", "op").unwrap() {
            g.release();
        }
        for s in calls.lock().unwrap().iter() {
            assert!(!s.contains(root.to_str().unwrap()));
            assert!(!s.contains(name));
            assert!(!s.contains("PAYLOAD"));
            *seen.entry(s.clone()).or_default() += 1;
        }
        let _ = fs::remove_dir_all(&root);
    }
    for c in seen.values() {
        assert_eq!(c % 2, 0);
    }
}

#[test]
fn batched_one_run_for_1201_paths() {
    let names: Vec<String> = (0..1201).map(|i| format!("d/f{i}.js")).collect();
    let files: Vec<(&str, Vec<u8>)> = names.iter().enumerate().map(|(i, n)| (n.as_str(), format!("file {i}").into_bytes())).collect();
    let refs: Vec<(&str, &[u8])> = files.iter().map(|(n, c)| (*n, c.as_slice())).collect();
    let root = ws(&refs);
    let (calls, run) = recording();
    let f = sh(&root, run);
    let st = f.stat(&names, true).unwrap();
    assert_eq!(calls.lock().unwrap().len(), 1);
    match &st[7].1 {
        StatRow::Found { hash, .. } => assert_eq!(hash.as_deref(), Some(sha1_upper(b"file 7").as_str())),
        _ => panic!("missing"),
    }
    let rd = f.read(&names).unwrap();
    assert_eq!(calls.lock().unwrap().len(), 2);
    assert_eq!(rd.len(), 1201);
    assert_eq!(rd[1200].1, b"file 1200");
    assert_eq!(f.list(&ListOpts::default()).unwrap().rows.len(), 1201);
    assert_eq!(calls.lock().unwrap().len(), 3);
    f.remove(&names).unwrap();
    assert_eq!(calls.lock().unwrap().len(), 4);
    assert_eq!(fs::read_dir(root.join("d")).unwrap().count(), 0);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn read_mis_sized_batch_is_reread_per_file() {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    let root = ws(&[("a.js", b"alpha"), ("b.js", b"bravo"), ("c.bin", &[0, 1, 2, 255])]);
    let calls = Arc::new(Mutex::new(0usize));
    let local = local_runner(ShellDialect::Bash);
    let c = calls.clone();
    let run: Runner = Arc::new(move |s, i| {
        *c.lock().unwrap() += 1;
        let out = local(s, i)?;
        if !out.contains("@DATA") {
            return Ok(out);
        }
        let (head, data) = out.split_once("@DATA\n").unwrap();
        let clean: String = data.chars().filter(|c| !c.is_whitespace()).collect();
        let mut bytes = b64.decode(clean).unwrap();
        bytes.push(b'!');
        Ok(format!("{head}@DATA\n{}\n", b64.encode(bytes)))
    });
    let f = sh(&root, run);
    let got = f.read(&v(&["a.js", "nope", "b.js", "c.bin"])).unwrap();
    assert_eq!(*calls.lock().unwrap(), 2);
    let keys: Vec<&str> = got.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["a.js", "b.js", "c.bin"]);
    assert_eq!(got[0].1, b"alpha");
    assert_eq!(got[1].1, b"bravo");
    assert_eq!(got[2].1, [0, 1, 2, 255]);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn commit_transport_failure_is_response_lost() {
    let root = ws(&[("a.js", b"a")]);
    let f = sh(&root, Arc::new(|_, _| Err("simulated: connection dropped after send".into())));
    let r = f.commit("a.js", b"b", &CommitOpts { expected_hash: Some(sha1_upper(b"a")), create: false }).unwrap().unwrap();
    assert_eq!(r.outcome, Outcome::ResponseLost);
    assert!(r.error.unwrap().contains("connection dropped"));
    let g = sh(&root, Arc::new(|_, _| Ok("what?\n".into())));
    let r = g.commit("a.js", b"b", &CommitOpts::default()).unwrap().unwrap();
    assert_eq!(r.outcome, Outcome::ResponseLost);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn commit_corrupt_payload_publishes_nothing() {
    let root = ws(&[("a.js", b"original")]);
    let local = local_runner(ShellDialect::Bash);
    let run: Runner = Arc::new(move |s, i| {
        if s.contains("CORRUPT") { local(s, &i[..i.len() - 4]) } else { local(s, i) }
    });
    let f = sh(&root, run);
    let r = f
        .commit("a.js", b"a replacement long enough to clip", &CommitOpts { expected_hash: Some(sha1_upper(b"original")), create: false })
        .unwrap()
        .unwrap();
    assert_eq!(r.outcome, Outcome::ResponseLost);
    assert!(r.error.unwrap().contains("corrupted"));
    assert_eq!(fs::read(root.join("a.js")).unwrap(), b"original");
    let stray: Vec<_> = fs::read_dir(&root).unwrap().filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().starts_with(".okcode-")).collect();
    assert!(stray.is_empty());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn missing_root_errors_rather_than_missing() {
    let root = std::env::temp_dir().join(format!("okcode-no-such-root-{}", std::process::id()));
    let f = sh(&root, local_runner(ShellDialect::Bash));
    let e = f.stat(&v(&["a.js"]), false).unwrap_err();
    assert!(e.contains("workspace root not found"), "{e}");
    let e = f.list(&ListOpts::default()).unwrap_err();
    assert!(e.contains("workspace root not found"), "{e}");
}

#[test]
fn lock_for_another_host_is_never_broken() {
    let root = ws(&[]);
    let local = local_runner(ShellDialect::Bash);
    let run: Runner = Arc::new(move |s, i| {
        if s.contains("uname -n") { local(&s.replace("$(uname -n)", "elsewhere"), i) } else { local(s, i) }
    });
    let f = sh(&root, run);
    let mut c = std::process::Command::new("true").spawn().unwrap();
    let pid = c.id();
    c.wait().unwrap();
    let host = okcode_access::common::hostname();
    fs::write(root.join("x.lock"), format!(r#"{{"pid":{pid},"host":"{host}","id":"old"}}"#)).unwrap();
    match f.lock("x.lock", "new").unwrap() {
        LockOutcome::Failed(r) => assert_eq!(r, "locked"),
        LockOutcome::Held(_) => panic!("lock was broken"),
    }
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn quote_round_trips_through_sh() {
    for s in ["plain", "it's", "'", "''", "a\"b$c`d\\e", "new\nline", "— unicode"] {
        let out = std::process::Command::new("sh").arg("-c").arg(format!("printf '%s' {}", quote(s))).output().unwrap();
        assert_eq!(String::from_utf8(out.stdout).unwrap(), s);
    }
}

#[test]
fn rejects_empty_root() {
    // Dialect is an enum and run is mandatory in Rust, so only the root check remains.
    assert!(Shell::new("", local_runner(ShellDialect::Bash), ShellDialect::Bash).is_err());
}
