//! okcode-okdb: okcode's client of okdb's stable C bridge (include/okdb_bridge.h, ABI 1.0).
//! okdb is never compiled in: `Bridge::load` dlopens the .node that @kedem/okdb installs.
//!
//! - [`Bridge`]: the loaded table (ABI and struct_size checked).
//! - [`Handle`]: one okdb instance, `open`ed (the CLI: okdb's own owner thread) or `attach`ed to a JS host's instance (Node).
//!   `call` / `batch` take requests in okdb's JS shapes (`[scope, env, method, args]` as JSON in okcode's value
//!   conventions, see [`wire`]); `submit` works from any thread.
//! - [`Host`]: what okcode serves when okdb calls back (`$fn` ids: the headedChunks strategy, the content resolvers).
pub mod ffi;
pub mod locate;
pub mod wire;

use serde_json::{json, Value};
use std::ffi::{c_char, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};

/// An okdb error as the bridge reports it: `{code, message, details?}` plus the status class.
#[derive(Clone, Debug)]
pub struct BridgeError {
    pub status: i32,
    pub code: String,
    pub message: String,
    pub details: Value,
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl BridgeError {
    fn new(status: i32, code: &str, message: impl Into<String>) -> BridgeError {
        BridgeError { status, code: code.into(), message: message.into(), details: Value::Null }
    }
}

pub type Result<T> = std::result::Result<T, BridgeError>;

/// The loaded bridge: keep the library alive as long as any handle.
pub struct Bridge {
    /// None when the table came from the host (`db._native.bridgeTable()`): the host keeps the image loaded
    _lib: Option<libloading::Library>,
    t: &'static ffi::OkdbBridgeV1,
    pub path: PathBuf,
}

// The table is immutable function pointers; every call states its own threading rules.
unsafe impl Send for Bridge {}
unsafe impl Sync for Bridge {}

impl Bridge {
    /// dlopen okdb's .node and take its ABI-1 table. Inside Node, the path `require` loaded gives the same image.
    pub fn load(path: &Path) -> Result<Arc<Bridge>> {
        let lib = unsafe { libloading::Library::new(path) }.map_err(|e| BridgeError::new(ffi::OKDB_INVALID, "BRIDGE_LOAD_FAILED", format!("{}: {e}", path.display())))?;
        let get: libloading::Symbol<ffi::okdb_bridge_fn> =
            unsafe { lib.get(b"okdb_bridge\0") }.map_err(|_| BridgeError::new(ffi::OKDB_INVALID, "ABI_UNSUPPORTED", format!("{} exports no okdb_bridge (okdb too old)", path.display())))?;
        let t = unsafe { get(ffi::OKDB_BRIDGE_ABI_MAJOR) };
        let t = Self::check(t)?;
        Ok(Arc::new(Bridge { _lib: Some(lib), t, path: path.to_path_buf() }))
    }

    /// The table a JS host already holds (`db._native.bridgeTable()`, the address as a BigInt): no dlopen, the same image
    /// as the host's okdb. Safety: `addr` must be that address, and the host's addon stays loaded for the process lifetime.
    pub unsafe fn from_table(addr: u64, lib_path: Option<PathBuf>) -> Result<Arc<Bridge>> {
        let t = Self::check(addr as usize as *const ffi::OkdbBridgeV1)?;
        Ok(Arc::new(Bridge { _lib: None, t, path: lib_path.unwrap_or_default() }))
    }

    fn check(t: *const ffi::OkdbBridgeV1) -> Result<&'static ffi::OkdbBridgeV1> {
        if t.is_null() {
            return Err(BridgeError::new(ffi::OKDB_INVALID, "ABI_UNSUPPORTED", "no okdb bridge table"));
        }
        let t: &'static ffi::OkdbBridgeV1 = unsafe { &*t };
        if t.abi_major != ffi::OKDB_BRIDGE_ABI_MAJOR {
            return Err(BridgeError::new(ffi::OKDB_INVALID, "ABI_UNSUPPORTED", format!("okdb bridge ABI {} (okcode speaks {})", t.abi_major, ffi::OKDB_BRIDGE_ABI_MAJOR)));
        }
        // the whole 1.0 table must be there; 1.1 / 1.2 fields are used only when struct_size covers them
        if (t.struct_size as usize) < ffi::V1_0_SIZE {
            return Err(BridgeError::new(ffi::OKDB_INVALID, "ABI_UNSUPPORTED", format!("okdb bridge table too small ({} < {})", t.struct_size, ffi::V1_0_SIZE)));
        }
        Ok(t)
    }

    /// ABI 1.1: submit_batch
    pub fn has_submit_batch(&self) -> bool {
        self.t.struct_size as usize >= ffi::V1_1_SIZE
    }

    /// ABI 1.2: read (env reads from any thread, no owner hop)
    pub fn has_read(&self) -> bool {
        self.t.struct_size as usize >= ffi::V1_2_SIZE
    }

    /// okdb's package version ("3.0.0")
    pub fn version(&self) -> String {
        unsafe { CStr::from_ptr((self.t.version)()) }.to_string_lossy().into_owned()
    }

    /// Open a standalone instance. `owner_thread`: okdb runs the instance on its own thread (calls from any thread
    /// are forwarded); `defer`: construct only, finish with [`Handle::finish_open`] after registering callbacks.
    pub fn open(self: &Arc<Self>, path: &Path, opts: &Value, owner_thread: bool, defer: bool) -> Result<Handle> {
        let p = CString::new(path.to_string_lossy().as_bytes()).map_err(|_| BridgeError::new(ffi::OKDB_INVALID, "BAD_REQUEST", "path contains NUL"))?;
        let o = if opts.is_null() { Vec::new() } else { wire::encode(opts) };
        let flags = if owner_thread { ffi::OKDB_OPEN_OWNER_THREAD } else { 0 } | if defer { ffi::OKDB_OPEN_DEFER } else { 0 };
        let (mut h, mut err, mut err_len) = (0u64, std::ptr::null_mut::<u8>(), 0usize);
        let st = unsafe { (self.t.open)(p.as_ptr(), if o.is_empty() { std::ptr::null() } else { o.as_ptr() }, o.len(), flags, &mut h, &mut err, &mut err_len) };
        if st != ffi::OKDB_OK {
            return Err(self.take_err(st, err, err_len));
        }
        Ok(Handle { b: self.clone(), h, owner_thread, host: std::sync::Mutex::new(None) })
    }

    /// Attach to a JS host's instance (`db._native.bridgeHandle()`), ON THAT INSTANCE'S JS THREAD. close() only detaches.
    pub fn attach(self: &Arc<Self>, native_handle: u64) -> Result<Handle> {
        let (mut h, mut err, mut err_len) = (0u64, std::ptr::null_mut::<u8>(), 0usize);
        let st = unsafe { (self.t.attach)(native_handle, &mut h, &mut err, &mut err_len) };
        if st != ffi::OKDB_OK {
            return Err(self.take_err(st, err, err_len));
        }
        Ok(Handle { b: self.clone(), h, owner_thread: false, host: std::sync::Mutex::new(None) })
    }

    /// Run okdb's CLI in this process (`okdb …`).
    pub fn cli_main(&self, args: &[String]) -> i32 {
        let c: Vec<CString> = args.iter().map(|a| CString::new(a.as_bytes()).unwrap_or_default()).collect();
        let p: Vec<*const c_char> = c.iter().map(|s| s.as_ptr()).collect();
        unsafe { (self.t.cli_main)(p.len() as i32, p.as_ptr()) }
    }

    /// An okdb-owned reply buffer → JSON (freed here).
    fn take(&self, ptr: *mut u8, len: usize) -> std::result::Result<Value, String> {
        if ptr.is_null() {
            return Ok(Value::Null);
        }
        let v = wire::decode(unsafe { std::slice::from_raw_parts(ptr, len) });
        unsafe { (self.t.free)(ptr, len) };
        v
    }

    fn take_err(&self, status: i32, ptr: *mut u8, len: usize) -> BridgeError {
        match self.take(ptr, len) {
            Ok(env) => envelope(status, env).err().unwrap_or_else(|| BridgeError::new(status, "PANIC", "okdb returned an error status with an ok envelope")),
            Err(e) => BridgeError::new(status, "BAD_REPLY", e),
        }
    }
}

/// `{"ok": v}` → Ok(v); `{"err": {code, message, details}}` → Err.
fn envelope(status: i32, env: Value) -> Result<Value> {
    match env {
        Value::Object(mut m) => {
            if let Some(v) = m.remove("ok") {
                return Ok(v);
            }
            let e = m.remove("err").unwrap_or(Value::Null);
            Err(BridgeError {
                status: if status == ffi::OKDB_OK { ffi::OKDB_ERR } else { status },
                code: e.get("code").and_then(Value::as_str).unwrap_or("Error").to_string(),
                message: e.get("message").and_then(Value::as_str).unwrap_or("").to_string(),
                details: e.get("details").cloned().unwrap_or(Value::Null),
            })
        }
        _ => Err(BridgeError::new(status, "BAD_REPLY", "okdb reply is not an {ok}/{err} envelope")),
    }
}

/// One okdb instance over the bridge.
pub struct Handle {
    b: Arc<Bridge>,
    h: u64,
    /// opened with OKDB_OPEN_OWNER_THREAD: dispatch from any thread is fine (okdb forwards)
    pub owner_thread: bool,
    host: std::sync::Mutex<Option<Box<HostBox>>>,
}

unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

/// `[scope, env, method, args]`
pub fn request(scope: &str, env: Option<&str>, method: &str, args: Vec<Value>) -> Value {
    json!([scope, env, method, args])
}

impl Handle {
    /// One request, synchronously (owner thread, or any thread on an owner-thread handle).
    pub fn call(&self, scope: &str, env: Option<&str>, method: &str, args: Vec<Value>) -> Result<Value> {
        self.raw(&request(scope, env, method, args))
    }

    pub fn raw(&self, req: &Value) -> Result<Value> {
        let r = wire::encode(req);
        let (mut out, mut len) = (std::ptr::null_mut::<u8>(), 0usize);
        let st = unsafe { (self.b.t.dispatch)(self.h, r.as_ptr(), r.len(), &mut out, &mut len) };
        let env = self.b.take(out, len).map_err(|e| BridgeError::new(st, "BAD_REPLY", e))?;
        envelope(st, env)
    }

    /// Many requests as ONE write group: one result per item (a failing item fails alone).
    pub fn batch(&self, reqs: &[Value]) -> Result<Vec<Result<Value>>> {
        let r = wire::encode(&Value::Array(reqs.to_vec()));
        let (mut out, mut len) = (std::ptr::null_mut::<u8>(), 0usize);
        let st = unsafe { (self.b.t.dispatch_batch)(self.h, r.as_ptr(), r.len(), &mut out, &mut len) };
        let env = self.b.take(out, len).map_err(|e| BridgeError::new(st, "BAD_REPLY", e))?;
        match envelope(st, env)? {
            Value::Array(items) => Ok(items.into_iter().map(|e| envelope(ffi::OKDB_OK, e)).collect()),
            _ => Err(BridgeError::new(st, "BAD_REPLY", "batch reply is not an array")),
        }
    }

    /// From ANY thread: queue the request and wait for its completion on the owner thread. Never call it from the owner
    /// thread of an attached/inline handle (the completion would need that very thread): use [`call`](Self::call) there.
    pub fn submit_wait(&self, req: &Value) -> Result<Value> {
        let r = wire::encode(req);
        let (tx, rx) = mpsc::sync_channel::<(i32, std::result::Result<Value, String>)>(1);
        let ctx = Box::into_raw(Box::new(tx)) as *mut c_void;
        let st = unsafe { (self.b.t.submit)(self.h, r.as_ptr(), r.len(), done_trampoline, ctx) };
        if st != ffi::OKDB_OK {
            drop(unsafe { Box::from_raw(ctx as *mut mpsc::SyncSender<(i32, std::result::Result<Value, String>)>) });
            return Err(BridgeError::new(st, if st == ffi::OKDB_CLOSED { "CLOSED" } else { "BAD_HANDLE" }, "okdb did not queue the request"));
        }
        match rx.recv() {
            Ok((st, Ok(env))) => envelope(st, env),
            Ok((st, Err(e))) => Err(BridgeError::new(st, "BAD_REPLY", e)),
            Err(_) => Err(BridgeError::new(ffi::OKDB_CLOSED, "CLOSED", "okdb dropped the request")),
        }
    }

    /// ABI 1.1: many requests as ONE write group from ANY thread; waits for the completion on the owner thread.
    pub fn submit_batch_wait(&self, reqs: &[Value]) -> Result<Vec<Result<Value>>> {
        if !self.b.has_submit_batch() {
            return Err(BridgeError::new(ffi::OKDB_UNSUPPORTED, "ABI_UNSUPPORTED", "okdb bridge < 1.1: no submit_batch"));
        }
        let r = wire::encode(&Value::Array(reqs.to_vec()));
        let (tx, rx) = mpsc::sync_channel::<(i32, std::result::Result<Value, String>)>(1);
        let ctx = Box::into_raw(Box::new(tx)) as *mut c_void;
        let st = unsafe { (self.b.t.submit_batch)(self.h, r.as_ptr(), r.len(), done_trampoline, ctx) };
        if st != ffi::OKDB_OK {
            drop(unsafe { Box::from_raw(ctx as *mut mpsc::SyncSender<(i32, std::result::Result<Value, String>)>) });
            return Err(BridgeError::new(st, if st == ffi::OKDB_CLOSED { "CLOSED" } else { "BAD_HANDLE" }, "okdb did not queue the batch"));
        }
        let env = match rx.recv() {
            Ok((st, Ok(env))) => envelope(st, env)?,
            Ok((st, Err(e))) => return Err(BridgeError::new(st, "BAD_REPLY", e)),
            Err(_) => return Err(BridgeError::new(ffi::OKDB_CLOSED, "CLOSED", "okdb dropped the batch")),
        };
        match env {
            Value::Array(items) => Ok(items.into_iter().map(|e| envelope(ffi::OKDB_OK, e)).collect()),
            _ => Err(BridgeError::new(ffi::OKDB_ERR, "BAD_REPLY", "batch reply is not an array")),
        }
    }

    /// ABI 1.2: a read-only env request on THIS thread (committed data; no owner hop).
    pub fn read(&self, req: &Value) -> Result<Value> {
        let r = wire::encode(req);
        let (mut out, mut len) = (std::ptr::null_mut::<u8>(), 0usize);
        let st = unsafe { (self.b.t.read)(self.h, r.as_ptr(), r.len(), &mut out, &mut len) };
        let env = self.b.take(out, len).map_err(|e| BridgeError::new(st, "BAD_REPLY", e))?;
        envelope(st, env)
    }

    /// Inline handles: run due work for up to `budget_ms`; the ms until the next deadline (None: nothing scheduled).
    pub fn pump(&self, budget_ms: u32) -> Result<Option<u64>> {
        let mut next = -1i64;
        let st = unsafe { (self.b.t.pump)(self.h, budget_ms, &mut next) };
        if st != ffi::OKDB_OK {
            return Err(BridgeError::new(st, "PUMP_FAILED", "okdb pump failed"));
        }
        Ok(if next < 0 { None } else { Some(next as u64) })
    }

    /// Finish an OKDB_OPEN_DEFER open (after callbacks are registered).
    pub fn finish_open(&self) -> Result<Value> {
        self.call("bridge", None, "open", vec![])
    }

    /// `{abi, version, mode, open}`
    pub fn info(&self) -> Result<Value> {
        self.call("bridge", None, "info", vec![])
    }

    /// Install okcode's callback host for this handle's owner thread (replaces a previous one).
    pub fn set_host(&self, host: Arc<dyn Host>) -> Result<()> {
        let boxed = Box::new(HostBox { host });
        let vt = ffi::OkdbBridgeHostV1 {
            struct_size: std::mem::size_of::<ffi::OkdbBridgeHostV1>() as u32,
            reserved: 0,
            ctx: &*boxed as *const HostBox as *mut c_void,
            call: host_call,
            release: Some(host_release),
            free_reply: Some(host_free),
            drop: None,
        };
        let st = unsafe { (self.b.t.set_host)(self.h, &vt) };
        if st != ffi::OKDB_OK {
            return Err(BridgeError::new(st, "SET_HOST_FAILED", "okdb refused the callback host"));
        }
        // keep ctx alive as long as okdb may call it (replaced on the next set_host, freed with the handle)
        *self.host.lock().unwrap_or_else(|e| e.into_inner()) = Some(boxed);
        Ok(())
    }

    pub fn close(&self) -> Result<()> {
        let st = unsafe { (self.b.t.close)(self.h) };
        if st != ffi::OKDB_OK && st != ffi::OKDB_CLOSED {
            return Err(BridgeError::new(st, if st == ffi::OKDB_WRONG_THREAD { "WRONG_THREAD" } else { "CLOSE_FAILED" }, format!("okdb close failed (status {st})")));
        }
        Ok(())
    }

    pub fn bridge(&self) -> &Arc<Bridge> {
        &self.b
    }
}

unsafe extern "C" fn done_trampoline(ctx: *mut c_void, status: i32, reply: *const u8, len: usize) {
    let tx = Box::from_raw(ctx as *mut mpsc::SyncSender<(i32, std::result::Result<Value, String>)>);
    let v = if reply.is_null() { Ok(Value::Null) } else { wire::decode(std::slice::from_raw_parts(reply, len)) };
    let _ = tx.send((status, v));
}

// ── host callbacks ─────────────────────────────────────────────────────────

/// What okcode serves when okdb calls back. `op`/`args` as okdb_bridge README "Host ops" (e.g. "call" [fnId, …args]).
pub trait Host: Send + Sync {
    fn call(&self, op: &str, args: Value) -> std::result::Result<Value, (String, String)>;
    fn release(&self, _id: u64) {}
}

struct HostBox {
    host: Arc<dyn Host>,
}

unsafe extern "C" fn host_call(ctx: *mut c_void, op: *const c_char, args: *const u8, args_len: usize, out: *mut *mut u8, out_len: *mut usize) -> i32 {
    let hb = &*(ctx as *const HostBox);
    let op = CStr::from_ptr(op).to_string_lossy().into_owned();
    let args = if args.is_null() { Ok(Value::Array(vec![])) } else { wire::decode(std::slice::from_raw_parts(args, args_len)) };
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match args {
        Ok(a) => hb.host.call(&op, a),
        Err(e) => Err(("BAD_REQUEST".into(), e)),
    }))
    .unwrap_or_else(|_| Err(("PANIC".into(), format!("okcode host callback {op} panicked"))));
    let (st, env) = match r {
        Ok(v) => (ffi::OKDB_OK, json!({ "ok": v })),
        Err((code, message)) => (ffi::OKDB_ERR, json!({ "err": { "code": code, "message": message } })),
    };
    let bytes = wire::encode(&env).into_boxed_slice();
    let len = bytes.len();
    if !out.is_null() {
        *out = Box::into_raw(bytes) as *mut u8;
        *out_len = len;
    }
    st
}

unsafe extern "C" fn host_release(ctx: *mut c_void, id: u64) {
    let hb = &*(ctx as *const HostBox);
    hb.host.release(id);
}

unsafe extern "C" fn host_free(_ctx: *mut c_void, ptr: *mut u8, len: usize) {
    if !ptr.is_null() {
        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len)));
    }
}
