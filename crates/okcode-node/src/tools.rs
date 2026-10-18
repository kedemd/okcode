//! lib/tools.js over okcode_core::Tools.
//!
//!   toolsCreate(options, host) → ToolsHandle
//!       `host(id, op, payload)` is lib/tools.js's dispatcher, called on the JS thread through a
//!       threadsafe function; it answers with toolsReply(id, err, value, toolWs):
//!         op 'workspace' { hint }            → toolWs (native ws) | value { key, id, root, has } (a
//!                                              bridged JS ws, registered under `key`) | null
//!         op 'workspaces' {}                 → value [{ id, root?, description? }]
//!         op 'wsCall' { key, method, args }  → value (the JSON result of ws[method](...args))
//!         op 'release' { key }               → (fire and forget, id 0)
//!   toolsRender(tools, name, args) → Promise<string>   (threadpool; never rejects by design)
//!   toolsSchemas(known) → schema list                   (sync: `known` is workspaces() from JS)
//!   toolsPure(name, args)                              (isTool, subjectOf, hitWithin, …)
//!
//! Like access_bridge.rs: a worker thread blocks on the reply; a call from the JS thread itself
//! must be refused (it would deadlock). A ws key is released when its render finishes (the
//! bridged workspace is dropped).
use crate::{guarded, ToolWsHandle};

use napi::threadsafe_function::{ErrorStrategy, ThreadSafeCallContext, ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi::{Env, JsFunction, Status};
use napi_derive::napi;
use okcode_core::host::{ToolHost, ToolWorkspace};
use okcode_core::tools::{ToolOptions, Tools};
use okcode_core::Error;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::ThreadId;

#[napi]
pub struct ToolsHandle {
    pub(crate) tools: Arc<Tools>,
}

static NEXT_ID: AtomicU32 = AtomicU32::new(1);

struct Reply {
    err: Option<String>,
    value: Value,
    tw: Option<Arc<dyn ToolWorkspace>>,
}

fn pending() -> &'static Mutex<HashMap<u32, SyncSender<Reply>>> {
    static P: OnceLock<Mutex<HashMap<u32, SyncSender<Reply>>>> = OnceLock::new();
    P.get_or_init(Default::default)
}

struct Req {
    id: u32,
    op: &'static str,
    payload: Value,
}

/// The JS host: one threadsafe function into lib/tools.js's dispatcher.
struct JsHost {
    tsfn: ThreadsafeFunction<Req, ErrorStrategy::Fatal>,
    main: ThreadId,
}

impl JsHost {
    fn call(&self, op: &'static str, payload: Value) -> std::result::Result<Reply, Error> {
        if std::thread::current().id() == self.main {
            return Err(Error::msg(format!("tools host {op}: cannot be called from the JS thread (sync call path)")));
        }
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed).max(1);
        let (tx, rx) = sync_channel(1);
        pending().lock().unwrap_or_else(|e| e.into_inner()).insert(id, tx);
        let st = self.tsfn.call(Req { id, op, payload }, ThreadsafeFunctionCallMode::Blocking);
        if st != Status::Ok {
            pending().lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
            return Err(Error::msg(format!("tools host {op}: the JS thread is gone ({st:?})")));
        }
        let r = match rx.recv() {
            Ok(r) => r,
            Err(_) => {
                pending().lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
                return Err(Error::msg(format!("tools host {op}: no reply")));
            }
        };
        match r.err {
            Some(e) => Err(parse_err(&e)),
            None => Ok(r),
        }
    }

    fn post(&self, op: &'static str, payload: Value) {
        let _ = self.tsfn.call(Req { id: 0, op, payload }, ThreadsafeFunctionCallMode::NonBlocking);
    }
}

/// The JS side sends errors as JSON `{ code, message }`.
fn parse_err(s: &str) -> Error {
    match serde_json::from_str::<Value>(s) {
        Ok(v) if v.is_object() => {
            let message = v["message"].as_str().unwrap_or(s).to_string();
            match v["code"].as_str() {
                Some(c) => Error::code(c, message),
                None => Error::msg(message),
            }
        }
        _ => Error::msg(s),
    }
}

struct HostRef(Arc<JsHost>);

impl ToolHost for HostRef {
    fn workspace(&self, hint: Option<&str>) -> std::result::Result<Option<Arc<dyn ToolWorkspace>>, Error> {
        let r = self.0.call("workspace", json!({ "hint": hint }))?;
        if let Some(tw) = r.tw {
            return Ok(Some(tw));
        }
        let v = r.value;
        if v.is_null() {
            return Ok(None);
        }
        let has: HashSet<String> = v["has"].as_array().map(|a| a.iter().filter_map(|m| m.as_str().map(String::from)).collect()).unwrap_or_default();
        Ok(Some(Arc::new(BridgedWs {
            host: self.0.clone(),
            key: v["key"].clone(),
            id: v.get("id").map_or_else(|| "undefined".to_string(), okcode_core::okcode::js_string),
            root: v["root"].as_str().map(String::from),
            has,
        })))
    }

    fn workspaces(&self) -> Vec<Value> {
        match self.0.call("workspaces", json!({})) {
            Ok(r) => match r.value {
                Value::Array(a) => a,
                _ => vec![],
            },
            Err(_) => vec![],
        }
    }
}

/// A JS workspace object that is not native (a fake, a ws with a wrapped method): every verb is a
/// round trip to the JS thread, keyed. Released when the render that resolved it ends.
struct BridgedWs {
    host: Arc<JsHost>,
    key: Value,
    id: String,
    root: Option<String>,
    has: HashSet<String>,
}

impl ToolWorkspace for BridgedWs {
    fn id(&self) -> String {
        self.id.clone()
    }
    fn access_root(&self) -> Option<String> {
        self.root.clone()
    }
    fn has(&self, method: &str) -> bool {
        self.has.contains(method)
    }
    fn call(&self, method: &str, args: &[Value]) -> std::result::Result<Value, Error> {
        Ok(self.host.call("wsCall", json!({"key": self.key, "method": method, "args": args}))?.value)
    }
}

impl Drop for BridgedWs {
    fn drop(&mut self) {
        self.host.post("release", json!({ "key": self.key }));
    }
}

/// toolsCreate({ grepMaxChars }, host)
#[napi]
pub fn tools_create(env: Env, options: Option<Value>, host: JsFunction) -> napi::Result<ToolsHandle> {
    let mut tsfn: ThreadsafeFunction<Req, ErrorStrategy::Fatal> = host.create_threadsafe_function(0, |ctx: ThreadSafeCallContext<Req>| {
        let r = ctx.value;
        let id = ctx.env.create_uint32(r.id)?.into_unknown();
        let op = ctx.env.create_string(r.op)?.into_unknown();
        let payload = crate::js_value(&ctx.env, r.payload)?;
        Ok(vec![id, op, payload])
    })?;
    tsfn.unref(&env)?;
    let mut opts = ToolOptions::default();
    if let Some(n) = options.as_ref().and_then(|o| o["grepMaxChars"].as_f64()) {
        if n.is_finite() && n >= 0.0 {
            opts.grep_max_chars = n as usize;
        }
    }
    let h = Arc::new(JsHost { tsfn, main: std::thread::current().id() });
    Ok(ToolsHandle { tools: Arc::new(Tools::new(Arc::new(HostRef(h)), opts)) })
}

/// toolsReply(id, err, value, toolWs): the JS side's answer to one host request.
#[napi]
pub fn tools_reply(id: u32, err: Option<String>, value: Option<Value>, tool_ws: Option<&ToolWsHandle>) {
    let tx = pending().lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
    if let Some(tx) = tx {
        let _ = tx.send(Reply { err, value: value.unwrap_or(Value::Null), tw: tool_ws.map(|t| t.tw.clone()) });
    }
}

type Job = Box<dyn FnOnce() + Send>;

/// A dedicated, bounded set of std threads for renders. A render blocks on JS host round trips,
/// and those need libuv pool threads, so renders must never occupy the pool themselves.
fn render_pool() -> &'static Mutex<std::sync::mpsc::Sender<Job>> {
    static P: OnceLock<Mutex<std::sync::mpsc::Sender<Job>>> = OnceLock::new();
    P.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        let rx = Arc::new(Mutex::new(rx));
        for i in 0..8 {
            let rx = rx.clone();
            let _ = std::thread::Builder::new().name(format!("okcode-tools-{i}")).spawn(move || loop {
                let job = rx.lock().unwrap_or_else(|e| e.into_inner()).recv();
                match job {
                    Ok(j) => {
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(j));
                    }
                    Err(_) => break,
                }
            });
        }
        Mutex::new(tx)
    })
}

/// toolsRender(tools, name, args, done): runs on the render pool; `done(text)` is called on the JS
/// thread with the result (lib/tools.js wraps it in a promise). Never fails.
#[napi]
pub fn tools_render(tools: &ToolsHandle, name: String, args: Option<Value>, done: JsFunction) -> napi::Result<()> {
    let tsfn: ThreadsafeFunction<String, ErrorStrategy::Fatal> =
        done.create_threadsafe_function(0, |ctx: ThreadSafeCallContext<String>| Ok(vec![ctx.env.create_string(&ctx.value)?]))?;
    let tools = tools.tools.clone();
    let args = args.unwrap_or(Value::Null);
    let job: Job = Box::new(move || {
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tools.render(&name, &args)))
            .unwrap_or_else(|_| format!("({name}) failed: internal error"));
        tsfn.call(out, ThreadsafeFunctionCallMode::NonBlocking);
    });
    let sent = render_pool().lock().unwrap_or_else(|e| e.into_inner()).send(job);
    if sent.is_err() {
        return Err(napi::Error::from_reason("tools render pool is gone"));
    }
    Ok(())
}

#[napi]
pub fn tools_schemas(known: Option<Vec<Value>>) -> napi::Result<Value> {
    guarded("schemas", || Ok(okcode_core::tools::schemas_for(&known.unwrap_or_default())))
}

#[napi]
pub fn tools_pure(name: String, args: Option<Vec<Value>>) -> napi::Result<Value> {
    guarded(&name, || okcode_core::tools::call_pure(&name, &args.unwrap_or_default()))
}
