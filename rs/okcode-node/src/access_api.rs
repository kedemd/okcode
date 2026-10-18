//! C2 (access slice): the native access facades (localFs, shell, fn/memory) for the thin
//! src/access wrappers (js/src/access/*.js). Slice-owned (rs/tools/slices.conf).
//!
//! A `Facade` wraps one `Arc<dyn Access>`; `facade.call(method, args, buffers)` runs the sync core on
//! the libuv threadpool (AsyncTask) and resolves to JSON (read: `[paths, buffers]`). Host JS the
//! facade needs (a shell's `run(script, stdin)`, an `access.fn` user's list/read/stat) is reached
//! through ONE threadsafe function per facade: the worker thread posts `(id, kind, args)` to the JS
//! thread, the JS dispatcher awaits the host function and answers `facadeReply(id, err, value,
//! buffers)`; the worker blocks on that reply. The JS thread is never blocked (a call arriving on
//! it is refused, it would deadlock).
use napi::bindgen_prelude::Buffer;
use napi::threadsafe_function::{ErrorStrategy, ThreadSafeCallContext, ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi::{Env, JsFunction, JsObject, JsUnknown, Status, Task};
use napi_derive::napi;
use okcode_core::analysis::access::{
    self as acc, fn_access, CommitOpts, FileRow, ListOpts, LockOutcome, Runner, Shell, ShellDialect, StatRow, DEFAULT_SKIP,
};
use okcode_core::Access;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::ThreadId;

// ── host callouts ─────────────────────────────────────────────────────────

struct CReq {
    id: u32,
    kind: &'static str,
    args: Value,
}

struct CReply {
    err: Option<String>,
    value: Value,
    bufs: Vec<Vec<u8>>,
}

static NEXT_ID: AtomicU32 = AtomicU32::new(1);
static MAIN: OnceLock<ThreadId> = OnceLock::new();

fn pending() -> &'static Mutex<HashMap<u32, SyncSender<CReply>>> {
    static P: OnceLock<Mutex<HashMap<u32, SyncSender<CReply>>>> = OnceLock::new();
    P.get_or_init(Default::default)
}

#[derive(Clone)]
struct Callout {
    tsfn: ThreadsafeFunction<CReq, ErrorStrategy::Fatal>,
    main: ThreadId,
}

impl Callout {
    fn new(env: &Env, cb: JsFunction) -> napi::Result<Callout> {
        let mut tsfn: ThreadsafeFunction<CReq, ErrorStrategy::Fatal> = cb.create_threadsafe_function(0, |ctx: ThreadSafeCallContext<CReq>| {
            let r = ctx.value;
            let id = ctx.env.create_uint32(r.id)?.into_unknown();
            let kind = ctx.env.create_string(r.kind)?.into_unknown();
            let args = crate::js_value(&ctx.env, r.args)?;
            Ok(vec![id, kind, args])
        })?;
        tsfn.unref(env)?;
        let main = *MAIN.get_or_init(|| std::thread::current().id());
        Ok(Callout { tsfn, main })
    }

    /// Post one request to the JS dispatcher and block for its reply.
    fn call(&self, kind: &'static str, args: Value) -> Result<(Value, Vec<Vec<u8>>), String> {
        if std::thread::current().id() == self.main {
            return Err(format!("access.{kind}: a JS host function cannot be called from the JS thread (sync call path)"));
        }
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed).max(1);
        let (tx, rx) = sync_channel(1);
        pending().lock().unwrap_or_else(|e| e.into_inner()).insert(id, tx);
        let st = self.tsfn.call(CReq { id, kind, args }, ThreadsafeFunctionCallMode::Blocking);
        if st != Status::Ok {
            pending().lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
            return Err(format!("access.{kind}: the JS thread is gone ({st:?})"));
        }
        let r = rx.recv().map_err(|_| format!("access.{kind}: no reply"))?;
        match r.err {
            Some(e) => Err(e),
            None => Ok((r.value, r.bufs)),
        }
    }
}

/// facadeReply(id, err, value, buffers): the JS dispatcher's answer to one callout.
#[napi]
pub fn facade_reply(id: u32, err: Option<String>, value: Option<Value>, bufs: Option<Vec<Buffer>>) {
    let tx = pending().lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
    if let Some(tx) = tx {
        let _ = tx.send(CReply { err, value: value.unwrap_or(Value::Null), bufs: bufs.unwrap_or_default().into_iter().map(|b| b.to_vec()).collect() });
    }
}

// ── the facade object ─────────────────────────────────────────────────────

#[napi]
pub struct Facade {
    a: Arc<dyn Access>,
    /// true for shell/fn facades: their calls block on a JS host answer, so they must NOT occupy a
    /// libuv pool thread (the host's own work, e.g. localRunner's spawn, needs the pool).
    callout: bool,
    locks: Arc<Mutex<HashMap<u32, acc::LockGuard>>>,
}

fn facade_of(a: impl Access + 'static, callout: bool) -> Facade {
    Facade { a: Arc::new(a), callout, locks: Default::default() }
}

fn gen_err(e: String) -> napi::Error {
    napi::Error::from_reason(e)
}

fn dialect_of(d: &str) -> Result<ShellDialect, napi::Error> {
    match d {
        "bash" => Ok(ShellDialect::Bash),
        "powershell" => Ok(ShellDialect::PowerShell),
        _ => Err(gen_err(format!("unknown shell dialect: {d}"))),
    }
}

/// facadeLocalFs(root)
#[napi]
pub fn facade_local_fs(root: String) -> napi::Result<Facade> {
    Ok(facade_of(acc::LocalFs::new(&root).map_err(gen_err)?, false))
}

/// facadeShell({ root, dialect }, cb) — cb(id, 'run', { script, stdin }) answers facadeReply(id, err, outString).
#[napi]
pub fn facade_shell(env: Env, desc: Value, cb: JsFunction) -> napi::Result<Facade> {
    let co = Callout::new(&env, cb)?;
    let run: Runner = Arc::new(move |script: &str, stdin: &[u8]| {
        let (v, _) = co.call("run", json!({"script": script, "stdin": String::from_utf8_lossy(stdin)}))?;
        Ok(v.as_str().map(String::from).unwrap_or_else(|| v.to_string()))
    });
    let dialect = dialect_of(desc["dialect"].as_str().unwrap_or("bash"))?;
    Ok(facade_of(Shell::new(desc["root"].as_str().unwrap_or(""), run, dialect).map_err(gen_err)?, true))
}

/// A host mtime as f64: numbers pass; anything else (string, null) is 0 (PARITY.md).
fn num(v: &Value) -> f64 {
    v.as_f64().unwrap_or(0.0)
}

fn rows_of(v: &Value) -> Vec<FileRow> {
    v.as_array()
        .map(|a| {
            a.iter()
                .map(|r| FileRow {
                    path: r["path"].as_str().unwrap_or("").to_string(),
                    size: r["size"].as_f64().unwrap_or(0.0) as u64,
                    mtime: num(&r["mtime"]),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// facadeFn({ kind, root, stat }, cb) — cb(id, 'list'|'read'|'stat', args): the host's own functions.
/// list → [{path,size,mtime}], read → { paths } + buffers, stat → [[path, row|null]].
#[napi]
pub fn facade_fn(env: Env, desc: Value, cb: JsFunction) -> napi::Result<Facade> {
    let co = Callout::new(&env, cb)?;
    let (c1, c2, c3) = (co.clone(), co.clone(), co);
    let list: fn_access::ListFn = Box::new(move |o: &ListOpts| {
        let args = match &o.under {
            Some(u) => json!({"under": u}),
            None => json!({}),
        };
        Ok(rows_of(&c1.call("list", args)?.0))
    });
    let read: fn_access::ReadFn = Box::new(move |paths: &[String]| {
        let (v, bufs) = c2.call("read", json!({"paths": paths}))?;
        let names: Vec<String> = v["paths"].as_array().map(|a| a.iter().map(|p| p.as_str().unwrap_or("").to_string()).collect()).unwrap_or_default();
        Ok(names.into_iter().zip(bufs).collect())
    });
    let stat: Option<fn_access::StatFn> = if desc["stat"].as_bool() == Some(true) {
        Some(Box::new(move |paths: &[String], hash: bool| {
            let (v, _) = c3.call("stat", json!({"paths": paths, "hash": hash}))?;
            Ok(v.as_array()
                .map(|a| {
                    a.iter()
                        .map(|e| {
                            let r = &e[1];
                            let row = if r.is_null() {
                                StatRow::Missing
                            } else {
                                StatRow::Found {
                                    size: r["size"].as_f64().unwrap_or(0.0) as u64,
                                    mtime: num(&r["mtime"]),
                                    hash: r["hash"].as_str().map(String::from),
                                }
                            };
                            (e[0].as_str().unwrap_or("").to_string(), row)
                        })
                        .collect()
                })
                .unwrap_or_default())
        }))
    } else {
        None
    };
    let root = desc["root"].as_str().map(String::from);
    let kind = desc["kind"].as_str().unwrap_or("fn");
    Ok(facade_of(fn_access::fn_access(list, read, stat).with_kind(kind, root), true))
}

pub enum Out {
    Json(Value),
    Read(Vec<(String, Vec<u8>)>),
}

pub struct FacadeCall {
    a: Arc<dyn Access>,
    locks: Arc<Mutex<HashMap<u32, acc::LockGuard>>>,
    method: String,
    args: Value,
    bufs: Vec<Vec<u8>>,
}

fn strs(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().map(|p| p.as_str().map(String::from).unwrap_or_else(|| p.to_string())).collect()).unwrap_or_default()
}

fn commit_json(r: acc::CommitResult) -> Value {
    let mut m = Map::new();
    m.insert("outcome".into(), r.outcome.as_str().into());
    if let Some(h) = r.hash {
        m.insert("hash".into(), h.into());
    }
    if let Some(h) = r.current {
        m.insert("current".into(), h.into());
    }
    if let Some(e) = r.error {
        m.insert("error".into(), e.into());
    }
    Value::Object(m)
}

impl FacadeCall {
    fn run(&mut self) -> Result<Out, String> {
        let a = &self.a;
        let args = &self.args;
        Ok(Out::Json(match self.method.as_str() {
            "list" => {
                let skip = match args.get("skip") {
                    None => DEFAULT_SKIP.iter().map(|s| s.to_string()).collect(),
                    Some(v) => strs(v),
                };
                let under = args["under"].as_str().filter(|s| !s.is_empty()).map(String::from);
                let limit = args["limit"].as_f64().filter(|n| *n > 0.0).map_or(0, |n| n.floor() as usize);
                let out = a.list(&ListOpts { skip, under, limit })?;
                let rows: Vec<Value> = out.rows.iter().map(|r| json!({"path": r.path, "size": r.size, "mtime": r.mtime})).collect();
                json!({"rows": rows, "truncated": out.truncated})
            }
            "stat" => {
                let rows = a.stat(&strs(&args["paths"]), args["hash"].as_bool() == Some(true))?;
                Value::Array(
                    rows.into_iter()
                        .map(|(p, r)| match r {
                            StatRow::Missing => json!([p, null]),
                            StatRow::Found { size, mtime, hash } => {
                                let mut m = Map::new();
                                m.insert("size".into(), size.into());
                                m.insert("mtime".into(), json!(mtime));
                                if let Some(h) = hash {
                                    m.insert("hash".into(), h.into());
                                }
                                json!([p, Value::Object(m)])
                            }
                        })
                        .collect(),
                )
            }
            "read" => return a.read(&strs(&args["paths"])).map(Out::Read),
            "commit" => {
                let path = args["path"].as_str().unwrap_or("");
                let bytes = self.bufs.first().map(|b| b.as_slice()).unwrap_or(&[]);
                let opts = CommitOpts { expected_hash: args["expectedHash"].as_str().filter(|s| !s.is_empty()).map(String::from), create: args["create"].as_bool() == Some(true) };
                match a.commit(path, bytes, &opts) {
                    Some(r) => commit_json(r?),
                    None => return Err("commit is not supported by this facade".into()),
                }
            }
            "lock" => match a.lock(args["path"].as_str().unwrap_or(""), args["id"].as_str().unwrap_or(""))? {
                LockOutcome::Held(g) => {
                    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed).max(1);
                    self.locks.lock().unwrap_or_else(|e| e.into_inner()).insert(id, g);
                    json!({"ok": true, "lockId": id})
                }
                LockOutcome::Failed(reason) => json!({"ok": false, "reason": reason}),
            },
            "release" => {
                let id = args["lockId"].as_u64().unwrap_or(0) as u32;
                let g = self.locks.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
                if let Some(g) = g {
                    g.release();
                }
                Value::Null
            }
            "remove" => {
                a.remove(&strs(&args["paths"]))?;
                Value::Null
            }
            "exec" => {
                let o = a.exec(args["command"].as_str().unwrap_or(""))?;
                json!({"ok": o.ok, "out": o.out})
            }
            "filesContaining" => match a.files_containing(&strs(&args["paths"]), args["text"].as_str().unwrap_or(""), args["ignoreCase"].as_bool() == Some(true))? {
                Some(set) => Value::Array(set.into_iter().map(Value::String).collect()),
                None => Value::Null,
            },
            m => return Err(format!("unknown facade method {m}")),
        }))
    }
}

impl Task for FacadeCall {
    type Output = Out;
    type JsValue = JsUnknown;
    fn compute(&mut self) -> napi::Result<Out> {
        run_guarded(self)
    }
    fn resolve(&mut self, env: Env, out: Out) -> napi::Result<JsUnknown> {
        out_to_js(env, out)
    }
}

fn run_guarded(c: &mut FacadeCall) -> napi::Result<Out> {
    match std::panic::catch_unwind(AssertUnwindSafe(|| c.run())) {
        Ok(r) => r.map_err(gen_err),
        Err(p) => Err(gen_err(
            p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_else(|| "panic".into()),
        )),
    }
}

fn out_to_js(env: Env, out: Out) -> napi::Result<JsUnknown> {
    {
        match out {
            Out::Json(v) => crate::js_value(&env, v),
            Out::Read(files) => {
                let mut paths = env.create_array_with_length(files.len())?;
                let mut bufs = env.create_array_with_length(files.len())?;
                for (i, (p, b)) in files.into_iter().enumerate() {
                    paths.set_element(i as u32, env.create_string(&p)?)?;
                    bufs.set_element(i as u32, env.create_buffer_with_data(b)?.into_raw())?;
                }
                let mut pair = env.create_array_with_length(2)?;
                pair.set_element(0, paths)?;
                pair.set_element(1, bufs)?;
                Ok(pair.into_unknown())
            }
        }
    }
}

#[napi]
impl Facade {
    /// call(method, args, buffers) → Promise (list/stat/read/commit/lock/release/remove/exec/filesContaining).
    /// localFs: libuv pool (pure IO). shell/fn: a dedicated thread per call (it blocks on the JS host),
    /// settled through a deferred, so host work that needs the pool can never be starved.
    #[napi(ts_return_type = "Promise<unknown>")]
    pub fn call(&self, env: Env, method: String, args: Option<Value>, bufs: Option<Vec<Buffer>>) -> napi::Result<JsObject> {
        let mut call = FacadeCall {
            a: self.a.clone(),
            locks: self.locks.clone(),
            method,
            args: args.unwrap_or(Value::Null),
            bufs: bufs.unwrap_or_default().into_iter().map(|b| b.to_vec()).collect(),
        };
        if !self.callout {
            return Ok(env.spawn(call)?.promise_object());
        }
        let (deferred, promise) = env.create_deferred::<JsUnknown, Box<dyn FnOnce(Env) -> napi::Result<JsUnknown>>>()?;
        std::thread::Builder::new()
            .name("okcode-facade".into())
            .spawn(move || match run_guarded(&mut call) {
                Ok(out) => deferred.resolve(Box::new(move |env| out_to_js(env, out))),
                Err(e) => deferred.reject(e),
            })
            .map_err(|e| gen_err(e.to_string()))?;
        Ok(promise)
    }
}

// ── helpers ───────────────────────────────────────────────────────────────

/// facadeSha1(buffer): uppercase hex SHA-1.
#[napi]
pub fn facade_sha1(b: Buffer) -> String {
    acc::common::sha1_upper(&b)
}

/// facadeLocalRun(dialect, script, stdin) → Promise<string>: shell.localRunner's spawn, on its own
/// thread (never the libuv pool: pool threads may be blocked in facade callouts waiting on this).
#[napi(ts_return_type = "Promise<string>")]
pub fn facade_local_run(env: Env, dialect: String, script: String, stdin: Option<String>) -> napi::Result<JsObject> {
    let d = dialect_of(&dialect)?;
    let stdin = stdin.unwrap_or_default();
    let (deferred, promise) = env.create_deferred::<String, Box<dyn FnOnce(Env) -> napi::Result<String>>>()?;
    std::thread::Builder::new()
        .name("okcode-localrun".into())
        .spawn(move || {
            let run = acc::local_runner(d);
            match std::panic::catch_unwind(AssertUnwindSafe(|| run(&script, stdin.as_bytes()))) {
                Ok(Ok(out)) => deferred.resolve(Box::new(move |_| Ok(out))),
                Ok(Err(e)) => deferred.reject(gen_err(e)),
                Err(_) => deferred.reject(gen_err("local runner panicked".into())),
            }
        })
        .map_err(|e| gen_err(e.to_string()))?;
    Ok(promise)
}
