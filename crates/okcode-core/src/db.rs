//! The okdb database a store lives in, through okdb's stable C bridge.
//!
//! okcode never compiles okdb in: `okcode_okdb` dlopens the .node of okcode's `@kedem/okdb` dependency (the CLI,
//! `DbHost::new`) or takes the bridge table of the HOST's instance (Node library mode, `DbHost::attach`). Requests are
//! okdb's JS call shapes (`[scope, env, method, args]`) in okcode's JSON conventions ($buf, $date, $map, …).
//!
//! ONE INSTANCE PER PATH PER PROCESS: a process-wide registry keyed by the canonical path (refcounted; the last `close`
//! closes an instance okcode opened, and only detaches from a host's).
//!
//! Threading. An okdb instance is single-threaded; the bridge says where calls may run:
//! - opened (`new`): okdb runs its own owner thread (`OKDB_OPEN_OWNER_THREAD`); `call` from any thread is forwarded.
//! - attached (`attach`): the owner is the host's JS thread. On it, `call` runs inline; from any other thread the request
//!   is `submit`ted and the caller waits for its completion (which the JS loop runs).
//!
//! Callbacks okdb makes into okcode (the headedChunks chunk strategy, the content field resolvers) are `{"$fn": id}` markers
//! served by okcode's bridge host from a process-wide registry ([`register_callback`]). They run on the instance's owner
//! thread; a resolver must never wait on the JS thread (store.rs `Stash` / PENDING).
use crate::error::{Error, Result};
use okcode_okdb::{Bridge, BridgeError, Handle, Host};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

/// `new OKDB(path, options)` options (auth, role/processors, license, …) as JSON.
#[derive(Clone, Debug, Default)]
pub struct DbOptions {
    pub raw: Value,
}

/// okdb's error → okcode's (code, message, details).
pub fn berr(e: BridgeError) -> Error {
    let mut x = Error::code(e.code, e.message);
    if !e.details.is_null() {
        x.extra.insert("details".into(), e.details);
    }
    x
}

// ───────────────────────── callbacks okdb makes into okcode ─────────────────────────

/// A callback okdb calls with the arguments after the fn id (`host.call("call", [id, ...args])`).
pub type Callback = Arc<dyn Fn(&[Value]) -> std::result::Result<Value, (String, String)> + Send + Sync>;

fn callbacks() -> &'static RwLock<HashMap<u64, Callback>> {
    static C: OnceLock<RwLock<HashMap<u64, Callback>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

static NEXT_FN: AtomicU64 = AtomicU64::new(1);

/// Register a callback; its `{"$fn": id}` marker goes into a request where okdb's JS API takes a function.
pub fn register_callback(f: Callback) -> u64 {
    let id = NEXT_FN.fetch_add(1, Ordering::SeqCst);
    callbacks().write().unwrap_or_else(|e| e.into_inner()).insert(id, f);
    id
}

pub fn unregister_callback(id: u64) {
    callbacks().write().unwrap_or_else(|e| e.into_inner()).remove(&id);
}

/// `{"$fn": id}`
pub fn fn_marker(id: u64) -> Value {
    json!({ "$fn": id })
}

struct OkcodeHost;

impl Host for OkcodeHost {
    fn call(&self, op: &str, args: Value) -> std::result::Result<Value, (String, String)> {
        let a = match args {
            Value::Array(a) => a,
            _ => return Err(("BAD_REQUEST".into(), format!("host op {op}: args must be an array"))),
        };
        match op {
            "call" | "callAsync" => {
                let id = a.first().and_then(Value::as_u64).ok_or_else(|| ("BAD_REQUEST".to_string(), "call: no fn id".to_string()))?;
                let f = callbacks().read().unwrap_or_else(|e| e.into_inner()).get(&id).cloned();
                let f = f.ok_or_else(|| ("NOT_FOUND".to_string(), format!("okcode callback {id} is not registered")))?;
                let r = f(&a[1..])?;
                Ok(if op == "callAsync" { json!({ "$value": r }) } else { r })
            }
            other => Err(("UNSUPPORTED".into(), format!("okcode serves no host op {other}"))),
        }
    }
}

// ───────────────────────── the bridge (the CLI's dlopen) ─────────────────────────

/// okcode's own @kedem/okdb dependency's .node, loaded once (`okcode_okdb::locate`).
fn own_bridge() -> Result<Arc<Bridge>> {
    static B: OnceLock<std::result::Result<Arc<Bridge>, Error>> = OnceLock::new();
    B.get_or_init(|| {
        let path = okcode_okdb::locate::find_okdb_node().map_err(|tried| {
            Error::code(
                "OKCODE_OKDB_NOT_FOUND",
                format!(
                    "okcode needs @kedem/okdb ^3 (its native package for {}); set OKDB_NATIVE_BINARY or install it. Tried: {}",
                    okcode_okdb::locate::platform_key(),
                    tried.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
                ),
            )
        })?;
        Bridge::load(&path).map_err(berr)
    })
    .clone()
}

// ───────────────────────── the host ─────────────────────────

/// One call of a [`DbHost::call_batch`]: what [`DbHost::call`] takes.
#[derive(Clone, Debug)]
pub struct BatchCall {
    pub scope: String,
    pub env: Option<String>,
    pub method: String,
    pub args: Vec<Value>,
}

enum Mode {
    /// okcode opened it, okdb runs its owner thread: calls from any thread
    Owned,
    /// a host's instance: owner = this JS thread; `native` = its bridgeHandle (a reopened path is a NEW instance)
    Attached { js_thread: ThreadId, native: u64 },
}

pub struct DbHost {
    path: PathBuf,
    handle: Handle,
    mode: Mode,
    /// guarded by the registry lock
    refs: AtomicUsize,
    opened: Mutex<bool>,
    closed: AtomicBool,
}

#[derive(Default)]
struct Reg {
    map: HashMap<PathBuf, Arc<DbHost>>,
}

fn registry() -> &'static Mutex<Reg> {
    static R: OnceLock<Mutex<Reg>> = OnceLock::new();
    R.get_or_init(Default::default)
}

fn canonical(path: &Path) -> PathBuf {
    if let Ok(p) = std::fs::canonicalize(path) {
        return p;
    }
    let abs = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir().map(|d| d.join(path)).unwrap_or_else(|_| path.to_path_buf()) };
    match (abs.parent().and_then(|p| std::fs::canonicalize(p).ok()), abs.file_name()) {
        (Some(p), Some(n)) => p.join(n),
        _ => abs,
    }
}

fn closed_error() -> Error {
    Error::code("INVALID_STATE", "the okdb instance is closed")
}

/// A request ABI 1.2's `read` serves: scope env, a read method.
fn readable(req: &Value) -> bool {
    const READS: &[&str] = &["get", "getEntry", "has", "count", "getMany", "getRange", "getKeys", "getValues", "getCount", "getByPrefix", "getIndex", "byIndex", "countByIndex"];
    req.get(0).and_then(Value::as_str) == Some("env") && req.get(2).and_then(Value::as_str).is_some_and(|m| READS.contains(&m))
}

/// `[scope, env, method, args]`
fn request(scope: &str, env: Option<&str>, method: &str, args: Vec<Value>) -> Value {
    json!([scope, env, method, args])
}

impl DbHost {
    /// Get-or-create okcode's OWN instance at `path` (the CLI). Constructed deferred: callbacks can be registered before
    /// `open()` finishes it.
    pub fn new(path: &Path, opts: DbOptions) -> Result<Arc<DbHost>> {
        let key = canonical(path);
        let mut reg = registry().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(h) = reg.map.get(&key) {
            h.refs.fetch_add(1, Ordering::SeqCst);
            return Ok(h.clone());
        }
        let bridge = own_bridge()?;
        let handle = bridge.open(&key, &opts.raw, true, true).map_err(berr)?;
        handle.set_host(Arc::new(OkcodeHost)).map_err(berr)?;
        let host = Arc::new(DbHost { path: key.clone(), handle, mode: Mode::Owned, refs: AtomicUsize::new(1), opened: Mutex::new(false), closed: AtomicBool::new(false) });
        reg.map.insert(key, host.clone());
        Ok(host)
    }

    /// The HOST's instance (Node library mode), ON ITS JS THREAD: `table` = `db._native.bridgeTable()`, `native` =
    /// `db._native.bridgeHandle()`, `path` = `db.path`. okcode never opens, pumps or closes it (close detaches).
    ///
    /// # Safety
    /// `table` must be the address the host's okdb returned (its addon stays loaded).
    pub unsafe fn attach(table: u64, native: u64, path: &Path) -> Result<Arc<DbHost>> {
        let key = canonical(path);
        let mut reg = registry().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(h) = reg.map.get(&key) {
            // the same live host instance: share it; a closed or different one (the host closed and reopened the
            // path) is replaced by a fresh attachment
            if matches!(h.mode, Mode::Attached { native: n, .. } if n == native) && !h.closed.load(Ordering::SeqCst) {
                h.refs.fetch_add(1, Ordering::SeqCst);
                return Ok(h.clone());
            }
            if matches!(h.mode, Mode::Owned) && !h.closed.load(Ordering::SeqCst) {
                return Err(Error::code("DB_ALREADY_OPEN", format!("{} is already open by okcode itself", key.display())));
            }
        }
        let bridge = Bridge::from_table(table, None).map_err(berr)?;
        let handle = bridge.attach(native).map_err(berr)?;
        handle.set_host(Arc::new(OkcodeHost)).map_err(berr)?;
        let host = Arc::new(DbHost {
            path: key.clone(),
            handle,
            mode: Mode::Attached { js_thread: std::thread::current().id(), native },
            refs: AtomicUsize::new(1),
            opened: Mutex::new(true),
            closed: AtomicBool::new(false),
        });
        reg.map.insert(key, host.clone());
        Ok(host)
    }

    /// `db.open()` — finishes a deferred open (idempotent; a host's instance is already open).
    pub fn open(&self) -> Result<()> {
        let mut opened = self.opened.lock().unwrap_or_else(|e| e.into_inner());
        if *opened {
            return Ok(());
        }
        self.handle.finish_open().map_err(berr)?;
        *opened = true;
        Ok(())
    }

    /// `db.close()` — drops one reference; the last one closes okcode's own instance (or detaches from a host's).
    pub fn close(&self) -> Result<()> {
        {
            let mut reg = registry().lock().unwrap_or_else(|e| e.into_inner());
            let cur = self.refs.load(Ordering::SeqCst);
            if cur == 0 || self.closed.load(Ordering::SeqCst) {
                return Ok(());
            }
            self.refs.store(cur - 1, Ordering::SeqCst);
            if cur > 1 {
                return Ok(());
            }
            if reg.map.get(&self.path).is_some_and(|h| std::ptr::eq(Arc::as_ptr(h), self)) {
                reg.map.remove(&self.path);
            }
        }
        self.closed.store(true, Ordering::SeqCst);
        *self.opened.lock().unwrap_or_else(|e| e.into_inner()) = false;
        match self.mode {
            Mode::Owned => self.handle.close().map_err(berr),
            // detaching is owner-thread only; off it okcode just lets go (the host's instance owns everything)
            Mode::Attached { .. } if !self.on_owner() => Ok(()),
            Mode::Attached { .. } => self.handle.close().or_else(|e| if e.code == "CLOSED" { Ok(()) } else { Err(e) }).map_err(berr),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// true where a synchronous dispatch is allowed (attached: only on the JS thread)
    fn on_owner(&self) -> bool {
        match self.mode {
            Mode::Owned => true, // okdb forwards (an owner-thread handle)
            Mode::Attached { js_thread, .. } => std::thread::current().id() == js_thread,
        }
    }

    fn send(&self, req: &Value) -> Result<Value> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(closed_error());
        }
        // ABI 1.2: a committed-data env read runs right here, on any thread (no owner hop, no JS-thread wait)
        if readable(req) && self.handle.bridge().has_read() {
            return self.handle.read(req).map_err(berr);
        }
        if self.on_owner() {
            self.handle.raw(req).map_err(berr)
        } else {
            self.handle.submit_wait(req).map_err(berr)
        }
    }

    /// One request in okdb's JS shapes (`scope` ∈ db, env, fts, embeddings, engines, pipelines, licenses, …).
    pub fn call(&self, scope: &str, env: Option<&str>, method: &str, args: Vec<Value>) -> Result<Value> {
        if scope == "db" {
            match method {
                "open" => return self.open().map(|_| Value::Null),
                "close" => return self.close().map(|_| Value::Null),
                _ => {}
            }
        }
        if scope == "fts" && method == "ready" {
            return self.fts_ready(&args);
        }
        if scope == "fts" && method == "reset" {
            return self.fts_reset(&args);
        }
        self.send(&request(scope, env, method, args))
    }

    /// [`call`](Self::call), kept for the binding's sync methods.
    pub fn call_sync(&self, scope: &str, env: Option<&str>, method: &str, args: Vec<Value>) -> Result<Value> {
        self.call(scope, env, method, args)
    }

    /// Many requests as ONE write group (one outer txn per env, a nested one per item: a failing item fails alone).
    /// Off the owner thread of an attached instance the bridge has no batch submit: the items go one by one.
    pub fn call_batch(&self, _sync: bool, calls: Vec<BatchCall>) -> Result<Vec<Result<Value>>> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(closed_error());
        }
        let special = |c: &BatchCall| (c.scope == "db" && (c.method == "open" || c.method == "close")) || (c.scope == "fts" && (c.method == "ready" || c.method == "reset"));
        if calls.iter().any(special) {
            return Ok(calls.into_iter().map(|c| self.call(&c.scope, c.env.as_deref(), &c.method, c.args)).collect());
        }
        let reqs: Vec<Value> = calls.into_iter().map(|c| request(&c.scope, c.env.as_deref(), &c.method, c.args)).collect();
        // all reads: each on this thread (ABI 1.2), no owner involved
        if self.handle.bridge().has_read() && reqs.iter().all(readable) {
            return Ok(reqs.iter().map(|r| self.handle.read(r).map_err(berr)).collect());
        }
        let out = if self.on_owner() {
            self.handle.batch(&reqs)
        } else if self.handle.bridge().has_submit_batch() {
            // ABI 1.1: ONE write group from a threadpool thread (completed on the owner)
            self.handle.submit_batch_wait(&reqs)
        } else {
            return Ok(reqs.iter().map(|r| self.send(r)).collect());
        };
        Ok(out.map_err(berr)?.into_iter().map(|r| r.map_err(berr)).collect())
    }
}

// ───────────────────────── fts waits (on the CALLING thread) ─────────────────────────

const FTS_DEADLINE: Duration = Duration::from_millis(120_000);

fn env_name(v: Option<&Value>) -> Result<String> {
    match v {
        Some(Value::String(x)) => Ok(x.clone()),
        None | Some(Value::Null) => Ok("default".to_string()),
        _ => Err(Error::code("INVALID_ARGUMENT", "env must be an environment (or its name)")),
    }
}

impl DbHost {
    /// `fts.ready(type, name, env, timeout?)`: okdb's is asynchronous and never blocks other calls, so this waits on the
    /// CALLING thread, one short request at a time, leaving the owner free in between. Total deadline: the 4th argument
    /// (ms) or 120 s, then FTS_READY_TIMEOUT.
    fn fts_ready(&self, a: &[Value]) -> Result<Value> {
        let ty = a.first().and_then(Value::as_str).ok_or_else(|| Error::code("INVALID_ARGUMENT", "fts.ready: type"))?.to_string();
        let name = a.get(1).cloned().unwrap_or(Value::Null);
        let envname = env_name(a.get(2))?;
        let limit = a.get(3).and_then(Value::as_f64).filter(|t| *t > 0.0).map(|t| Duration::from_millis(t as u64)).unwrap_or(FTS_DEADLINE);
        let end = Instant::now() + limit;
        loop {
            match self.send(&request("fts", None, "ready", vec![json!(ty), name.clone(), json!(envname), json!(1)])) {
                Ok(v) => return Ok(v),
                Err(e) if e.code.as_deref() == Some("FTS_READY_TIMEOUT") => {
                    // on the JS thread of an attached instance nothing else could make progress while this spins
                    if Instant::now() >= end || (matches!(self.mode, Mode::Attached { .. }) && self.on_owner()) {
                        return Err(e);
                    }
                    std::thread::sleep(Duration::from_millis(2))
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// `fts.reset(type, name, clear, env)`: resolves once the rebuild has settled, as okdb-node `_build` does.
    fn fts_reset(&self, a: &[Value]) -> Result<Value> {
        let ty = a.first().and_then(Value::as_str).ok_or_else(|| Error::code("INVALID_ARGUMENT", "fts.reset: type"))?.to_string();
        let name = a.get(1).and_then(Value::as_str).unwrap_or("").to_string();
        let clear = a.get(2).cloned().unwrap_or(Value::Null);
        let envname = env_name(a.get(3))?;
        drive_build(self, &ty, &name, &envname, Some(clear)).map(|_| Value::Null)
    }
}

/// Start (`first` = the reset's `clear` argument) or continue (`None`: buildStep) an fts build until it settles:
/// BUILD_CONTINUE resumes with `buildStep` (after a short sleep when it says `wait`, the ~fts flush being behind);
/// FIELD_RESOLVER_PENDING parks nothing, so it is retried as a clearing `reset`. Shared by fts.reset and store.rs ensure_fts.
pub fn drive_build(db: &DbHost, ty: &str, name: &str, env: &str, first: Option<Value>) -> Result<()> {
    let end = Instant::now() + FTS_DEADLINE;
    let mut stepping = first.is_none();
    let mut clear = first.unwrap_or(Value::Bool(true));
    loop {
        let req = if stepping {
            request("fts", None, "buildStep", vec![json!(ty), json!(name), json!(env)])
        } else {
            request("fts", None, "reset", vec![json!(ty), json!(name), clear.clone(), json!(env)])
        };
        match db.send(&req) {
            Ok(Value::Bool(false)) => stepping = true,
            Ok(_) => return Ok(()),
            Err(e) if matches!(e.code.as_deref(), Some("FTS_BUILD_CONTINUE") | Some("BUILD_CONTINUE")) => {
                stepping = true;
                if e.extra.get("details").and_then(|d| d.get("wait")) == Some(&Value::Bool(true)) {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            Err(e) if e.code.as_deref() == Some("FIELD_RESOLVER_PENDING") => {
                if Instant::now() >= end {
                    return Err(e);
                }
                std::thread::sleep(Duration::from_millis(10));
                stepping = false;
                clear = Value::Bool(true);
            }
            Err(e) => return Err(e),
        }
        if Instant::now() >= end {
            return Err(Error::code("FTS_READY_TIMEOUT", format!("fts build of {ty} did not settle in {}s", FTS_DEADLINE.as_secs())));
        }
    }
}
