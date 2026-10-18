//! lib/okcode.js over okcode_core::Okcode.
//!
//! lib/okcode.js builds the db (`new OKDB(path, flags)`, unless the host passed one), registers the
//! function profiles' factories (lib/embedders.js preRegister) and then:
//!   okcodeOpen(dbHandle, { ownsDb, license, embedders: [{ name, cfg, fnKeys }], active, workspace }, warn)
//!     → Promise<OkcodeHandle>
//!   okcodeAddWorkspace(okc, id, accessHandle, { options, index, fts, accessKind, accessRoot })
//!     → Promise<{ ws: WorkspaceHandle, store: StoreHandle | null }>
//!   okcodeAddEmbedder(okc, { name, cfg, fnKeys }) → Promise<result>
//!   okcodeCall(okc, method, args) → Promise<result>   (Okcode::call, threadpool)
//!   okcodeCallSync(okc, method, args) → result        (workspaces, useEmbedder, active, embedders)
//!   okcodeToolWs(okc, id, root) → ToolWsHandle | null (tools over an okcode ws: ask = wsAsk)
use crate::access_bridge::AccessHandle;
use crate::okdb::DbHandle;
use crate::store::{log_bridge, StoreHandle};
use crate::{guarded, ToolWsHandle, WorkspaceHandle};
use napi::bindgen_prelude::*;
use napi::{Env, JsFunction, JsObject, Task};
use napi_derive::napi;
use okcode_core::okcode::{AddOptions, HostProfile, Okcode, OpenOptions, Opened};
use okcode_core::workspace::Caps;
use okcode_core::Access;
use serde_json::Value;
use std::sync::Arc;

#[napi]
pub struct OkcodeHandle {
    pub(crate) okc: Arc<Okcode>,
}

/// `{ name, cfg, fnKeys }` → HostProfile
fn host_profile(v: &Value) -> HostProfile {
    HostProfile {
        name: v["name"].as_str().unwrap_or_default().to_string(),
        cfg: v["cfg"].clone(),
        fn_keys: v["fnKeys"].as_array().map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect()).unwrap_or_default(),
    }
}

pub struct OpenOkcode {
    db: Arc<okcode_core::db::DbHost>,
    opts: Option<OpenOptions>,
}

impl Task for OpenOkcode {
    type Output = Arc<Okcode>;
    type JsValue = OkcodeHandle;
    fn compute(&mut self) -> napi::Result<Self::Output> {
        let opts = self.opts.take().unwrap_or_default();
        guarded("okcode.open", || Okcode::open(self.db.clone(), opts))
    }
    fn resolve(&mut self, _env: Env, okc: Self::Output) -> napi::Result<OkcodeHandle> {
        Ok(OkcodeHandle { okc })
    }
}

/// okcodeOpen(db, options, warn)
#[napi(ts_return_type = "Promise<OkcodeHandle>")]
pub fn okcode_open(env: Env, db: &DbHandle, options: Option<Value>, warn: Option<JsFunction>) -> napi::Result<AsyncTask<OpenOkcode>> {
    let o = options.unwrap_or(Value::Null);
    let opts = OpenOptions {
        owns_db: o["ownsDb"].as_bool().unwrap_or(false),
        license: o["license"].clone(),
        embedders: o["embedders"].as_array().map(|a| a.iter().map(host_profile).collect()).unwrap_or_default(),
        active: o["active"].as_str().map(String::from),
        active_other: match &o["active"] {
            Value::Null | Value::String(_) => None,
            v => Some(okcode_core::okcode::js_string(v)),
        },
        workspace: o["workspace"].clone(),
        log: log_bridge(&env, warn)?,
    };
    Ok(AsyncTask::new(OpenOkcode { db: db.host.clone(), opts: Some(opts) }))
}

pub struct AddWs {
    okc: Arc<Okcode>,
    id: String,
    access: Arc<dyn Access>,
    caps: Caps,
    opts: AddOptions,
}

impl Task for AddWs {
    type Output = Opened;
    type JsValue = JsObject;
    fn compute(&mut self) -> napi::Result<Opened> {
        let o = std::mem::take(&mut self.opts);
        guarded("addWorkspace", || self.okc.add_workspace(&self.id, self.access.clone(), self.caps, o))
    }
    fn resolve(&mut self, env: Env, out: Opened) -> napi::Result<JsObject> {
        let mut obj = env.create_object()?;
        obj.set("ws", WorkspaceHandle { ws: out.ws })?;
        match out.store {
            Some(store) => obj.set("store", StoreHandle { store })?,
            None => obj.set("store", Null)?,
        }
        Ok(obj)
    }
}

/// okcodeAddWorkspace(okc, id, access, { options, index, fts, accessKind, accessRoot })
#[napi(ts_return_type = "Promise<{ ws: WorkspaceHandle, store: StoreHandle | null }>")]
pub fn okcode_add_workspace(okc: &OkcodeHandle, id: String, access: &AccessHandle, options: Option<Value>) -> AsyncTask<AddWs> {
    let o = options.unwrap_or(Value::Null);
    let s = |k: &str| o[k].as_str().map(String::from);
    let opts = AddOptions { options: o["options"].clone(), index: o["index"].clone(), fts: o["fts"].clone(), access_kind: s("accessKind"), access_root: s("accessRoot") };
    AsyncTask::new(AddWs { okc: okc.okc.clone(), id, access: access.access.clone(), caps: access.caps, opts })
}

/// serde_json::Value that converts like the sync path (ToNapiValue for Value): integers within
/// +-2^53 become Numbers, never BigInt (napi's serde serializer makes u64 > u32::MAX a BigInt).
pub struct JsJson(Value);
impl TypeName for JsJson {
    fn type_name() -> &'static str {
        "unknown"
    }
    fn value_type() -> ValueType {
        ValueType::Unknown
    }
}
impl ToNapiValue for JsJson {
    unsafe fn to_napi_value(env: napi::sys::napi_env, v: Self) -> napi::Result<napi::sys::napi_value> {
        <Value as ToNapiValue>::to_napi_value(env, v.0)
    }
}

pub struct OkcodeCall {
    okc: Arc<Okcode>,
    method: String,
    args: Vec<Value>,
    /// addEmbedder's profile
    profile: Option<HostProfile>,
}

impl Task for OkcodeCall {
    type Output = Value;
    type JsValue = JsJson;
    fn compute(&mut self) -> napi::Result<Value> {
        if let Some(p) = self.profile.take() {
            return guarded("addEmbedder", || self.okc.add_embedder(p));
        }
        guarded(&self.method, || self.okc.call(&self.method, &self.args))
    }
    fn resolve(&mut self, _env: Env, out: Value) -> napi::Result<JsJson> {
        Ok(JsJson(out))
    }
}

/// okcodeAddEmbedder(okc, { name, cfg, fnKeys })
#[napi(ts_return_type = "Promise<unknown>")]
pub fn okcode_add_embedder(okc: &OkcodeHandle, profile: Value) -> AsyncTask<OkcodeCall> {
    AsyncTask::new(OkcodeCall { okc: okc.okc.clone(), method: "addEmbedder".into(), args: Vec::new(), profile: Some(host_profile(&profile)) })
}

#[napi(ts_return_type = "Promise<unknown>")]
pub fn okcode_call(okc: &OkcodeHandle, method: String, args: Option<Vec<Value>>) -> AsyncTask<OkcodeCall> {
    AsyncTask::new(OkcodeCall { okc: okc.okc.clone(), method, args: args.unwrap_or_default(), profile: None })
}

/// The sync okcode methods (workspaces, useEmbedder, active, embedders) on the JS thread: db reads
/// must go through the owner's sync path (DbHost::call_sync / run_sync).
#[napi]
pub fn okcode_call_sync(okc: &OkcodeHandle, method: String, args: Option<Vec<Value>>) -> napi::Result<Value> {
    let _g = okcode_core::store::SyncGuard::enter();
    guarded(&method, || okc.okc.call(&method, &args.unwrap_or_default()))
}

/// okcodeToolWs(okc, ws, store, root) — THIS workspace as the tool layer sees it (ask = the
/// active-profile ask, tied to these handles, not to whatever is open under the id later).
#[napi]
pub fn okcode_tool_ws(okc: &OkcodeHandle, ws: &WorkspaceHandle, store: &StoreHandle, root: Option<String>) -> ToolWsHandle {
    let o = Opened { ws: ws.ws.clone(), store: Some(store.store.clone()) };
    ToolWsHandle { tw: okc.okc.tool_ws(o, root) }
}

pub struct WsAsk {
    okc: Arc<Okcode>,
    o: Opened,
    query: Value,
    opts: Value,
}

impl Task for WsAsk {
    type Output = Value;
    type JsValue = JsJson;
    fn compute(&mut self) -> napi::Result<Value> {
        guarded("ask", || self.okc.ws_ask(&self.o, &self.query, &self.opts))
    }
    fn resolve(&mut self, _env: Env, out: Value) -> napi::Result<JsJson> {
        Ok(JsJson(out))
    }
}

/// okcodeWsAsk(okc, ws, store, query, opts) — ws.ask of the okcode workspace built over these handles.
#[napi(ts_return_type = "Promise<unknown>")]
pub fn okcode_ws_ask(okc: &OkcodeHandle, ws: &WorkspaceHandle, store: &StoreHandle, query: Value, opts: Option<Value>) -> AsyncTask<WsAsk> {
    let o = Opened { ws: ws.ws.clone(), store: Some(store.store.clone()) };
    AsyncTask::new(WsAsk { okc: okc.okc.clone(), o, query, opts: opts.unwrap_or(Value::Null) })
}

/// okcodeWorkspace(okc, id) — the CURRENT open `{ ws, store }` handles (after reset({ scope: 'all' })
/// reopened the workspace), or null.
#[napi(ts_return_type = "{ ws: WorkspaceHandle, store: StoreHandle | null } | null")]
pub fn okcode_workspace(env: Env, okc: &OkcodeHandle, id: String) -> napi::Result<Option<JsObject>> {
    let Some(out) = okc.okc.workspace(&id) else { return Ok(None) };
    let mut obj = env.create_object()?;
    obj.set("ws", WorkspaceHandle { ws: out.ws })?;
    match out.store {
        Some(store) => obj.set("store", StoreHandle { store })?,
        None => obj.set("store", Null)?,
    }
    Ok(Some(obj))
}
