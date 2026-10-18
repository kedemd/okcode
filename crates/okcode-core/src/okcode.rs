//! The host-facing surface (lib/okcode.js#open).
//!
//! JS: `okcode.open({ path | db, role, license, embedders, active, workspace, log })` returns an
//! object with addWorkspace / workspace / workspaces / removeWorkspace / sync / status / reset /
//! addEmbedder / useEmbedder / removeEmbedder / removeOrphaned / compare / close, the `active`
//! getter and embedders(). Here: one `Okcode` (Send + Sync, interior locking) whose methods return
//! the JS result shapes as `serde_json::Value`.
//!
//! What lib/okcode.js keeps: functions. A profile's custom `embed` or `apiKey` function becomes
//! an okdb embedder FACTORY registered on the db by lib/embedders.js#preRegister (before
//! `db.open()`, and again before addEmbedder), so the native side only ever sees the derived
//! `okcode-<slug>` type. `HostProfile.fn_keys` says which keys of the config were functions (the
//! Rust `okcode_embed::embedders::from_host*` take that as flags). `new OKDB(path, role flags)`
//! happens in JS so the factories can be registered before open; `Okcode::open` gets the
//! resulting `DbHost`.
//!
//! The durable state (okdb env `okcode`: tables workspaces / embedders / settings) goes through
//! `DbHost::call("env", Some("okcode"), …)`.
//!
//! LOCKING (lib.rs): never hold `state` across a workspace, store or db call. A JS facade or a JS
//! embedder may be served by the JS thread that is itself waiting for this call.
use crate::db::DbHost;
use crate::error::{Error, Result};
use crate::host::{NativeWs, ToolWorkspace};
use crate::store::{env_name_for, FtsMode, LogFn, Store, StoreOptions, FILES, SYMBOLS};
use crate::workspace::scan::{RefreshOpts, SyncOpts};
use crate::workspace::{Caps, Workspace};
use indexmap::IndexMap;
use okcode_analysis::access::Access;
use okcode_analysis::embed::identity::{self, Parts};
use okcode_analysis::embed::embedders as emb;
use serde_json::{json, Map, Value};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

pub const META_ENV: &str = "okcode";
pub const T_WS: &str = "workspaces";
pub const T_EMB: &str = "embedders";
pub const T_SET: &str = "settings";
/// store.rs names both of its full-text indexes this
pub const FTS_NAME: &str = "text";

/// One host profile as configured (`embedders: { name: cfg }`), functions dropped.
#[derive(Clone, Debug, Default)]
pub struct HostProfile {
    pub name: String,
    /// the config as JSON (functions removed)
    pub cfg: Value,
    /// keys of `cfg` whose value was a function (`embed`, `apiKey`, …)
    pub fn_keys: Vec<String>,
}

/// open()'s options, after the JS layer took what is code.
#[derive(Clone, Default)]
pub struct OpenOptions {
    /// open({ path }) — okcode owns the db: open it here, close it on close()
    pub owns_db: bool,
    /// license (string | { blob, activation }) — null/absent = none; ignored (warned) when !owns_db
    pub license: Value,
    pub embedders: Vec<HostProfile>,
    pub active: Option<String>,
    /// a non-null, non-string `active` (JS-stringified): names no profile, so open rejects it
    pub active_other: Option<String>,
    /// the `workspace` defaults merged under every addWorkspace's options
    pub workspace: Value,
    /// L.warn (normalizeLog's warn; info/debug are unused by okcode.js)
    pub log: Option<LogFn>,
}

/// addWorkspace(id, { access, options, index, fts })
#[derive(Clone, Debug, Default)]
pub struct AddOptions {
    pub options: Value,
    /// 'full' (default) | 'none'; the raw JS value (Null = not a valid choice, as JS)
    pub index: Value,
    /// 'full' | 'symbols' | Null (→ open()'s workspace.fts → 'full'); raw
    pub fts: Value,
    /// access.kind / access.root, as the registry record keeps them
    pub access_kind: Option<String>,
    pub access_root: Option<String>,
}

/// What addWorkspace opened (the binding wraps both for JS).
#[derive(Clone)]
pub struct Opened {
    pub ws: Arc<Workspace>,
    /// None for index: 'none'
    pub store: Option<Arc<Store>>,
}

/// JS `String(v)` for a JSON value (identity::js_string prints objects/arrays as JSON and
/// whole floats as "1.0"; this is what an error message naming the host's value needs).
pub fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => match n.as_f64() {
            Some(f) if f.fract() == 0.0 && f.abs() < 1e21 => format!("{}", f as i128),
            _ => n.to_string(),
        },
        Value::Array(a) => a.iter().map(|x| if x.is_null() { String::new() } else { js_string(x) }).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

fn err_not_open(id: &str, what: &str) -> Error {
    coded(
        format!("workspace \"{id}\" is not open in this process \u{2014} {what} needs its access facade (addWorkspace first)"),
        "OKCODE_WORKSPACE_NOT_OPEN",
    )
}

fn err_unknown(id: &str) -> Error {
    coded(format!("no workspace \"{id}\""), "OKCODE_UNKNOWN_WORKSPACE")
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(true, |f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        _ => true,
    }
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn coded(message: impl Into<String>, code: &str) -> Error {
    Error::code(code, message)
}

fn is_missing(v: &Value) -> bool {
    // a missing key: nil over the bridge (the old local wire marker kept for old callers)
    v.is_null() || v.get("$okdb_missing").is_some()
}

/// One configured profile: `{ name, record, profile (store profile | None), needsConfig, reason }`.
#[derive(Clone, Debug)]
pub struct Prof {
    pub name: String,
    pub record: Value,
    pub profile: Option<Value>,
    pub needs_config: bool,
    pub reason: Option<String>,
}

#[derive(Clone)]
struct OpenWs {
    opened: Opened,
    access: Arc<dyn Access>,
    caps: Caps,
    options: Value,
    /// 'full' | 'none'
    index: String,
}

/// The API's mutable state (profiles, active name, opened workspaces). Never held across a
/// workspace, store or db call.
#[derive(Default)]
pub struct ApiState {
    profiles: IndexMap<String, Prof>,
    active: Option<String>,
    opened: IndexMap<String, OpenWs>,
}

pub struct Okcode {
    pub db: Arc<DbHost>,
    pub owns_db: bool,
    pub state: Mutex<ApiState>,
    defaults: Value,
    log: Option<LogFn>,
}

fn sync_result(id: &str, r: &Value, ms: i64) -> Value {
    let n = |k: &str| r.get(k).filter(|v| truthy(Some(v))).cloned().unwrap_or(json!(0));
    json!({ "id": id, "scanned": n("scanned"), "changed": n("reparsed"), "removed": n("removed"), "ms": ms })
}

fn merge_obj(base: &Value, over: &Value) -> Value {
    let mut m = base.as_object().cloned().unwrap_or_default();
    if let Some(o) = over.as_object() {
        for (k, v) in o {
            m.insert(k.clone(), v.clone());
        }
    }
    Value::Object(m)
}

/// The tool layer's view of an okcode workspace: native, except `ask` follows the active profile.
struct OkToolWs {
    okc: Arc<Okcode>,
    native: NativeWs,
    /// the workspace (and store) this wrapper was built for, NOT looked up by id: a stale one
    /// after remove / re-add / reset-all asks its own (closed) workspace, as the JS ws does
    opened: Opened,
}

impl ToolWorkspace for OkToolWs {
    fn id(&self) -> String {
        self.native.id()
    }
    fn access_root(&self) -> Option<String> {
        self.native.access_root()
    }
    fn has(&self, method: &str) -> bool {
        self.native.has(method)
    }
    fn call(&self, method: &str, args: &[Value]) -> Result<Value> {
        if method == "ask" && self.opened.store.is_some() {
            let a = |i: usize| args.get(i).cloned().unwrap_or(Value::Null);
            return self.okc.ws_ask(&self.opened, &a(0), &a(1));
        }
        self.native.call(method, args)
    }
}

impl Okcode {
    // ── small helpers ───────────────────────────────────────────────────

    fn lock(&self) -> std::sync::MutexGuard<'_, ApiState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn warn(&self, msg: &str) {
        if let Some(l) = &self.log {
            l(msg);
        }
    }

    fn mc(&self, sync: bool, method: &str, args: Vec<Value>) -> Result<Value> {
        if sync {
            self.db.call_sync("env", Some(META_ENV), method, args)
        } else {
            self.db.call("env", Some(META_ENV), method, args)
        }
    }

    fn meta_get(&self, sync: bool, ty: &str, key: &str) -> Result<Option<Value>> {
        let v = self.mc(sync, "get", vec![json!(ty), json!(key)])?;
        Ok(if is_missing(&v) || v.is_null() { None } else { Some(v) })
    }

    fn meta_put(&self, sync: bool, ty: &str, key: &str, value: Value) -> Result<()> {
        self.mc(sync, "put", vec![json!(ty), json!(key), value]).map(|_| ())
    }

    fn meta_remove(&self, ty: &str, key: &str) -> Result<()> {
        self.mc(false, "remove", vec![json!(ty), json!(key)]).map(|_| ())
    }

    fn meta_range(&self, sync: bool, ty: &str) -> Result<Vec<(String, Value)>> {
        let v = self.mc(sync, "getRange", vec![json!(ty), json!({})])?;
        Ok(v.as_array()
            .map(|a| a.iter().map(|e| (e["key"].as_str().unwrap_or_default().to_string(), e["value"].clone())).collect())
            .unwrap_or_default())
    }

    fn reg_get(&self, sync: bool, id: &str) -> Result<Option<Value>> {
        self.meta_get(sync, T_WS, id)
    }

    fn reg_list(&self, sync: bool) -> Result<Vec<Value>> {
        Ok(self.meta_range(sync, T_WS)?.into_iter().map(|(_, v)| v).filter(|v| !v.is_null()).collect())
    }

    fn reg_patch(&self, id: &str, patch: Value) -> Result<()> {
        if let Some(cur) = self.reg_get(false, id)? {
            self.meta_put(false, T_WS, id, merge_obj(&cur, &patch))?;
        }
        Ok(())
    }

    fn persist_profile(&self, rec: &Value) -> Result<()> {
        let name = rec["name"].as_str().unwrap_or_default().to_string();
        self.meta_put(false, T_EMB, &name, rec.clone())
    }

    fn profile_list(&self) -> Vec<Prof> {
        self.lock().profiles.values().cloned().collect()
    }

    fn usable_profiles(&self) -> Vec<Prof> {
        self.lock().profiles.values().filter(|p| !p.needs_config && p.profile.is_some()).cloned().collect()
    }

    fn open_snapshot(&self, id: &str) -> Option<OpenWs> {
        self.lock().opened.get(id).cloned()
    }

    fn assert_known(&self, id: &str) -> Result<()> {
        if self.reg_get(false, id)?.is_none() && self.lock().opened.get(id).is_none() {
            return Err(err_unknown(id));
        }
        Ok(())
    }

    fn require_open(&self, id: &str, what: &str) -> Result<OpenWs> {
        self.open_snapshot(id).ok_or_else(|| err_not_open(id, what))
    }

    fn refuse_unindexed(&self, id: &str, what: &str) -> Result<()> {
        if self.open_snapshot(id).is_some_and(|o| o.index == "none") {
            return Err(coded(format!("workspace \"{id}\" is not indexed \u{2014} {what} needs an index"), "OKCODE_NOT_INDEXED"));
        }
        Ok(())
    }

    /// The profile name an ask() resolves to: an explicit one must exist and be usable; none means the active profile.
    /// `name` is the raw JS value: null/missing = active; a non-string never matches a profile
    /// (profiles.get(5) in JS), so it fails as String() of it.
    fn resolve_profile(&self, name: &Value) -> Result<String> {
        let st = self.lock();
        let n = match name {
            Value::String(n) => n.to_string(),
            Value::Null => st.active.clone().ok_or_else(|| coded("no embedder profile is configured \u{2014} ask() is unavailable", "OKCODE_NO_EMBEDDINGS"))?,
            other => return Err(coded(format!("no embedding profile named \"{}\"", js_string(other)), "OKCODE_NO_EMBEDDINGS")),
        };
        let p = st.profiles.get(&n).ok_or_else(|| coded(format!("no embedding profile named \"{n}\""), "OKCODE_NO_EMBEDDINGS"))?;
        if p.needs_config {
            return Err(coded(
                format!("embedding profile \"{n}\" needs config: {}", p.reason.clone().unwrap_or_default()),
                "OKCODE_NO_EMBEDDINGS",
            ));
        }
        Ok(n)
    }

    /// The workspace env's name WITHOUT a store (None when it does not exist / unindexed).
    fn env_of(&self, id: &str) -> Result<Option<String>> {
        if let Some(o) = self.open_snapshot(id) {
            return Ok(o.opened.store.as_ref().map(|s| s.env_name.clone()));
        }
        let name = env_name_for(id)?;
        match self.db.call("db", None, "openEnv", vec![json!(name)]) {
            Ok(_) => Ok(Some(name)),
            Err(e) if e.code.as_deref() == Some("ENV_NOT_FOUND") => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Drop a workspace's env (pipelines, engines and vectors go with it).
    fn drop_env(&self, id: &str, store: Option<Arc<Store>>) -> Result<()> {
        if let Some(s) = store {
            return s.drop_env();
        }
        if let Some(name) = self.env_of(id)? {
            match self.db.call("db", None, "removeEnvironment", vec![json!(name)]) {
                Ok(_) => {}
                Err(e) if e.code.as_deref() == Some("ENV_NOT_FOUND") => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// What a store learned about a profile (its dims) goes into the record.
    fn learn_dims(&self, store: &Store) -> Result<()> {
        let sps = store.profiles()?;
        let mut persist = Vec::new();
        {
            let mut st = self.lock();
            for sp in sps {
                let (name, dims) = (sp["name"].as_str().unwrap_or_default().to_string(), sp.get("dims").cloned().unwrap_or(Value::Null));
                if !truthy(Some(&dims)) {
                    continue;
                }
                if let Some(p) = st.profiles.get_mut(&name) {
                    if truthy(p.record.get("dims")) {
                        continue;
                    }
                    p.record["dims"] = dims.clone();
                    if let Some(pr) = &mut p.profile {
                        pr["dims"] = dims;
                    }
                    persist.push(p.record.clone());
                }
            }
        }
        for r in persist {
            self.persist_profile(&r)?;
        }
        Ok(())
    }

    /// lib/embedders.js#fromHost over a config whose functions were dropped (`fn_keys` names them).
    fn translate(&self, p: &HostProfile) -> Result<emb::Translated> {
        let name = p.name.as_str();
        if name.is_empty() {
            return Err(Error::msg("an embedder profile needs a name"));
        }
        if !p.cfg.is_object() && !p.cfg.is_array() {
            return Err(Error::msg(format!("embedder \"{name}\": config must be an object")));
        }
        let has = |k: &str| p.fn_keys.iter().any(|x| x == k);
        if has("embed") {
            return emb::from_host_custom(name, &p.cfg).map_err(|m| {
                if m.contains("needs an `id`") {
                    coded(m, "OKCODE_NEEDS_ID")
                } else {
                    Error::msg(m)
                }
            });
        }
        let ty = p.cfg.get("type").and_then(Value::as_str).filter(|s| !s.is_empty());
        let Some(ty) = ty else {
            return Err(Error::msg(format!("embedder \"{name}\": needs a type ('ollama', 'openai', \u{2026}) or an embed function")));
        };
        for k in &p.fn_keys {
            if k != "apiKey" && k != "api_key" && k != "type" && k != "dims" && k != "pipeline" {
                return Err(Error::msg(format!("embedder \"{name}\": \"{k}\" cannot be a function")));
            }
        }
        let key_fn = has("apiKey") || has("api_key");
        if key_fn {
            let known = self.db.call("embeddings", None, "hasEmbedderFactory", vec![json!(ty)])?;
            if known != json!(true) {
                return Err(coded(
                    format!("okdb has no embedder factory \"{ty}\" (register it with db.embeddings.registerEmbedderFactory)"),
                    "OKCODE_UNKNOWN_EMBEDDER_TYPE",
                ));
            }
        }
        let t = emb::from_host(name, &p.cfg, key_fn).map_err(Error::msg)?;
        if key_fn {
            // preRegister swallows its own failures: the derived factory must really exist
            let dty = t.record.get("type").and_then(Value::as_str).unwrap_or(ty).to_string();
            let known = self.db.call("embeddings", None, "hasEmbedderFactory", vec![json!(dty)])?;
            if known != json!(true) {
                return Err(coded(
                    format!("okdb has no embedder factory \"{dty}\" (register it with db.embeddings.registerEmbedderFactory)"),
                    "OKCODE_UNKNOWN_EMBEDDER_TYPE",
                ));
            }
        }
        Ok(t)
    }

    // ── open ────────────────────────────────────────────────────────────

    /// open() from `db.open()` on: license, the meta env and its tables, profiles (host, then
    /// persisted), the active profile. On an error after the db was opened here, close it.
    pub fn open(db: Arc<DbHost>, opts: OpenOptions) -> Result<Arc<Okcode>> {
        let owns = opts.owns_db;
        let okc = Okcode { db: db.clone(), owns_db: owns, state: Mutex::new(ApiState::default()), defaults: opts.workspace.clone(), log: opts.log.clone() };
        if owns {
            db.open()?;
            if !opts.license.is_null() {
                // Before the management env, workspaces or pipelines exist, so none is created under the free tier's limits.
                match db.call("licenses", None, "add", vec![opts.license.clone()]) {
                    Ok(installed) => {
                        if truthy(installed.get("needsActivation")) {
                            okc.warn(&format!(
                                "[okcode] license {} needs activation on this node: send PIN {} to the vendor, then pass {{ license: {{ blob, activation }} }} (running on the free tier until then)",
                                identity::js_string(&installed["id"]),
                                identity::js_string(&installed["pin"]),
                            ));
                        }
                    }
                    Err(e) => {
                        let _ = db.close();
                        return Err(e);
                    }
                }
            }
        } else if !opts.license.is_null() {
            okc.warn("[okcode] `license` ignored: a host-supplied db owns its licensing (db.licenses.add)");
        }
        match okc.open_inner(&opts) {
            Ok(()) => Ok(Arc::new(okc)),
            Err(e) => {
                if owns {
                    let _ = db.close();
                }
                Err(e)
            }
        }
    }

    fn open_inner(&self, opts: &OpenOptions) -> Result<()> {
        // meta env: open or create
        match self.db.call("db", None, "openEnv", vec![json!(META_ENV)]) {
            Ok(_) => {}
            Err(e) if e.code.as_deref() == Some("ENV_NOT_FOUND") => {
                self.db.call("db", None, "createEnvironment", vec![json!(META_ENV)])?;
            }
            Err(e) => return Err(e),
        }
        for t in [T_WS, T_EMB, T_SET] {
            self.mc(false, "ensureType", vec![json!(t)])?;
        }
        // host profiles
        for hp in &opts.embedders {
            let t = self.translate(hp)?;
            self.lock().profiles.insert(hp.name.clone(), Prof { name: hp.name.clone(), record: t.record.clone(), profile: Some(t.profile), needs_config: false, reason: None });
            self.persist_profile(&t.record)?;
        }
        // persisted ones
        for (key, value) in self.meta_range(false, T_EMB)? {
            if value.is_null() || self.lock().profiles.contains_key(&key) {
                continue;
            }
            let Some(t) = emb::from_record(&value) else { continue };
            let needs = truthy(t.get("needsConfig"));
            let reason = t.get("reason").and_then(Value::as_str).map(String::from);
            self.lock().profiles.insert(
                key.clone(),
                Prof {
                    name: key.clone(),
                    record: t["record"].clone(),
                    profile: t.get("profile").filter(|p| !p.is_null()).cloned(),
                    needs_config: needs,
                    reason: reason.clone(),
                },
            );
            if needs {
                self.warn(&format!("[okcode] embedder \"{key}\" needs config: {}", reason.unwrap_or_default()));
            }
        }
        // the active profile: explicit > persisted > first usable
        let first_usable = |st: &ApiState| st.profiles.values().find(|p| !p.needs_config && p.profile.is_some()).map(|p| p.name.clone());
        if let Some(a) = &opts.active_other {
            return Err(coded(format!("no embedder profile named \"{a}\""), "OKCODE_UNKNOWN_EMBEDDER"));
        }
        if let Some(a) = &opts.active {
            if !self.lock().profiles.contains_key(a) {
                return Err(coded(format!("no embedder profile named \"{a}\""), "OKCODE_UNKNOWN_EMBEDDER"));
            }
            self.lock().active = Some(a.clone());
            self.meta_put(false, T_SET, "active", json!({ "name": a }))?;
        } else {
            let stored = self.meta_get(false, T_SET, "active")?;
            let stored_name = stored.as_ref().and_then(|s| s.get("name")).and_then(Value::as_str).map(String::from);
            let mut st = self.lock();
            let ok = stored_name.as_ref().is_some_and(|n| st.profiles.get(n).is_some_and(|p| !p.needs_config));
            st.active = if ok { stored_name } else { first_usable(&st) };
        }
        Ok(())
    }

    fn open_one(&self, id: &str, access: Arc<dyn Access>, caps: Caps, options: &Value, fts: FtsMode) -> Result<Opened> {
        let profiles: Vec<Value> = self.usable_profiles().into_iter().filter_map(|p| p.profile).collect();
        let store = Arc::new(Store::open(self.db.clone(), access.clone(), StoreOptions { id: id.to_string(), profiles, fts, log: self.log.clone() })?);
        let ws = Workspace::open(id, access.clone(), caps, Some(store.clone()), merge_obj(&self.defaults, options))?;
        let opened = Opened { ws, store: Some(store) };
        self.lock().opened.insert(id.to_string(), OpenWs { opened: opened.clone(), access, caps, options: options.clone(), index: "full".into() });
        Ok(opened)
    }

    /// addWorkspace(id, { access, options, index, fts }) — registers (index 'full'), opens store +
    /// workspace, runs the open walk (refresh), patches lastSync/lastScan, learns dims.
    pub fn add_workspace(&self, id: &str, access: Arc<dyn Access>, caps: Caps, o: AddOptions) -> Result<Opened> {
        env_name_for(id)?; // validates the id
        let index = match o.index.as_str() {
            Some(i @ ("full" | "none")) => i.to_string(),
            _ => return Err(Error::msg(format!("addWorkspace(\"{id}\"): index must be 'full' or 'none' (got {})", o.index))),
        };
        let fts_v = match &o.fts {
            f if !f.is_null() => f.clone(),
            _ => self.defaults.get("fts").filter(|v| !v.is_null()).cloned().unwrap_or(json!("full")),
        };
        let fts = match fts_v.as_str() {
            Some("full") => FtsMode::Full,
            Some("symbols") => FtsMode::Symbols,
            _ => return Err(Error::msg(format!("addWorkspace(\"{id}\"): fts must be 'full' or 'symbols' (got {fts_v})"))),
        };
        let existing = self.reg_get(false, id)?;
        if index == "none" && existing.is_some() {
            return Err(coded(format!("workspace \"{id}\" is an indexed workspace \u{2014} pick another id for an unindexed one"), "OKCODE_WORKSPACE_EXISTS"));
        }
        let prior = self.lock().opened.shift_remove(id);
        if let Some(p) = prior {
            let _ = p.opened.ws.close();
        }
        let options = if o.options.is_null() { json!({}) } else { o.options.clone() };
        if index == "none" {
            let mut merged = merge_obj(&self.defaults, &options);
            merged["index"] = json!("none");
            let ws = Workspace::open(id, access.clone(), caps, None, merged)?;
            let opened = Opened { ws, store: None };
            self.lock().opened.insert(id.to_string(), OpenWs { opened: opened.clone(), access, caps, options, index: "none".into() });
            return Ok(opened);
        }
        let now = now_ms();
        let mut rec = existing.as_ref().and_then(Value::as_object).cloned().unwrap_or_default();
        let added = existing.as_ref().and_then(|e| e.get("added")).filter(|v| truthy(Some(v))).cloned().unwrap_or(json!(now));
        let last_sync = existing.as_ref().and_then(|e| e.get("lastSync")).filter(|v| truthy(Some(v))).cloned().unwrap_or(Value::Null);
        rec.insert("id".into(), json!(id));
        rec.insert("env".into(), json!(env_name_for(id)?));
        rec.insert("added".into(), added);
        rec.insert("lastSync".into(), last_sync);
        rec.insert("access".into(), json!({ "kind": o.access_kind.filter(|k| !k.is_empty()), "root": o.access_root }));
        self.meta_put(false, T_WS, id, Value::Object(rec))?;
        let opened = self.open_one(id, access, caps, &options, fts)?;
        let t0 = now_ms();
        let r = opened.ws.refresh(RefreshOpts::default())?;
        self.reg_patch(id, json!({ "lastSync": now_ms(), "lastScan": sync_result(id, &r, now_ms() - t0) }))?;
        if let Some(s) = &opened.store {
            self.learn_dims(s)?;
        }
        Ok(opened)
    }

    /// workspace(id) — the open one, or None
    pub fn workspace(&self, id: &str) -> Option<Opened> {
        self.open_snapshot(id).map(|o| o.opened)
    }

    fn workspaces_inner(&self, sync: bool) -> Result<Value> {
        let reg = self.reg_list(sync)?;
        let opened: Vec<(String, String)> = self.lock().opened.iter().map(|(k, o)| (k.clone(), o.index.clone())).collect();
        let mut out: Vec<Value> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for r in reg {
            let id = r["id"].as_str().unwrap_or_default().to_string();
            seen.insert(id.clone());
            let mut m = r.as_object().cloned().unwrap_or_default();
            m.insert("open".into(), json!(opened.iter().any(|(k, _)| *k == id)));
            out.push(Value::Object(m));
        }
        for (id, index) in &opened {
            if seen.contains(id) {
                continue;
            }
            out.push(if index == "none" {
                json!({ "id": id, "env": Value::Null, "index": "none", "open": true })
            } else {
                json!({ "id": id, "env": env_name_for(id)?, "open": true })
            });
        }
        // JS compares strings by UTF-16 code units
        out.sort_by(|a, b| {
            let (x, y) = (a["id"].as_str().unwrap_or_default(), b["id"].as_str().unwrap_or_default());
            x.encode_utf16().cmp(y.encode_utf16())
        });
        Ok(Value::Array(out))
    }

    /// workspaces() — sync in JS: registry rows + open-only ones, sorted by id
    pub fn workspaces(&self) -> Result<Value> {
        self.workspaces_inner(true)
    }

    fn workspace_ids(&self) -> Result<Vec<String>> {
        Ok(self.workspaces_inner(false)?.as_array().map(|a| a.iter().map(|w| w["id"].as_str().unwrap_or_default().to_string()).collect()).unwrap_or_default())
    }

    pub fn remove_workspace(&self, id: &str) -> Result<Value> {
        let o = self.lock().opened.shift_remove(id);
        let known = o.is_some() || self.reg_get(false, id)?.is_some();
        if let Some(o) = &o {
            let _ = o.opened.ws.close();
            if o.index == "none" {
                return Ok(json!({ "id": id, "removed": true })); // nothing was ever stored
            }
        }
        self.drop_env(id, o.and_then(|o| o.opened.store))?;
        if self.reg_get(false, id)?.is_some() {
            self.meta_remove(T_WS, id)?;
        }
        Ok(json!({ "id": id, "removed": known }))
    }

    // ── management ──────────────────────────────────────────────────────

    /// sync(id?, { force }) — `sync({ force })` (an object first arg) is handled by the caller/dispatch
    pub fn sync(&self, id: Option<&str>, force: bool) -> Result<Value> {
        let ids: Vec<String> = match id {
            Some(i) => vec![i.to_string()],
            None => self.lock().opened.keys().cloned().collect(),
        };
        let mut out = Vec::new();
        for wid in ids {
            let o = self.require_open(&wid, "sync")?;
            let t0 = now_ms();
            let r = o.opened.ws.sync(SyncOpts { force })?;
            let res = sync_result(&wid, &r, now_ms() - t0);
            if let Some(s) = &o.opened.store {
                self.reg_patch(&wid, json!({ "lastSync": now_ms(), "lastScan": res }))?;
                self.learn_dims(s)?;
            }
            out.push(res);
        }
        Ok(Value::Array(out))
    }

    fn pipeline_names(&self, env: &str) -> Vec<String> {
        match self.db.call("pipelines", Some(env), "listRecords", vec![]) {
            Ok(Value::Array(a)) => a
                .iter()
                .map(|r| {
                    let v = r.get("value").and_then(|v| v.get("name")).filter(|n| truthy(Some(n)));
                    match v {
                        Some(n) => identity::js_string(n),
                        None => r.get("key").map(identity::js_string).unwrap_or_default(),
                    }
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// One profile's pipeline in one workspace, without a store.
    fn pipeline_for(&self, env: &str, p: &Prof, names: Option<&[String]>) -> Option<(String, u64)> {
        let parts = p
            .profile
            .as_ref()
            .and_then(|x| x.get("identity"))
            .filter(|i| i.is_object())
            .map(Parts::from_json)
            .or_else(|| emb::parts_of_record(&p.record))?;
        if parts.r#type.is_empty() {
            return None;
        }
        let dims = p.record.get("dims").filter(|d| truthy(Some(d))).cloned().unwrap_or(Value::Null);
        match names {
            Some(n) => identity::find_pipeline(n, &parts, &dims),
            None => identity::find_pipeline(&self.pipeline_names(env), &parts, &dims),
        }
    }

    /// okcode's pipelines in a workspace env that no known profile addresses.
    fn orphans_of(&self, env: &str) -> Vec<String> {
        let names = self.pipeline_names(env);
        let mut claimed = std::collections::HashSet::new();
        for p in self.profile_list() {
            if let Some((pl, _)) = self.pipeline_for(env, &p, Some(&names)) {
                claimed.insert(pl);
            }
        }
        names.into_iter().filter(|n| identity::is_ours(&json!(n)) && !claimed.contains(n)).collect()
    }

    fn indexer_stats(&self, scoped: &str) -> Value {
        let attempt = || -> Result<Value> {
            // the live indexer's own stats where this process runs it (engines.apiCall: {value} | {__absent});
            // not running here (a passive process, or no such engine) → its durable stats below
            let live = match self.db.call("engines", None, "apiCall", vec![json!("indexer"), json!(scoped), json!("stats"), json!([])]) {
                Ok(r) if r.get("__absent").is_none() => r.get("value").cloned().unwrap_or(Value::Null),
                Ok(_) => Value::Null,
                Err(e) if matches!(e.code.as_deref(), Some("ENGINE_SYSTEM_NOT_STARTED") | Some("ENGINE_NOT_FOUND") | Some("NOT_FOUND")) => Value::Null,
                Err(e) => return Err(e),
            };
            if !live.is_null() {
                return Ok(live);
            }
            if let Some(sep) = scoped.find(':').filter(|s| *s > 0) {
                let _ = self.db.call("embeddings", None, "ensureTypeEnv", vec![json!(&scoped[..sep]), json!(FILES)]);
            }
            self.db.call("embeddings", None, "durableStats", vec![json!(scoped)])
        };
        attempt().unwrap_or_else(|e| json!({ "error": e.message }))
    }

    fn fts_state(&self, ty: &str, env: &str) -> Value {
        match self.db.call("fts", None, "list", vec![json!(ty), json!(env)]) {
            Ok(Value::Array(a)) => match a.iter().find(|x| x["name"] == FTS_NAME) {
                None => json!({ "status": "absent" }),
                Some(x) => json!({
                    "status": x.get("status").filter(|v| truthy(Some(v))).cloned().unwrap_or(Value::Null),
                    "lag": x.get("lag").cloned().unwrap_or(Value::Null),
                    "error": x.get("error").filter(|v| truthy(Some(v))).cloned().unwrap_or(Value::Null),
                }),
            },
            // JS: `.find` on a non-array result throws -> 'unavailable'
            Ok(_) => json!({ "status": "unavailable", "error": "(db.fts.list(...) || []).find is not a function" }),
            Err(e) => json!({ "status": "unavailable", "error": e.message }),
        }
    }

    fn embedder_entry(&self, p: &Prof, active: &Option<String>, pipeline: Option<&str>, dims: &Value, stats: &Value, error: Option<Value>, waiting: Option<Value>) -> Value {
        let counts = stats.get("doc_counts").cloned().unwrap_or(Value::Null);
        let c = |k: &str| counts.get(k).filter(|v| truthy(Some(v))).cloned().unwrap_or(json!(0));
        let space = emb::describe(&p.record, dims);
        let (done, pending, failed) = (c("done"), c("pending"), c("failed"));
        let mut e = json!({
            "name": p.name,
            "pipeline": pipeline,
            "type": space["type"], "endpoint": space["endpoint"], "model": space["model"], "dims": space["dims"], "identity": space["identity"],
            "active": active.as_deref() == Some(p.name.as_str()),
            "done": done, "pending": pending, "failed": failed,
            "vectors": if stats.get("vector_count").is_some_and(Value::is_number) { stats["vector_count"].clone() } else { Value::Null },
        });
        let stats_err = stats.get("error").filter(|v| truthy(Some(v))).cloned();
        let error = error.filter(|v| truthy(Some(v)));
        if p.needs_config {
            e["state"] = json!("needs-config");
            e["error"] = json!(p.reason);
        } else if error.is_some() || stats_err.is_some() {
            e["state"] = json!("error");
            e["error"] = error.or(stats_err).unwrap_or(Value::Null);
        } else if let Some(w) = waiting.filter(|v| truthy(Some(v))) {
            e["state"] = json!("pending");
            e["reason"] = w;
        } else if pipeline.is_none() {
            e["state"] = json!("absent");
        } else {
            let (pn, fnum) = (pending.as_f64().unwrap_or(0.0), failed.as_f64().unwrap_or(0.0));
            e["state"] = json!(if pn > 0.0 { "building" } else if fnum > 0.0 { "failed" } else { "ready" });
        }
        e
    }

    fn workspace_status(&self, id: &str) -> Result<Value> {
        let reg = self.reg_get(false, id)?;
        let o = self.open_snapshot(id);
        if o.as_ref().is_some_and(|o| o.index == "none") {
            return Ok(json!({ "id": id, "env": Value::Null, "index": "none", "open": true, "lastSync": Value::Null, "files": 0, "symbols": 0, "fts": {}, "embedders": [], "orphaned": [] }));
        }
        let env = self.env_of(id)?;
        let mut out = json!({
            "id": id,
            "env": env_name_for(id)?,
            "open": o.is_some(),
            "lastSync": reg.as_ref().and_then(|r| r.get("lastSync")).filter(|v| truthy(Some(v))).cloned().unwrap_or(Value::Null),
            "files": 0, "symbols": 0, "fts": {}, "embedders": [], "orphaned": [],
        });
        if let Some(o) = &o {
            let skipped = o.opened.ws.scan_warnings();
            if let Some(a) = skipped.as_array().filter(|a| !a.is_empty()) {
                out["warnings"] = Value::Array(
                    a.iter()
                        .map(|w| {
                            let mut m = Map::new();
                            m.insert("kind".into(), json!("scan-skipped"));
                            if let Some(wo) = w.as_object() {
                                for (k, v) in wo {
                                    m.insert(k.clone(), v.clone());
                                }
                            }
                            Value::Object(m)
                        })
                        .collect(),
                );
            }
        }
        let Some(env) = env else {
            out["missing"] = json!(true);
            return Ok(out);
        };
        let count = |t: &str| -> Value {
            let has = self.db.call("env", Some(&env), "hasType", vec![json!(t)]).map(|v| v == json!(true)).unwrap_or(false);
            if !has {
                return json!(0);
            }
            self.db.call("env", Some(&env), "getCount", vec![json!(t), json!({})]).unwrap_or(json!(0))
        };
        out["files"] = count(FILES);
        out["symbols"] = count(SYMBOLS);
        let files_fts = self.fts_state(FILES, &env);
        let mode = match o.as_ref().and_then(|o| o.opened.store.as_ref()) {
            Some(s) => s.fts_mode.as_str(),
            None => {
                if files_fts["status"] == "absent" {
                    "symbols"
                } else {
                    "full"
                }
            }
        };
        out["fts"] = json!({ "mode": mode, "symbols": self.fts_state(SYMBOLS, &env), "files": files_fts });

        let store_profiles: IndexMap<String, Value> = match o.as_ref().and_then(|o| o.opened.store.as_ref()) {
            Some(s) => s.profiles()?.into_iter().map(|sp| (sp["name"].as_str().unwrap_or_default().to_string(), sp)).collect(),
            None => IndexMap::new(),
        };
        let active = self.lock().active.clone();
        let mut embs = Vec::new();
        for p in self.profile_list() {
            let sp = store_profiles.get(&p.name);
            let g = |k: &str| sp.and_then(|s| s.get(k)).filter(|v| truthy(Some(v))).cloned();
            if let Some(pl) = g("pipeline") {
                embs.push(self.embedder_entry(&p, &active, pl.as_str(), &g("dims").unwrap_or(Value::Null), &g("status").unwrap_or(Value::Null), g("error"), None));
                continue;
            }
            if let Some(err) = g("error") {
                embs.push(self.embedder_entry(&p, &active, None, &Value::Null, &Value::Null, Some(err), None));
                continue;
            }
            if let Some(w) = g("pending") {
                embs.push(self.embedder_entry(&p, &active, None, &g("dims").unwrap_or(Value::Null), &Value::Null, None, Some(w)));
                continue;
            }
            match self.pipeline_for(&env, &p, None) {
                None => embs.push(self.embedder_entry(&p, &active, None, &Value::Null, &Value::Null, None, None)),
                Some((pl, d)) => {
                    let stats = self.indexer_stats(&format!("{env}:{pl}"));
                    embs.push(self.embedder_entry(&p, &active, Some(&pl), &json!(d), &stats, None, None));
                }
            }
        }
        out["embedders"] = Value::Array(embs);
        let mut orphaned = Vec::new();
        for pl in self.orphans_of(&env) {
            let stats = self.indexer_stats(&format!("{env}:{pl}"));
            let c = stats.get("doc_counts").and_then(|c| c.get("done")).filter(|v| truthy(Some(v))).cloned().unwrap_or(json!(0));
            orphaned.push(json!({
                "pipeline": pl,
                "done": c,
                "vectors": if stats.get("vector_count").is_some_and(Value::is_number) { stats["vector_count"].clone() } else { Value::Null },
            }));
        }
        out["orphaned"] = Value::Array(orphaned);
        Ok(out)
    }

    fn profile_entry(&self, p: &Prof, active: &Option<String>) -> Value {
        let mut m = Map::new();
        m.insert("name".into(), json!(p.name));
        if let Some(d) = emb::describe(&p.record, &Value::Null).as_object() {
            for (k, v) in d {
                m.insert(k.clone(), v.clone());
            }
        }
        m.insert("active".into(), json!(active.as_deref() == Some(p.name.as_str())));
        m.insert("state".into(), json!(if p.needs_config { "needs-config" } else { "configured" }));
        if p.needs_config {
            m.insert("error".into(), json!(p.reason));
        }
        Value::Object(m)
    }

    /// A short, secret-free summary of what okdb has in effect (status().license).
    fn license_summary(&self) -> Value {
        let attempt = || -> Result<Value> {
            let e = self.db.call("licenses", None, "effective", vec![])?;
            let free = truthy(e.get("free"));
            let pending = if free {
                self.db.call("licenses", None, "list", vec![])?.as_array().and_then(|a| a.iter().find(|l| truthy(l.get("needsActivation"))).cloned())
            } else {
                None
            };
            let mut m = Map::new();
            m.insert("status".into(), json!(if free { if pending.is_some() { "needs-activation" } else { "free" } } else { "active" }));
            for k in ["type", "licensee", "expiresAt", "enforced"] {
                m.insert(k.into(), e.get(k).cloned().unwrap_or(Value::Null));
            }
            if let Some(p) = pending {
                m.insert("id".into(), p["id"].clone());
                m.insert("pin".into(), p["pin"].clone());
            }
            Ok(Value::Object(m))
        };
        attempt().unwrap_or(Value::Null)
    }

    fn role(&self) -> Result<Value> {
        self.db.call("db", None, "role", vec![])
    }

    pub fn status(&self, id: Option<&str>) -> Result<Value> {
        let ids = match id {
            None => self.workspace_ids()?,
            Some(i) => {
                self.assert_known(i)?;
                vec![i.to_string()]
            }
        };
        let mut list = Vec::new();
        for wid in ids {
            list.push(self.workspace_status(&wid)?);
        }
        let (active, profs) = {
            let st = self.lock();
            (st.active.clone(), st.profiles.values().cloned().collect::<Vec<_>>())
        };
        let role = match self.role() {
            Ok(r) => json!({ "processors": r["processors"] != json!(false), "engines": r["engines"] != json!(false) }),
            Err(_) => Value::Null,
        };
        Ok(json!({
            "workspaces": list,
            "active": active,
            "embedders": profs.iter().map(|p| self.profile_entry(p, &active)).collect::<Vec<_>>(),
            "role": role,
            "license": self.license_summary(),
        }))
    }

    /// reset(id, { scope: 'vectors' | 'fts' | 'all', embedder })
    pub fn reset(&self, id: &str, opts: &Value) -> Result<Value> {
        self.assert_known(id)?;
        self.refuse_unindexed(id, "reset()")?;
        let scope = match opts.get("scope") {
            None => "vectors".to_string(),
            Some(Value::String(s)) => s.clone(),
            Some(v) => identity::js_string(v),
        };
        // a non-null, non-string embedder matches no profile
        let embedder: Option<String> = match opts.get("embedder") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => Some("\u{0}no-such-profile".into()),
        };
        match scope.as_str() {
            "vectors" => {
                let Some(env) = self.env_of(id)? else {
                    return Ok(json!({ "id": id, "scope": scope, "rebuilt": [] }));
                };
                let mut rebuilt: Vec<Value> = Vec::new();
                for p in self.profile_list() {
                    if embedder.as_deref().is_some_and(|e| p.name != e) || p.needs_config {
                        continue;
                    }
                    let Some((pl, _)) = self.pipeline_for(&env, &p, None) else { continue };
                    if rebuilt.iter().any(|r| r["pipeline"] == json!(pl)) {
                        continue;
                    }
                    let result = self.db.call("pipelines", Some(&env), "rebuild", vec![json!(pl)])?;
                    rebuilt.push(json!({ "name": p.name, "pipeline": pl, "result": result }));
                }
                Ok(json!({ "id": id, "scope": scope, "rebuilt": rebuilt }))
            }
            "fts" => {
                // The content index reads a RESOLVED field, served only where the access facade is.
                let o = self.require_open(id, "reset({ scope: 'fts' })")?;
                let env = o.opened.store.as_ref().map(|s| s.env_name.clone()).unwrap_or_default();
                let drains = self.role().map(|r| r["processors"] != json!(false)).unwrap_or(true);
                let mut out = Vec::new();
                for ty in [SYMBOLS, FILES] {
                    let has = self.db.call("fts", None, "has", vec![json!(ty), json!(FTS_NAME), json!(env)])?;
                    if has != json!(true) {
                        continue;
                    }
                    self.db.call("fts", None, "reset", vec![json!(ty), json!(FTS_NAME), json!(true), json!(env)])?;
                    if drains {
                        let _ = self.db.call("fts", None, "ready", vec![json!(ty), json!(FTS_NAME), json!(env)]);
                    }
                    out.push(json!(ty));
                }
                Ok(json!({ "id": id, "scope": scope, "reset": out }))
            }
            "all" => {
                let Some(o) = self.open_snapshot(id) else {
                    // No facade here: drop the env; the next addWorkspace rescans.
                    self.drop_env(id, None)?;
                    self.reg_patch(id, json!({ "lastSync": Value::Null, "lastScan": Value::Null }))?;
                    return Ok(json!({ "id": id, "scope": scope, "dropped": true, "rescanned": false }));
                };
                let _ = o.opened.ws.close();
                self.lock().opened.shift_remove(id);
                self.drop_env(id, o.opened.store.clone())?;
                let fts = o.opened.store.as_ref().map(|s| s.fts_mode).unwrap_or_default();
                let n = self.open_one(id, o.access.clone(), o.caps, &o.options, fts)?;
                let t0 = now_ms();
                let r = n.ws.refresh(RefreshOpts::default())?;
                let res = sync_result(id, &r, now_ms() - t0);
                self.reg_patch(id, json!({ "lastSync": now_ms(), "lastScan": res }))?;
                if let Some(s) = &n.store {
                    self.learn_dims(s)?;
                }
                let mut out = json!({ "id": id, "scope": scope, "dropped": true, "rescanned": true });
                if let Some(rm) = res.as_object() {
                    for (k, v) in rm {
                        out[k] = v.clone();
                    }
                }
                Ok(out)
            }
            other => Err(Error::msg(format!("reset: unknown scope \"{other}\" (vectors | fts | all)"))),
        }
    }

    pub fn add_embedder(&self, hp: HostProfile) -> Result<Value> {
        let name = hp.name.clone();
        let prior = self.lock().profiles.get(&name).cloned();
        if prior.as_ref().is_some_and(|p| !p.needs_config) {
            return Err(coded(format!("embedder \"{name}\" already exists \u{2014} removeEmbedder it first"), "OKCODE_EMBEDDER_EXISTS"));
        }
        let t = self.translate(&hp)?;
        // A needs-config profile being re-supplied keeps what it learned.
        let mut record = t.record.clone();
        if let Some(pr) = &prior {
            if !truthy(record.get("dims")) && truthy(pr.record.get("dims")) {
                record["dims"] = pr.record["dims"].clone();
            }
        }
        let mut profile = t.profile;
        if truthy(record.get("dims")) {
            profile["dims"] = record["dims"].clone();
        }
        let entry = Prof { name: name.clone(), record: record.clone(), profile: Some(profile.clone()), needs_config: false, reason: None };
        let set_active = {
            let mut st = self.lock();
            st.profiles.insert(name.clone(), entry);
            st.active.is_none()
        };
        self.persist_profile(&record)?;
        if set_active {
            self.lock().active = Some(name.clone());
            self.meta_put(false, T_SET, "active", json!({ "name": name }))?;
        }
        let opened: Vec<(String, Arc<Store>)> = self.lock().opened.iter().filter_map(|(k, o)| o.opened.store.clone().map(|s| (k.clone(), s))).collect();
        let mut results = Vec::new();
        for (id, store) in &opened {
            let st = store.add_profile(profile.clone())?;
            let g = |k: &str| st.get(k).filter(|v| truthy(Some(v))).cloned().unwrap_or(Value::Null);
            let mut m = serde_json::Map::new();
            m.insert("id".into(), json!(id));
            if let Some(v) = st.get("pipeline") {
                m.insert("pipeline".into(), v.clone());
            }
            if let Some(v) = st.get("dims") {
                m.insert("dims".into(), v.clone());
            }
            m.insert("error".into(), g("error"));
            m.insert("pending".into(), g("pending"));
            results.push(Value::Object(m));
        }
        for (_, store) in &opened {
            self.learn_dims(store)?;
        }
        Ok(json!({ "name": name, "workspaces": results }))
    }

    /// useEmbedder(name) — sync in JS; the settings write is done before it returns
    pub fn use_embedder(&self, name: &str) -> Result<String> {
        {
            let mut st = self.lock();
            let p = st.profiles.get(name).ok_or_else(|| coded(format!("no embedder profile named \"{name}\""), "OKCODE_UNKNOWN_EMBEDDER"))?;
            if p.needs_config {
                return Err(coded(format!("embedder \"{name}\" needs config: {}", p.reason.clone().unwrap_or_default()), "OKCODE_NEEDS_CONFIG"));
            }
            st.active = Some(name.to_string());
        }
        if let Err(e) = self.meta_put(true, T_SET, "active", json!({ "name": name })) {
            self.warn(&format!("[okcode] persist failed: {}", e.message));
        }
        Ok(name.to_string())
    }

    pub fn remove_embedder(&self, name: &str) -> Result<Value> {
        let p = self
            .lock()
            .profiles
            .get(name)
            .cloned()
            .ok_or_else(|| coded(format!("no embedder profile named \"{name}\""), "OKCODE_UNKNOWN_EMBEDDER"))?;
        let mut removed = Vec::new();
        let mut ids: Vec<String> = self.reg_list(false)?.iter().map(|r| r["id"].as_str().unwrap_or_default().to_string()).collect();
        for k in self.lock().opened.keys() {
            if !ids.contains(k) {
                ids.push(k.clone());
            }
        }
        for id in ids {
            let Some(env) = self.env_of(&id)? else { continue };
            let Some((found, _)) = self.pipeline_for(&env, &p, None) else { continue };
            // Another profile with the same identity shares the pipeline.
            let mut shared = false;
            for q in self.profile_list() {
                if q.name == p.name {
                    continue;
                }
                if self.pipeline_for(&env, &q, None).is_some_and(|(f, _)| f == found) {
                    shared = true;
                }
            }
            if shared {
                continue;
            }
            self.db.call("pipelines", Some(&env), "remove", vec![json!(found)])?;
            removed.push(json!({ "id": id, "pipeline": found }));
        }
        self.lock().profiles.shift_remove(name);
        self.meta_remove(T_EMB, name)?;
        let was_active = self.lock().active.as_deref() == Some(name);
        if was_active {
            let next = self.lock().profiles.values().find(|p| !p.needs_config && p.profile.is_some()).map(|p| p.name.clone());
            self.lock().active = next.clone();
            match next {
                Some(n) => self.meta_put(false, T_SET, "active", json!({ "name": n }))?,
                None => self.meta_remove(T_SET, "active")?,
            }
        }
        Ok(json!({ "name": name, "removed": removed }))
    }

    pub fn remove_orphaned(&self, id: Option<&str>) -> Result<Value> {
        let ids = match id {
            None => self.workspace_ids()?,
            Some(i) => {
                self.assert_known(i)?;
                vec![i.to_string()]
            }
        };
        let mut removed = Vec::new();
        for wid in ids {
            let Some(env) = self.env_of(&wid)? else { continue };
            for pl in self.orphans_of(&env) {
                self.db.call("pipelines", Some(&env), "remove", vec![json!(pl)])?;
                removed.push(json!({ "id": wid, "pipeline": pl }));
            }
        }
        Ok(json!({ "removed": removed }))
    }

    /// compare(id, query, names?, { limit, text })
    pub fn compare(&self, id: &str, query: &Value, names: &Value, opts: &Value) -> Result<Value> {
        let o = self.require_open(id, "compare")?;
        self.refuse_unindexed(id, "compare()")?;
        let list: Vec<Value> = match names {
            Value::Null => self.usable_profiles().into_iter().map(|p| json!(p.name)).collect(),
            Value::Array(a) => a.clone(),
            other => vec![other.clone()],
        };
        let store = o.opened.store.clone().ok_or_else(|| coded(format!("workspace \"{id}\" is not indexed \u{2014} compare() needs an index"), "OKCODE_NOT_INDEXED"))?;
        let limit = opts.get("limit").cloned().unwrap_or(json!(8));
        let text = opts.get("text").cloned().unwrap_or(json!(false));
        let mut out = Map::new();
        for n in list {
            let key = identity::js_string(&n);
            self.resolve_profile(&n)?;
            out.insert(key, store.call("ask", &[query.clone(), json!({ "profile": n, "limit": limit, "text": text })])?);
        }
        Ok(Value::Object(out))
    }

    /// The ask() an okcode workspace exposes: without `profile`, the ACTIVE profile
    /// (okcode.js openOne's ws.ask wrapper).
    /// Tied to the workspace it was built for (`o`), not to whatever is open under its id now.
    pub fn ws_ask(&self, o: &Opened, query: &Value, opts: &Value) -> Result<Value> {
        let has = o.store.as_ref().is_some_and(|s| s.has_profiles());
        if !has {
            return o.ws.call("ask", &[query.clone(), opts.clone()]);
        }
        let profile = self.resolve_profile(opts.get("profile").unwrap_or(&Value::Null))?;
        let mut m = opts.as_object().cloned().unwrap_or_default();
        m.insert("profile".into(), json!(profile));
        o.ws.call("ask", &[query.clone(), Value::Object(m)])
    }

    /// An open workspace as the tool layer sees it (its `ask` is `ws_ask`).
    pub fn tool_ws(self: &Arc<Self>, o: Opened, root: Option<String>) -> Arc<dyn ToolWorkspace> {
        Arc::new(OkToolWs { okc: self.clone(), native: NativeWs { ws: o.ws.clone(), root }, opened: o })
    }

    /// the `active` getter
    pub fn active(&self) -> Option<String> {
        self.lock().active.clone()
    }

    /// embedders() — [{ name, type, endpoint, model, dims, identity, active, state, error? }]
    pub fn embedders(&self) -> Value {
        let (active, profs) = {
            let st = self.lock();
            (st.active.clone(), st.profiles.values().cloned().collect::<Vec<_>>())
        };
        Value::Array(profs.iter().map(|p| self.profile_entry(p, &active)).collect())
    }

    /// close() — closes the open workspaces, closes the db when owned.
    pub fn close(&self) -> Result<()> {
        let all: Vec<OpenWs> = {
            let mut st = self.lock();
            let v = st.opened.values().cloned().collect();
            st.opened.clear();
            v
        };
        for o in all {
            let _ = o.opened.ws.close();
        }
        if self.owns_db {
            self.db.close()?;
        }
        Ok(())
    }

    /// The binding's dispatch for the JSON-only methods (args as the JS caller passed them).
    /// addWorkspace (needs an access handle) and open are separate binding entries.
    pub fn call(&self, method: &str, args: &[Value]) -> Result<Value> {
        let a = |i: usize| args.get(i).cloned().unwrap_or(Value::Null);
        // Null/absent = no id ('all'); non-strings were rejected above, before this is used
        let opt_s = |i: usize| match args.get(i) {
            Some(Value::String(s)) => Some(s.clone()),
            _ => None,
        };
        // A non-string id never matches the opened Map / registry (strict keys); where JS reaches
        // envNameFor it throws 'a workspace id is required'; otherwise it is reported as String(id).
        let name_of = |i: usize| match args.get(i) {
            None => "undefined".to_string(),
            Some(v) => js_string(v),
        };
        let is_str = |i: usize| matches!(args.get(i), Some(Value::String(_)));
        let id_required = || Error::msg("a workspace id is required");
        match method {
            "workspaces" => self.workspaces(),
            "removeWorkspace" | "checkId" if !is_str(0) => Err(id_required()),
            "sync" if !matches!(args.first(), None | Some(Value::Null | Value::String(_) | Value::Object(_) | Value::Array(_))) => {
                Err(err_not_open(&name_of(0), "sync"))
            }
            "status" | "removeOrphaned" if !matches!(args.first(), None | Some(Value::Null | Value::String(_))) => Err(err_unknown(&name_of(0))),
            "reset" if !is_str(0) => Err(err_unknown(&name_of(0))),
            "compare" if !is_str(0) => Err(err_not_open(&name_of(0), "compare")),
            "useEmbedder" if !is_str(0) => Err(coded(format!("no embedder profile named \"{}\"", name_of(0)), "OKCODE_UNKNOWN_EMBEDDER")),
            "removeEmbedder" if !is_str(0) => Err(coded(format!("no embedder profile named \"{}\"", name_of(0)), "OKCODE_UNKNOWN_EMBEDDER")),
            "hasEmbedder" if !is_str(0) => Ok(json!(false)),
            "removeWorkspace" => self.remove_workspace(&opt_s(0).unwrap_or_default()),
            "sync" => {
                // sync({ force }) ≡ sync(null, { force })
                let (id, o) = if a(0).is_object() || a(0).is_array() { (None, a(0)) } else { (opt_s(0), a(1)) };
                self.sync(id.as_deref(), truthy(o.get("force")))
            }
            "status" => self.status(opt_s(0).as_deref()),
            "reset" => self.reset(&opt_s(0).unwrap_or_default(), &a(1)),
            "useEmbedder" => self.use_embedder(&opt_s(0).unwrap_or_default()).map(Value::String),
            "removeEmbedder" => self.remove_embedder(&opt_s(0).unwrap_or_default()),
            "removeOrphaned" => self.remove_orphaned(opt_s(0).as_deref()),
            "compare" => self.compare(&opt_s(0).unwrap_or_default(), &a(1), &a(2), &a(3)),
            // lib/okcode.js's pre-checks (sync): an id env_name_for accepts; an existing, usable profile
            "checkId" => env_name_for(&opt_s(0).unwrap_or_default()).map(|_| Value::Null),
            "hasEmbedder" => Ok(json!(self.lock().profiles.get(&opt_s(0).unwrap_or_default()).is_some_and(|p| !p.needs_config))),
            "active" => Ok(self.active().map(Value::String).unwrap_or(Value::Null)),
            "embedders" => Ok(self.embedders()),
            "close" => self.close().map(|_| Value::Null),
            other => Err(Error::msg(format!("okcode has no method {other}"))),
        }
    }
}
