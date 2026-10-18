//! okcode's handle on an okdb instance (release plan C4: okdb's stable C bridge, no okdb in this addon).
//!
//! Library mode: the HOST's `@kedem/okdb` v3 instance. JS calls `dbAttach(db._native.bridgeTable(),
//! db._native.bridgeHandle(), db.path)` ON THE JS THREAD (src/native/okdb.js `bridgeOf`); okcode's Rust core then
//! talks to that very instance through its bridge (jobs from the threadpool are submitted and completed on the JS
//! loop). okcode never opens, pumps or closes the host's instance.
//!
//! `dbCall(db, scope, env, method, args)` stays for the JS layer's rare direct requests (async, threadpool).
use crate::guarded;
use napi::bindgen_prelude::*;
use napi::{Env, Task};
use napi_derive::napi;
use okcode_core::db::DbHost;
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;

#[napi]
pub struct DbHandle {
    pub(crate) host: Arc<DbHost>,
}

/// dbAttach(bridgeTable: bigint, bridgeHandle: bigint, path: string): the host's instance, attached on this JS thread.
#[napi]
pub fn db_attach(table: BigInt, handle: BigInt, path: String) -> napi::Result<DbHandle> {
    let (t, h) = (table.get_u64().1, handle.get_u64().1);
    // SAFETY: `table` is the address the host's okdb (db._native.bridgeTable()) returned; its addon stays loaded.
    let host = guarded("okcode attach", || unsafe { DbHost::attach(t, h, Path::new(&path)) })?;
    Ok(DbHandle { host })
}

/// dbDetach(db): okcode lets go of the host's instance (one reference).
#[napi]
pub fn db_detach(db: &DbHandle) -> napi::Result<()> {
    guarded("okcode detach", || db.host.close())
}

pub struct DbCall {
    host: Arc<DbHost>,
    scope: String,
    env: Option<String>,
    method: String,
    args: Vec<Value>,
}

impl Task for DbCall {
    type Output = Value;
    type JsValue = napi::JsUnknown;
    fn compute(&mut self) -> napi::Result<Value> {
        let args = std::mem::take(&mut self.args);
        guarded(&self.method, || self.host.call(&self.scope, self.env.as_deref(), &self.method, args))
    }
    fn resolve(&mut self, env: Env, out: Value) -> napi::Result<napi::JsUnknown> {
        crate::js_value(&env, out)
    }
}

#[napi(ts_return_type = "Promise<unknown>")]
pub fn db_call(db: &DbHandle, scope: String, env: Option<String>, method: String, args: Option<Vec<Value>>) -> AsyncTask<DbCall> {
    AsyncTask::new(DbCall { host: db.host.clone(), scope, env, method, args: args.unwrap_or_default() })
}
