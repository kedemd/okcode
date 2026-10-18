//! JS access facades → the Rust `Access` trait. SHARED FILE (architect-owned).
//!
//! A facade written in JS (src/access fn/memory/shell objects, a test's wrapped or counting
//! facade) is called back through ONE threadsafe function: the worker thread posts
//! `(id, method, args, buffers)` to the JS thread, js/access.js awaits the facade method, normalises
//! its result to JSON + buffers and calls `accessReply(id, err, value, buffers)`; the worker thread
//! blocks on that reply. Calls from the JS thread itself are refused (they would deadlock).
//!
//! A plain `localFs(root)` facade (tagged by preload-native.js, methods not wrapped) skips all of
//! this: `accessLocalFs(root)` builds the native okcode_access::LocalFs.
use napi::bindgen_prelude::Buffer;
use napi::Status;
use napi::threadsafe_function::{ErrorStrategy, ThreadSafeCallContext, ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi::{Env, JsFunction};
use napi_derive::napi;
use okcode_core::analysis::access::{
    self as acc, CommitOpts, CommitResult, Dialect, ExecOut, FileRow, ListOpts, ListOut, LockGuard, LockOutcome, Outcome, StatRow,
};
use okcode_core::workspace::Caps;
use okcode_core::Access;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::ThreadId;

#[napi]
pub struct AccessHandle {
    pub(crate) access: Arc<dyn Access>,
    pub(crate) caps: Caps,
}

/// accessLocalFs(root): the native fast path.
#[napi]
pub fn access_local_fs(root: String) -> napi::Result<AccessHandle> {
    let fs = acc::LocalFs::new(&root).map_err(|e| napi::Error::from_reason(e))?;
    Ok(AccessHandle { access: Arc::new(fs), caps: Caps::LOCAL_FS })
}

/// accessJs({ kind, root, dialect, caps: { commit, lock, remove, exec, filesContaining } }, call)
/// where `call(id, method, args, buffers)` is js/access.js's dispatcher.
#[napi]
pub fn access_js(env: Env, desc: Value, call: JsFunction) -> napi::Result<AccessHandle> {
    let mut tsfn: ThreadsafeFunction<Req, ErrorStrategy::Fatal> = call.create_threadsafe_function(0, |ctx: ThreadSafeCallContext<Req>| {
        let r = ctx.value;
        let id = ctx.env.create_uint32(r.id)?.into_unknown();
        let m = ctx.env.create_string(r.method)?.into_unknown();
        let a = crate::js_value(&ctx.env, r.args)?;
        let mut arr = ctx.env.create_array_with_length(r.bufs.len())?;
        for (i, b) in r.bufs.into_iter().enumerate() {
            let buf = ctx.env.create_buffer_with_data(b)?.into_raw();
            arr.set_element(i as u32, buf)?;
        }
        Ok(vec![id, m, a, arr.into_unknown()])
    })?;
    tsfn.unref(&env)?;
    let caps_v = &desc["caps"];
    let cap = |k: &str| caps_v[k].as_bool().unwrap_or(false);
    let caps = Caps { commit: cap("commit"), lock: cap("lock"), remove: cap("remove"), exec: cap("exec"), files_containing: cap("filesContaining") };
    let dialect = if desc["dialect"].as_str() == Some("windows") { Dialect::Windows } else { Dialect::Posix };
    let main = *MAIN.get_or_init(|| std::thread::current().id());
    let js = JsAccess {
        kind: desc["kind"].as_str().unwrap_or("fn").to_string(),
        root: desc["root"].as_str().map(String::from),
        dialect,
        caps,
        js_check: desc["checkPath"].as_bool().unwrap_or(false),
        tsfn,
        main,
    };
    Ok(AccessHandle { access: Arc::new(js), caps })
}

/// accessReply(id, err, value, buffers): the JS side's answer to one request.
#[napi]
pub fn access_reply(id: u32, err: Option<String>, value: Option<Value>, bufs: Option<Vec<Buffer>>) {
    let tx = pending().lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
    if let Some(tx) = tx {
        let _ = tx.send(Reply { err, value: value.unwrap_or(Value::Null), bufs: bufs.unwrap_or_default().into_iter().map(|b| b.to_vec()).collect() });
    }
}

static MAIN: OnceLock<ThreadId> = OnceLock::new();
static NEXT_ID: AtomicU32 = AtomicU32::new(1);

struct Reply {
    err: Option<String>,
    value: Value,
    bufs: Vec<Vec<u8>>,
}

fn pending() -> &'static Mutex<HashMap<u32, SyncSender<Reply>>> {
    static P: OnceLock<Mutex<HashMap<u32, SyncSender<Reply>>>> = OnceLock::new();
    P.get_or_init(Default::default)
}

pub struct Req {
    id: u32,
    method: &'static str,
    args: Value,
    bufs: Vec<Vec<u8>>,
}

struct JsAccess {
    kind: String,
    root: Option<String>,
    dialect: Dialect,
    caps: Caps,
    js_check: bool,
    tsfn: ThreadsafeFunction<Req, ErrorStrategy::Fatal>,
    main: ThreadId,
}

impl JsAccess {
    /// Post one request and block for its reply.
    fn call(&self, method: &'static str, args: Value, bufs: Vec<Vec<u8>>) -> Result<(Value, Vec<Vec<u8>>), String> {
        if std::thread::current().id() == self.main {
            return Err(format!("access.{method}: a JS facade cannot be called from the JS thread (sync call path)"));
        }
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed).max(1);
        let (tx, rx) = sync_channel(1);
        pending().lock().unwrap_or_else(|e| e.into_inner()).insert(id, tx);
        let st = self.tsfn.call(Req { id, method, args, bufs }, ThreadsafeFunctionCallMode::Blocking);
        if st != Status::Ok {
            pending().lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
            return Err(format!("access.{method}: the JS thread is gone ({st:?})"));
        }
        let r = rx.recv().map_err(|_| format!("access.{method}: no reply"))?;
        match r.err {
            Some(e) => Err(e),
            None => Ok((r.value, r.bufs)),
        }
    }

    /// Fire and forget (lock release).
    fn post(&self, method: &'static str, args: Value) {
        let _ = self.tsfn.call(Req { id: 0, method, args, bufs: vec![] }, ThreadsafeFunctionCallMode::NonBlocking);
    }
}

fn num(v: &Value) -> f64 {
    v.as_f64().unwrap_or(0.0)
}

fn outcome_of(s: &str) -> Outcome {
    match s {
        "ok" => Outcome::Ok,
        "stale" => Outcome::Stale,
        "exists" => Outcome::Exists,
        "missing" => Outcome::Missing,
        "symlink" => Outcome::Symlink,
        "response-lost" => Outcome::ResponseLost,
        _ => Outcome::NoAtomic,
    }
}

impl Access for JsAccess {
    fn kind(&self) -> &str {
        &self.kind
    }
    fn dialect(&self) -> Dialect {
        self.dialect
    }
    fn root(&self) -> Option<&str> {
        self.root.as_deref()
    }
    fn check_path(&self, p: &str) -> Result<String, String> {
        // A facade with its own checkPath (not the dialect's stock one) decides for itself.
        if self.js_check && std::thread::current().id() != self.main {
            let (v, _) = self.call("checkPath", json!({"path": p}), vec![])?;
            return Ok(v.as_str().unwrap_or(p).to_string());
        }
        acc::check_path(p, self.dialect)
    }
    fn list(&self, o: &ListOpts) -> Result<ListOut, String> {
        let (v, _) = self.call("list", json!({"skip": o.skip, "under": o.under, "limit": o.limit}), vec![])?;
        let rows = v["rows"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|r| FileRow { path: r["path"].as_str().unwrap_or("").to_string(), size: num(&r["size"]) as u64, mtime: num(&r["mtime"]) })
                    .collect()
            })
            .unwrap_or_default();
        Ok(ListOut { rows, truncated: v["truncated"].as_bool().unwrap_or(false) })
    }
    fn stat(&self, paths: &[String], hash: bool) -> Result<Vec<(String, StatRow)>, String> {
        let (v, _) = self.call("stat", json!({"paths": paths, "hash": hash}), vec![])?;
        Ok(v.as_array()
            .map(|a| {
                a.iter()
                    .map(|e| {
                        let p = e[0].as_str().unwrap_or("").to_string();
                        let r = &e[1];
                        let row = if r.is_null() {
                            StatRow::Missing
                        } else {
                            StatRow::Found { size: num(&r["size"]) as u64, mtime: num(&r["mtime"]), hash: r["hash"].as_str().map(String::from) }
                        };
                        (p, row)
                    })
                    .collect()
            })
            .unwrap_or_default())
    }
    fn read(&self, paths: &[String]) -> Result<Vec<(String, Vec<u8>)>, String> {
        let (v, bufs) = self.call("read", json!({"paths": paths}), vec![])?;
        let names: Vec<String> = v["paths"].as_array().map(|a| a.iter().map(|p| p.as_str().unwrap_or("").to_string()).collect()).unwrap_or_default();
        Ok(names.into_iter().zip(bufs).collect())
    }
    fn commit(&self, path: &str, bytes: &[u8], o: &CommitOpts) -> Option<Result<CommitResult, String>> {
        if !self.caps.commit {
            return None;
        }
        Some(self.call("commit", json!({"path": path, "expectedHash": o.expected_hash, "create": o.create}), vec![bytes.to_vec()]).map(|(v, _)| {
            CommitResult {
                outcome: outcome_of(v["outcome"].as_str().unwrap_or("")),
                hash: v["hash"].as_str().map(String::from),
                current: v["current"].as_str().map(String::from),
                error: v["error"].as_str().map(String::from),
            }
        }))
    }
    fn lock(&self, path: &str, id: &str) -> Result<LockOutcome, String> {
        if !self.caps.lock {
            return Err("lock is not supported by this facade".into());
        }
        let (v, _) = self.call("lock", json!({"path": path, "id": id}), vec![])?;
        if v["ok"].as_bool() == Some(true) {
            let lock_id = v["lockId"].clone();
            let me = JsAccess { kind: self.kind.clone(), root: self.root.clone(), dialect: self.dialect, caps: self.caps, js_check: self.js_check, tsfn: self.tsfn.clone(), main: self.main };
            Ok(LockOutcome::Held(LockGuard::new(move || me.post("release", json!({"lockId": lock_id})))))
        } else {
            Ok(LockOutcome::Failed(v["reason"].as_str().unwrap_or("locked").to_string()))
        }
    }
    fn remove(&self, paths: &[String]) -> Result<(), String> {
        if !self.caps.remove {
            return Err("remove is not supported by this facade".into());
        }
        self.call("remove", json!({"paths": paths}), vec![]).map(|_| ())
    }
    fn exec(&self, command: &str) -> Result<ExecOut, String> {
        if !self.caps.exec {
            return Err("exec is not supported by this facade".into());
        }
        let (v, _) = self.call("exec", json!({"command": command}), vec![])?;
        Ok(ExecOut { ok: v["ok"].as_bool().unwrap_or(false), out: v["out"].as_str().unwrap_or("").to_string() })
    }
    fn files_containing(&self, paths: &[String], text: &str, ignore_case: bool) -> Result<Option<HashSet<String>>, String> {
        if !self.caps.files_containing {
            return Ok(None);
        }
        let (v, _) = self.call("filesContaining", json!({"paths": paths, "text": text, "ignoreCase": ignore_case}), vec![])?;
        Ok(v.as_array().map(|a| a.iter().filter_map(|p| p.as_str().map(String::from)).collect()))
    }
}

#[allow(dead_code)]
fn _assert_send_sync() {
    fn is<T: Send + Sync>() {}
    is::<JsAccess>();
}
