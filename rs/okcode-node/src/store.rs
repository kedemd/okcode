//! store.js over okcode_core::Store. OWNER: slice `perf` (wave 3; wave 2: `store`), with js/store.js.
//!
//! openStore({ db, id, access, profiles, fts, log }) in JS → `openStore(dbHandle, id, accessHandle,
//! options)`; every store method → `storeCall(store, method, args)` → `Store::call` on the threadpool.
//! A profile whose embedder is a JS function (a custom embed fn) needs a threadsafe-function bridge
//! like access_bridge.rs — add it here.
use crate::access_bridge::AccessHandle;
use crate::guarded;
use crate::okdb::DbHandle;
use napi::bindgen_prelude::*;
use napi::threadsafe_function::{ErrorStrategy, ThreadSafeCallContext, ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi::Task;
use napi_derive::napi;
use okcode_core::store::{FtsMode, Store, StoreOptions};
use okcode_core::store::LogFn;
use okcode_core::Access;
use serde_json::Value;
use std::sync::Arc;

#[napi]
pub struct StoreHandle {
    pub(crate) store: Arc<Store>,
}

#[napi]
impl StoreHandle {
    #[napi(getter)]
    pub fn env_name(&self) -> String {
        self.store.env_name.clone()
    }
    /// store.fts — symbol full-text index present
    #[napi(getter)]
    pub fn fts(&self) -> bool {
        self.store.fts()
    }
    /// store.contentFts — content full-text index present
    #[napi(getter)]
    pub fn content_fts(&self) -> bool {
        self.store.content_fts()
    }
    #[napi(getter)]
    pub fn fts_mode(&self) -> String {
        self.store.fts_mode.as_str().to_string()
    }
}

pub struct OpenStore {
    db: Arc<okcode_core::db::DbHost>,
    access: Arc<dyn Access>,
    opts: Option<StoreOptions>,
}

impl Task for OpenStore {
    type Output = Arc<Store>;
    type JsValue = StoreHandle;
    fn compute(&mut self) -> napi::Result<Self::Output> {
        let opts = self.opts.take().unwrap_or_default();
        guarded("openStore", || Store::open(self.db.clone(), self.access.clone(), opts).map(Arc::new))
    }
    fn resolve(&mut self, _env: Env, store: Self::Output) -> napi::Result<StoreHandle> {
        Ok(StoreHandle { store })
    }
}

/// store.js's `log(line)`: a non-blocking threadsafe call into the JS function (never waits for JS,
/// so it is safe from the db owner thread and from a store call the JS thread is blocked on).
pub(crate) fn log_bridge(env: &Env, f: Option<JsFunction>) -> napi::Result<Option<LogFn>> {
    let Some(f) = f else { return Ok(None) };
    let mut tsfn: ThreadsafeFunction<String, ErrorStrategy::Fatal> =
        f.create_threadsafe_function(0, |ctx: ThreadSafeCallContext<String>| Ok(vec![ctx.env.create_string(&ctx.value)?]))?;
    tsfn.unref(env)?; // a logger must not keep the process alive
    Ok(Some(Arc::new(move |line: &str| {
        let _ = tsfn.call(line.to_string(), ThreadsafeFunctionCallMode::NonBlocking);
    })))
}

/// openStore(db, id, access, { profiles, fts }, log)
#[napi(ts_return_type = "Promise<StoreHandle>")]
pub fn open_store(env: Env, db: &DbHandle, id: Value, access: &AccessHandle, options: Option<Value>, log: Option<JsFunction>) -> napi::Result<AsyncTask<OpenStore>> {
    let id = match id {
        Value::String(s) if !s.is_empty() => s,
        _ => return Err(napi::Error::from_reason("a workspace id is required")),
    };
    let o = options.unwrap_or(Value::Null);
    let fts = if o["fts"].as_str() == Some("symbols") { FtsMode::Symbols } else { FtsMode::Full };
    let profiles = o["profiles"].as_array().cloned().unwrap_or_default();
    let log = log_bridge(&env, log)?;
    Ok(AsyncTask::new(OpenStore { db: db.host.clone(), access: access.access.clone(), opts: Some(StoreOptions { id, profiles, fts, log }) }))
}

pub struct StoreCall {
    store: Arc<Store>,
    method: String,
    args: Vec<Value>,
}

impl Task for StoreCall {
    type Output = Value;
    type JsValue = napi::JsUnknown;
    fn compute(&mut self) -> napi::Result<Value> {
        guarded(&self.method, || self.store.call(&self.method, &self.args))
    }
    fn resolve(&mut self, env: Env, out: Value) -> napi::Result<napi::JsUnknown> {
        crate::js_value(&env, out)
    }
}

#[napi(ts_return_type = "Promise<unknown>")]
pub fn store_call(store: &StoreHandle, method: String, args: Option<Vec<Value>>) -> AsyncTask<StoreCall> {
    AsyncTask::new(StoreCall { store: store.store.clone(), method, args: args.unwrap_or_default() })
}

/// The sync store methods (setTextSource is internal; search/searchContent/contentCandidates/hasProfiles/load are sync in JS).
#[napi]
pub fn store_call_sync(store: &StoreHandle, method: String, args: Option<Vec<Value>>) -> napi::Result<Value> {
    let _g = okcode_core::store::SyncGuard::enter();
    guarded(&method, || store.store.call(&method, &args.unwrap_or_default()))
}

/// store.js's pure exports (envNameFor, quoteLines, queryTerms, asQuery).
#[napi]
pub fn store_pure(name: String, args: Option<Vec<Value>>) -> napi::Result<Value> {
    guarded(&name, || okcode_core::store::call_pure(&name, &args.unwrap_or_default()))
}
