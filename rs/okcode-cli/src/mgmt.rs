//! Management commands (MGMT_COMMANDS) and `stats`: status, sync, reset, embedders, workspaces,
//! remove-workspace.

use crate::args::Args;
use crate::open::{open_okcode, open_ws, truthy, Boot, Session};
use crate::render::{err, iso, j, js_str, out, pad, pad_str, tree};
use crate::{need, Exit};
use okcode_core::okcode::Opened;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

type R = Result<(), Exit>;

/// Runs `f` on an open session and closes it whatever happens (the JS `finally` of main).
fn with<T>(s: Session, f: impl FnOnce(&Session) -> Result<T, Exit>) -> Result<T, Exit> {
    let r = f(&s);
    s.close();
    r
}

pub fn status(b: &Boot, a: &Args) -> R {
    let (s, _ws) = open_ws(b, a)?;
    with(s, |s| {
        let id = if a.on("all") { None } else { Some(b.ws_id.as_str()) };
        let st = s.oc.status(id).map_err(Exit::from_core)?;
        if a.on("json") {
            j(&st);
        } else {
            out(&tree(&st, ""));
        }
        Ok(())
    })
}

pub fn sync(b: &Boot, a: &Args) -> R {
    let (s, opened) = open_ws(b, a)?;
    with(s, |s| {
        let r = s.oc.sync(Some(&b.ws_id), a.on("force")).map_err(Exit::from_core)?;
        // No daemon: this process is what keeps indexing alive, so by default it stays until the
        // embeddings are current. Other commands leave unfinished embedding to the next run.
        if !a.on("no-wait") {
            wait_embedded(s, &opened, &b.ws_id);
        }
        if a.on("json") {
            j(&if r.is_null() { json!({ "ok": true }) } else { r.clone() });
            return Ok(());
        }
        let rows: Vec<&Value> = match &r {
            Value::Array(x) => x.iter().collect(),
            Value::Object(_) => vec![&r],
            _ => vec![],
        };
        if rows.is_empty() {
            out(&format!("synced {}", b.ws_id));
        }
        for x in rows {
            let has = |k: &str| x.get(k).is_some();
            if has("id") && has("scanned") {
                let n = |k: &str| x.get(k).filter(|v| truthy(v)).map(js_str).unwrap_or_else(|| "0".into());
                let g = |k: &str| x.get(k).map(js_str).unwrap_or_else(|| "undefined".into());
                out(&format!("synced {}: {} scanned, {} changed, {} removed ({}ms)", g("id"), g("scanned"), n("changed"), n("removed"), g("ms")));
            } else {
                out(&tree(x, ""));
            }
        }
        Ok(())
    })
}

/// waitEmbedded: settle the store's profiles, printing `# name: done/total embedded` to stderr
/// every 2 s when the line changes.
fn wait_embedded(s: &Session, opened: &Opened, ws_id: &str) {
    let Some(store) = opened.store.as_ref().filter(|st| st.has_profiles()) else { return };
    let stop = Arc::new(AtomicBool::new(false));
    let ticker = {
        let (oc, stop, id) = (s.oc.clone(), stop.clone(), ws_id.to_string());
        std::thread::spawn(move || {
            let mut last = String::new();
            'outer: loop {
                for _ in 0..20 {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    if stop.load(Ordering::Relaxed) {
                        break 'outer;
                    }
                }
                // progress is best effort
                let Ok(st) = oc.status(Some(&id)) else { continue };
                let e = st["workspaces"][0]["embedders"].as_array().cloned().unwrap_or_default();
                let line = e
                    .iter()
                    .map(|x| {
                        let done = x["done"].as_f64().unwrap_or(0.0);
                        let pending = x["pending"].as_f64().unwrap_or(0.0);
                        format!("{}: {}/{} embedded", js_str(&x["name"]), crate::render::js_num(done), crate::render::js_num(done + pending))
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                if !line.is_empty() && line != last {
                    err(&format!("# {line}"));
                    last = line;
                }
            }
        })
    };
    let _ = store.settle(None);
    stop.store(true, Ordering::Relaxed);
    let _ = ticker.join();
}

pub fn reset(b: &Boot, a: &Args) -> R {
    let scope = a.get("scope").unwrap_or_default();
    if !["vectors", "fts", "all"].contains(&scope.as_str()) || a.on("scope") {
        return Err(Exit::fail("reset needs --scope vectors|fts|all".into()));
    }
    let (s, _ws) = open_ws(b, a)?;
    with(s, |s| {
        let r = s.oc.reset(&b.ws_id, &json!({ "scope": scope })).map_err(Exit::from_core)?;
        if a.on("json") {
            j(&if r.is_null() { json!({ "ok": true }) } else { r.clone() });
        } else if r.is_object() || r.is_array() {
            out(&tree(&r, ""));
        } else {
            out(&format!("reset {} ({scope})", b.ws_id));
        }
        Ok(())
    })
}

pub fn embedders(b: &Boot, a: &Args) -> R {
    let s = open_okcode(b, a)?;
    with(s, |s| {
        let list = s.oc.embedders();
        if a.on("json") {
            j(&list);
            return Ok(());
        }
        let items = list.as_array().cloned().unwrap_or_default();
        if items.is_empty() {
            out("no embedder profiles (pass --embedder-config FILE.json)");
        }
        for p in &items {
            let dims = if truthy(&p["dims"]) { format!("{}d", js_str(&p["dims"])) } else { "?d".into() };
            let endpoint = if truthy(&p["endpoint"]) { format!("  {}", js_str(&p["endpoint"])) } else { String::new() };
            out(&format!(
                "  {} {} {} {} {} {}{endpoint}",
                if truthy(&p["active"]) { "*" } else { " " },
                pad(&p["name"], 16),
                pad(&p["type"], 10),
                pad(&p["model"], 32),
                pad_str(&dims, 6),
                undef(p, "state"),
            ));
        }
        Ok(())
    })
}

/// `${o.k}` with a missing key printing 'undefined'
fn undef(o: &Value, k: &str) -> String {
    o.get(k).map(js_str).unwrap_or_else(|| "undefined".into())
}

pub fn workspaces(b: &Boot, a: &Args) -> R {
    let s = open_okcode(b, a)?;
    with(s, |s| {
        let list = s.oc.workspaces().map_err(Exit::from_core)?;
        if a.on("json") {
            j(&list);
            return Ok(());
        }
        let items = list.as_array().cloned().unwrap_or_default();
        if items.is_empty() {
            out("no workspaces registered");
        }
        for w in &items {
            if let Value::String(id) = w {
                out(&format!("  {id}"));
                continue;
            }
            let where_ = [&w["root"], &w["access"]["root"]].into_iter().find(|v| truthy(v)).map(js_str).unwrap_or_default();
            let kind = if truthy(&w["access"]["kind"]) { format!(" ({})", js_str(&w["access"]["kind"])) } else { String::new() };
            let synced = match w["lastSync"].as_f64() {
                Some(ms) if ms != 0.0 => format!("  synced {}", iso(ms)),
                _ => String::new(),
            };
            out(&format!("  {} {where_}{kind}{synced}", pad(&w["id"], 24)));
        }
        Ok(())
    })
}

pub fn remove_workspace(b: &Boot, a: &Args) -> R {
    let id = need(a.positional.first().cloned(), "remove-workspace needs <id>")?;
    let s = open_okcode(b, a)?;
    with(s, |s| {
        let r = s.oc.remove_workspace(&id).map_err(Exit::from_core)?;
        if a.on("json") {
            j(&if r.is_null() { json!({ "ok": true, "removed": id }) } else { r.clone() });
        } else {
            out(&format!("removed {id}"));
        }
        Ok(())
    })
}

/// stats (a workspace command): this workspace's counters.
pub fn stats(opened: &Opened, a: &Args) -> R {
    let st = opened.ws.call("stats", &[]).map_err(Exit::from_core)?;
    if a.on("json") {
        j(&st);
    } else {
        out(&tree(&st, ""));
    }
    Ok(())
}
