//! bin/okcode.js boot: root/store/id resolution, the in-folder store's .gitignore line,
//! --embedder-config, --license, openOkcode and openWs.

use crate::args::Args;
use crate::render::err;
use crate::Exit;
use okcode_core::analysis::access::LocalFs;
use okcode_core::db::{DbHost, DbOptions};
use okcode_core::okcode::{AddOptions, HostProfile, OpenOptions, Opened};
use okcode_core::workspace::Caps;
use okcode_core::Okcode;
use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

/// Where this invocation points: the resolved --root, --store and --id.
pub struct Boot {
    pub root: PathBuf,
    pub shared_store: bool,
    pub store_dir: PathBuf,
    pub ws_id: String,
}

/// path.resolve: absolute against cwd, `.`/`..` folded lexically (no symlink resolution).
pub fn resolve(p: &str) -> PathBuf {
    let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let joined = base.join(p);
    let mut out = PathBuf::from("/");
    for c in joined.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(x) => out.push(x),
            _ => {}
        }
    }
    out
}

impl Boot {
    pub fn new(a: &Args) -> Boot {
        let cwd = std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "/".into());
        // String(flags.root || cwd): a bare `--root` is `true` → "true", as JS
        let root = resolve(&a.get("root").unwrap_or(cwd));
        let shared_store = a.flags.contains_key("store");
        let store_dir = match a.get("store") {
            Some(s) => resolve(&s),
            None => root.join(".okcode"),
        };
        let ws_id = a.get("id").unwrap_or_else(|| {
            if shared_store {
                root.file_name().map(|n| n.to_string_lossy().into_owned()).filter(|n| !n.is_empty()).unwrap_or_else(|| "workspace".into())
            } else {
                "default".into()
            }
        });
        Boot { root, shared_store, store_dir, ws_id }
    }

    /// The in-folder store must never be committed: append `.okcode/` to an existing .gitignore.
    fn ignore_local_store(&self) {
        if self.shared_store {
            return;
        }
        let gi = self.root.join(".gitignore");
        let Ok(text) = std::fs::read_to_string(&gi) else { return };
        if text.lines().any(|l| matches!(l.trim_end(), ".okcode" | ".okcode/" | "/.okcode" | "/.okcode/")) {
            return;
        }
        let sep = if text.ends_with('\n') || text.is_empty() { "" } else { "\n" };
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&gi) {
            let _ = write!(f, "{sep}.okcode/\n");
        }
    }
}

/// --embedder-config FILE: `{ embedders: {...}, active }` or the profile map itself.
fn embedder_config(a: &Args) -> Result<(Vec<HostProfile>, Option<String>), Exit> {
    let Some(file) = a.get("embedder-config") else { return Ok((Vec::new(), None)) };
    let cfg: Value = std::fs::read_to_string(&file)
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
        .map_err(|e| Exit::fail(format!("--embedder-config {file}: {e}")))?;
    let map = match cfg.get("embedders") {
        Some(e @ (Value::Object(_) | Value::Array(_))) => e.clone(),
        _ => cfg.clone(),
    };
    let embedders = match &map {
        Value::Object(m) => m.iter().map(|(name, c)| HostProfile { name: name.clone(), cfg: c.clone(), fn_keys: Vec::new() }).collect(),
        _ => Vec::new(),
    };
    let active = match cfg.get("active") {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => a.get("embedder").filter(|s| !s.is_empty()),
    };
    Ok((embedders, active))
}

/// The open store and okcode. Closed in that order on drop of the CLI run (`close`).
pub struct Session {
    pub db: Arc<DbHost>,
    pub oc: Arc<Okcode>,
}

impl Session {
    pub fn close(&self) {
        let _ = self.oc.close();
        let _ = self.db.close();
    }
}

/// openOkcode: the CLI opens okdb itself (okdb's console logging off stdout first: stdout is the
/// answer and --json must stay parseable), installs --license, then okcode.open on that db.
pub fn open_okcode(b: &Boot, a: &Args) -> Result<Session, Exit> {
    std::fs::create_dir_all(&b.store_dir).map_err(|e| Exit::fail(format!("{}: {e}", b.store_dir.display())))?;
    b.ignore_local_store();
    let (embedders, active) = embedder_config(a)?;
    let verbose = std::env::var("OKCODE_VERBOSE").is_ok_and(|v| !v.is_empty());
    // stdout is the answer (--json must stay parseable): okdb's log goes to stderr with OKCODE_VERBOSE, else nowhere.
    // The sink is process-wide and set by the first open (this one).
    let log = if verbose { json!({ "console": "stderr", "level": "info" }) } else { json!({ "console": "off" }) };
    let db = DbHost::new(&b.store_dir, DbOptions { raw: json!({ "log": log }) }).map_err(Exit::from_core)?;
    db.open().map_err(Exit::from_core)?;
    if a.flags.contains_key("license") {
        install_license(&db, a)?;
    }
    let log: Option<okcode_core::store::LogFn> = if verbose { Some(Arc::new(|s: &str| err(s))) } else { Some(Arc::new(|_: &str| {})) };
    let opts = OpenOptions { owns_db: false, embedders, active, log, ..Default::default() };
    let oc = match Okcode::open(db.clone(), opts) {
        Ok(oc) => oc,
        Err(e) => {
            let _ = db.close();
            return Err(Exit::from_core(e));
        }
    };
    Ok(Session { db, oc })
}

/// --license FILE: the CLI hands okcode its own db, so it installs the license itself.
fn install_license(db: &DbHost, a: &Args) -> Result<(), Exit> {
    let file = crate::need(a.get("license").filter(|_| !a.on("license")), "--license needs a license file")?;
    let text = std::fs::read_to_string(&file).map_err(|e| Exit::fail(format!("--license {file}: {e}")))?;
    let r = db.call_sync("licenses", None, "add", vec![json!(text)]).map_err(|e| Exit::fail(format!("--license {file}: {e}")))?;
    let s = |k: &str| r.get(k).map(crate::render::js_str).unwrap_or_else(|| "undefined".into());
    if r.get("needsActivation").is_some_and(truthy) {
        err(&format!(
            "# license {} needs activation on this node: send PIN {} to the vendor, then append its token to the license file (second line) and pass --license again",
            s("id"),
            s("pin")
        ));
    } else if r.get("changed").is_some_and(truthy) {
        let lic = r.get("licensee").filter(|v| truthy(v)).map(|v| format!(" ({})", crate::render::js_str(v))).unwrap_or_default();
        err(&format!("# license installed: {}{lic}", s("type")));
    }
    Ok(())
}

pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// openWs: the workspace this invocation is about — --root through the native local facade.
pub fn open_ws(b: &Boot, a: &Args) -> Result<(Session, Opened), Exit> {
    let s = open_okcode(b, a)?;
    let r = (|| {
        if !b.root.is_dir() {
            return Err(Exit::fail(format!("--root {} is not a directory", b.root.display())));
        }
        let fts = a.get("fts");
        if let Some(f) = &fts {
            if f != "full" && f != "symbols" {
                return Err(Exit::fail("--fts must be full or symbols".into()));
            }
        }
        if let Some(o) = s.oc.workspace(&b.ws_id) {
            return Ok(o);
        }
        let root = b.root.to_string_lossy().into_owned();
        let fs = LocalFs::new(Path::new(&root)).map_err(|e| Exit::fail(e))?;
        let opts = AddOptions {
            index: json!("full"),
            fts: fts.map(Value::String).unwrap_or(Value::Null),
            access_kind: Some("localFs".into()),
            access_root: Some(root),
            ..Default::default()
        };
        s.oc.add_workspace(&b.ws_id, Arc::new(fs), Caps::LOCAL_FS, opts).map_err(Exit::from_core)
    })();
    match r {
        Ok(o) => Ok((s, o)),
        Err(e) => {
            s.close();
            Err(e)
        }
    }
}
