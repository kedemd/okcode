//! okcode-node — the napi binding of okcode-core.
//!
//! JS layer: lib/ (workspace.js, store.js, okcode.js, tools.js and the pure wrappers; lib/native/
//! loads this addon and converts arguments, results and errors).
//!
//! Every workspace/store API call runs on the libuv threadpool (`AsyncTask`); the core is sync with
//! interior locking. Errors cross as JSON in the napi error message (`{"$okcode":1,code,message,extra}`)
//! and the JS layer rebuilds an `Error` with `code` and the extra properties. A panic in the core
//! is caught and becomes an OKCODE_PANIC error, never an abort.
mod access_bridge;
mod access_api;
mod okcode;
mod okdb;
mod store;
mod tools;

use napi::bindgen_prelude::*;
use napi::Task;
use napi_derive::napi;
use okcode_core::host::{NativeWs, ToolWorkspace};
use okcode_core::workspace::Caps;
use okcode_core::{Access, Workspace};
use serde_json::{json, Value};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

pub use access_bridge::AccessHandle;

pub(crate) fn napi_err(e: okcode_core::Error) -> napi::Error {
    let v = json!({"$okcode": 1, "code": e.code, "message": e.message, "extra": Value::Object(e.extra)});
    napi::Error::new(Status::GenericFailure, v.to_string())
}

/// Run `f`, turning an Err or a panic into a JS-bound error.
pub(crate) fn guarded<T>(what: &str, f: impl FnOnce() -> okcode_core::Result<T>) -> napi::Result<T> {
    match std::panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(napi_err(e)),
        Err(p) => {
            let msg = p
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "panic".into());
            Err(napi_err(okcode_core::Error::code("OKCODE_PANIC", format!("{what}: {msg}"))))
        }
    }
}

// ── workspace ─────────────────────────────────────────────────────────────

#[napi]
pub struct WorkspaceHandle {
    pub(crate) ws: Arc<Workspace>,
}

#[napi]
impl WorkspaceHandle {
    #[napi(getter)]
    pub fn read_only(&self) -> bool {
        self.ws.read_only
    }
    #[napi(getter)]
    pub fn index(&self) -> String {
        self.ws.index_mode().to_string()
    }
}

pub struct OpenWorkspace {
    id: String,
    access: Arc<dyn Access>,
    caps: Caps,
    store: Option<Arc<okcode_core::Store>>,
    options: Value,
}

impl Task for OpenWorkspace {
    type Output = Arc<Workspace>;
    type JsValue = WorkspaceHandle;
    fn compute(&mut self) -> napi::Result<Self::Output> {
        let (id, access, caps, store, options) = (self.id.clone(), self.access.clone(), self.caps, self.store.take(), self.options.take());
        guarded("openWorkspace", move || Workspace::open(&id, access, caps, store, options))
    }
    fn resolve(&mut self, _env: Env, ws: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(WorkspaceHandle { ws })
    }
}

/// openWorkspace(id, access, store|null, options) → Promise<WorkspaceHandle>
#[napi(ts_return_type = "Promise<WorkspaceHandle>")]
pub fn open_workspace(id: String, access: &AccessHandle, store: Option<&store::StoreHandle>, options: Option<Value>) -> AsyncTask<OpenWorkspace> {
    AsyncTask::new(OpenWorkspace {
        id,
        access: access.access.clone(),
        caps: access.caps,
        store: store.map(|s| s.store.clone()),
        options: options.unwrap_or(Value::Null),
    })
}

pub struct WsCall {
    ws: Arc<Workspace>,
    method: String,
    args: Vec<Value>,
}

impl Task for WsCall {
    type Output = Value;
    type JsValue = napi::JsUnknown;
    fn compute(&mut self) -> napi::Result<Value> {
        let (ws, method, args) = (&self.ws, &self.method, &self.args);
        guarded(method, || ws.call(method, args))
    }
    fn resolve(&mut self, env: Env, out: Value) -> napi::Result<napi::JsUnknown> {
        js_value(&env, out)
    }
}

/// A serde_json::Value as a JS value, converted like the sync path (ToNapiValue for Value): integers
/// within +-2^53 become Numbers. Never `env.to_js_value`: napi's serde serializer turns a u64 above
/// u32::MAX (a ms timestamp) into a BigInt, which JSON.stringify refuses.
pub(crate) fn js_value(env: &Env, v: Value) -> napi::Result<napi::JsUnknown> {
    unsafe {
        let raw = <Value as ToNapiValue>::to_napi_value(env.raw(), v)?;
        napi::JsUnknown::from_napi_value(env.raw(), raw)
    }
}

fn args_of(args: Option<Value>) -> Vec<Value> {
    match args {
        Some(Value::Array(a)) => a,
        Some(Value::Null) | None => Vec::new(),
        Some(v) => vec![v],
    }
}

/// ws.<method>(...args) on the threadpool → Promise<result>
#[napi(ts_return_type = "Promise<unknown>")]
pub fn ws_call(ws: &WorkspaceHandle, method: String, args: Option<Value>) -> AsyncTask<WsCall> {
    AsyncTask::new(WsCall { ws: ws.ws.clone(), method, args: args_of(args) })
}

/// The sync methods (isLibraryPath, scanWarnings) on the JS thread. They must never touch the facade.
#[napi]
pub fn ws_call_sync(ws: &WorkspaceHandle, method: String, args: Option<Value>) -> napi::Result<Value> {
    let a = args_of(args);
    guarded(&method, || ws.ws.call(&method, &a))
}

/// workspace.js's pure exports (carveRegions, splitPiece, classifyCommit, parseRange, gitignoreMatches).
#[napi]
pub fn ws_pure(name: String, args: Option<Value>) -> napi::Result<Value> {
    let a = args_of(args);
    guarded(&name, || okcode_core::workspace::api::call_pure(&name, &a))
}

/// A lib/ module's pure export (okcode_core::pure), for the wrappers in lib/ (lib/native/pure.js).
#[napi]
pub fn pure_call(module: String, name: String, args: Option<Value>) -> napi::Result<Value> {
    let a = args_of(args);
    guarded(&name, || okcode_core::pure::call(&module, &name, &a))
}

/// The composed analysis extract() (handy for checks from JS).
#[napi]
pub fn extract(path: String, src: Option<String>) -> Value {
    okcode_core::analysis::extract(&path, src.as_deref())
}

// ── tool workspaces (shared by okcode.rs and tools.rs) ───────────────────────

/// A workspace as the native tool layer sees it (okcode_core::host::ToolWorkspace): the native
/// workspace itself (`wsToolHandle`), an okcode one (okcode.rs `okcodeToolWs`), or a bridged JS
/// object (tools.rs).
#[napi]
pub struct ToolWsHandle {
    pub(crate) tw: Arc<dyn ToolWorkspace>,
}

/// wsToolHandle(ws, root) — lib/workspace.js marks every api object with this (lazily).
#[napi]
pub fn ws_tool_handle(ws: &WorkspaceHandle, root: Option<String>) -> ToolWsHandle {
    ToolWsHandle { tw: Arc::new(NativeWs { ws: ws.ws.clone(), root }) }
}
