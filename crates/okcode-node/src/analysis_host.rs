//! The host's analysers for indexing (lib/analysis/parse.js): one threadsafe function into the JS
//! `extract()` / `analyzerVersionFor()`, installed when the host first registers an extractor or an
//! extension. A worker indexing a path the host claims posts `(id, kind, rel, text)` and blocks for
//! `analysisReply(id, err, value)`. The JS thread never waits on itself: a claimed path reached from
//! it is extracted natively.
use napi::threadsafe_function::{ErrorStrategy, ThreadSafeCallContext, ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi::{Env, JsFunction, Status};
use napi_derive::napi;
use okcode_core::host_analysis::{self, Host};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::ThreadId;

struct Req {
    id: u32,
    kind: &'static str,
    rel: String,
    text: Option<String>,
}

static NEXT_ID: AtomicU32 = AtomicU32::new(1);

fn pending() -> &'static Mutex<HashMap<u32, SyncSender<Result<Value, String>>>> {
    static P: OnceLock<Mutex<HashMap<u32, SyncSender<Result<Value, String>>>>> = OnceLock::new();
    P.get_or_init(Default::default)
}

struct JsHost {
    tsfn: ThreadsafeFunction<Req, ErrorStrategy::Fatal>,
    main: ThreadId,
}

impl JsHost {
    fn call(&self, kind: &'static str, rel: &str, text: Option<&str>) -> Option<Value> {
        if std::thread::current().id() == self.main {
            return None;
        }
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed).max(1);
        let (tx, rx) = sync_channel(1);
        pending().lock().unwrap_or_else(|e| e.into_inner()).insert(id, tx);
        let st = self.tsfn.call(Req { id, kind, rel: rel.to_string(), text: text.map(String::from) }, ThreadsafeFunctionCallMode::Blocking);
        if st != Status::Ok {
            pending().lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
            return None;
        }
        match rx.recv() {
            Ok(Ok(v)) if !v.is_null() => Some(v),
            _ => None,
        }
    }
}

impl Host for JsHost {
    fn extract(&self, rel: &str, text: &str) -> Option<Value> {
        self.call("extract", rel, Some(text))
    }

    fn version_for(&self, rel: &str) -> Option<Option<String>> {
        // { version: string | null }, or null when the host does not claim rel
        self.call("version", rel, None).map(|v| v.get("version").and_then(Value::as_str).map(String::from))
    }
}

/// analysisHost(langs, extensions, host): the host's claims (the languages it has an extractor for, and
/// whether it registered an extension), and on the first call its dispatcher
/// `host(id, kind, rel, text)` that answers with analysisReply.
#[napi]
pub fn analysis_host(env: Env, langs: Vec<String>, extensions: bool, host: Option<JsFunction>) -> napi::Result<()> {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    if let Some(cb) = host {
        if INSTALLED.get().is_none() {
            let mut tsfn: ThreadsafeFunction<Req, ErrorStrategy::Fatal> = cb.create_threadsafe_function(0, |ctx: ThreadSafeCallContext<Req>| {
                let r = ctx.value;
                let text = match r.text {
                    Some(t) => ctx.env.create_string_from_std(t)?.into_unknown(),
                    None => ctx.env.get_null()?.into_unknown(),
                };
                Ok(vec![ctx.env.create_uint32(r.id)?.into_unknown(), ctx.env.create_string(r.kind)?.into_unknown(), ctx.env.create_string_from_std(r.rel)?.into_unknown(), text])
            })?;
            tsfn.unref(&env)?;
            host_analysis::set_host(Arc::new(JsHost { tsfn, main: std::thread::current().id() }));
            let _ = INSTALLED.set(());
        }
    }
    host_analysis::set_claims(langs, extensions);
    Ok(())
}

/// analysisReply(id, err, value): the JS dispatcher's answer to one request.
#[napi]
pub fn analysis_reply(id: u32, err: Option<String>, value: Option<Value>) {
    let tx = pending().lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
    if let Some(tx) = tx {
        let _ = tx.send(match err {
            Some(e) => Err(e),
            None => Ok(value.unwrap_or(json!(null))),
        });
    }
}
