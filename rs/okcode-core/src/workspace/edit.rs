//! Edits and the syntax gate. OWNER: slice `edit` (also owns okcode-analysis/src/validate.rs).
//!
//! JS (src/workspace.js): 280-302 classifyCommit, 2104-2360 resolveSymbolInSnapshot,
//! resolveTargetInSnapshot, fileForTarget, explainTarget, palette, resolveModuleKind,
//! commitGuarded, readBack, ingestOwnWrite, readOnlyRefusal; api editBatch (3654), edit (3922),
//! writeAt (3936), writeWholeFile (3945), syntaxCheck (4043).
//! Writes go through `access.commit` (+ `lock`), are verified strictly first (`verify(.., true)`),
//! validated (okcode_analysis::validate) and re-ingested (`ingest_one`).
//!
//! Offsets are UTF-16 code units everywhere (symbols carry JS offsets), so text is composed on
//! `Vec<u16>` and converted back at the end.
use super::scan::Row;
use super::state::sha1_upper;
use super::{Workspace, MENTIONS_REASON_CAP};
use crate::error::{Error, Result};
use okcode_analysis::access::{CommitOpts, CommitResult, LockOutcome, Outcome, StatRow};
use okcode_analysis::text::convention_color::Palette;
use okcode_analysis::text::text as textu;
use okcode_analysis::validate::{self, DiagTarget, ValidateArgs};
use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Default)]
pub struct EditState {
    /// palette(): `(files.size:generation, palette)`
    palette_cache: Mutex<Option<(String, Vec<Palette>)>>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct EditBatchArgs {
    pub at: Option<String>,
    #[serde(deserialize_with = "de_edits")]
    pub edits: Vec<Value>,
}

fn de_edits<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Vec<Value>, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(match v {
        Value::Array(a) => a,
        _ => Vec::new(),
    })
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct EditOpts {
    pub at: Option<String>,
    pub from: Option<i64>,
    pub to: Option<i64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WriteAtOpts {
    pub at: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WriteWholeOpts {
    pub expected_hash: Option<String>,
    pub create: bool,
}

// ── pure helpers ────────────────────────────────────────────────────────

fn opt_str(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(String::from)
}

/// classifyCommit({ commit, candidateHash, beforeHash, readbackHash, readAvailable }) → outcome name
pub fn classify_commit(args: &Value) -> String {
    let commit = args.get("commit").cloned().unwrap_or(Value::Null);
    let response_lost = commit.get("outcome").and_then(Value::as_str) != Some("ok");
    classify(
        response_lost,
        opt_str(&commit, "hash").as_deref(),
        opt_str(args, "candidateHash").as_deref(),
        opt_str(args, "beforeHash").as_deref(),
        opt_str(args, "readbackHash").as_deref(),
        args.get("readAvailable").map_or(false, |v| !matches!(v, Value::Null | Value::Bool(false))),
    )
}

fn classify(
    response_lost: bool,
    commit_hash: Option<&str>,
    candidate: Option<&str>,
    before: Option<&str>,
    readback: Option<&str>,
    read_available: bool,
) -> String {
    let s = |x: &str| x.to_string();
    if !read_available {
        return s(if response_lost { "commit_outcome_unknown" } else { "committed_unverified" });
    }
    if readback == candidate {
        return s(if !response_lost && commit_hash == candidate { "ok" } else { "committed_verified_with_warning" });
    }
    if readback == before {
        return s("not_committed");
    }
    s(if response_lost { "commit_outcome_unknown" } else { "diverged_after_commit" })
}

fn is_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// `^(.*?)[SEPS](\d+)(?:-(\d+))?$` (JS regex semantics: `.` stops at line terminators).
fn split_range(raw: &str, seps: &[char]) -> Option<(String, i64, Option<i64>)> {
    for (i, c) in raw.char_indices() {
        if is_line_terminator(c) {
            return None;
        }
        if !seps.contains(&c) {
            continue;
        }
        let tail = &raw[i + c.len_utf8()..];
        let (a, b) = match tail.split_once('-') {
            Some((a, b)) => (a, Some(b)),
            None => (tail, None),
        };
        let digits = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit());
        if digits(a) && b.map_or(true, digits) {
            let n = |x: &str| x.parse::<i64>().unwrap_or(i64::MAX);
            return Some((raw[..i].to_string(), n(a), b.map(n)));
        }
    }
    None
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// parseRange(query), kept local so edit does not wait on the lookup slice.
fn parse_range(raw: &str) -> Option<(String, i64, i64)> {
    let (head, from, to) = split_range(raw, &[':', '#'])?;
    if utf16_len(&head) <= 1 {
        return None;
    }
    Some((head, from, to.unwrap_or(from)))
}

/// RANGE_TAIL
fn is_range_tail(tail: &str) -> bool {
    let (a, b) = match tail.split_once('-') {
        Some((a, b)) => (a, Some(b)),
        None => (tail, None),
    };
    let digits = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit());
    digits(a) && b.map_or(true, digits)
}

/// JS `String(v)` for the few shapes a target / body / line number arrives in.
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// `String(x || '')`
fn js_str_or_empty(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) | Some(Value::Bool(false)) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) if n.as_f64() == Some(0.0) => String::new(),
        Some(o) => o.to_string(),
    }
}

/// `${e.target}` — a missing target prints as undefined.
fn todo_target_display(e: &Value) -> String {
    match e.get("target") {
        None => "undefined".into(),
        Some(v) => js_string(v),
    }
}

fn body_edit(target: String, body: Option<Value>) -> Value {
    let mut m = serde_json::Map::new();
    m.insert("target".into(), json!(target));
    if let Some(b) = body {
        m.insert("body".into(), b);
    }
    Value::Object(m)
}

fn before_json(t: &Target) -> Value {
    let mut m = serde_json::Map::new();
    m.insert("start".into(), json!(t.start));
    m.insert("end".into(), json!(t.end));
    if let Some(v) = t.line_start { m.insert("lineStart".into(), json!(v)); }
    if let Some(v) = t.line_end { m.insert("lineEnd".into(), json!(v)); }
    Value::Object(m)
}

fn sym_str(s: &Value, k: &str) -> Option<String> {
    s.get(k).and_then(Value::as_str).filter(|x| !x.is_empty()).map(String::from)
}

/// `s.path || s.name`
fn sym_path(s: &Value) -> String {
    sym_str(s, "path").or_else(|| sym_str(s, "name")).unwrap_or_default()
}

/// pathEndsWith(sym, hint)
fn path_ends_with(s: &Value, hint: &str) -> bool {
    let p = sym_path(s);
    hint.contains('.') && utf16_len(&p) > utf16_len(hint) && p.ends_with(&format!(".{hint}"))
}

/// cappedList(items, { cap }) with no moreHint → (shown, more)
fn capped<'a, T>(items: &'a [T]) -> (&'a [T], String) {
    let cap = MENTIONS_REASON_CAP;
    let shown = &items[..items.len().min(cap)];
    let rest = items.len() - shown.len();
    let more = if rest > 0 { format!(" (showing {} of {})", shown.len(), items.len()) } else { String::new() };
    (shown, more)
}

/// An immutable view of one file, taken after strict verification.
struct Snapshot {
    rel: String,
    hash: Option<String>,
    content: Arc<str>,
    symbols: Vec<Value>,
}

/// A resolved edit target; `Err` is the refusal result itself.
struct Target {
    start: usize,
    end: usize,
    line_start: Option<i64>,
    line_end: Option<i64>,
    name: Option<String>,
    #[allow(dead_code)]
    kind: Value,
}

/// A bare or path-qualified symbol name, resolved WITHIN one file's snapshot only. Ambiguity is
/// refused, never guessed.
fn resolve_symbol_in_snapshot(snap: &Snapshot, hint: &str) -> std::result::Result<Value, Value> {
    let syms = &snap.symbols;
    let by_path: Vec<&Value> = syms.iter().filter(|s| sym_path(s) == hint).collect();
    let by_suffix: Vec<&Value> =
        if by_path.is_empty() { syms.iter().filter(|s| path_ends_with(s, hint)).collect() } else { by_path };
    let hits: Vec<&Value> = if by_suffix.is_empty() {
        syms.iter().filter(|s| sym_str(s, "name").as_deref() == Some(hint)).collect()
    } else {
        by_suffix
    };
    if hits.is_empty() {
        return Err(json!({"ok": false, "reason": format!("no symbol named \"{hint}\" in {}", snap.rel)}));
    }
    if hits.len() > 1 {
        let (shown, more) = capped(&hits);
        return Err(json!({
            "ok": false,
            "ambiguous": true,
            "reason": format!("\"{hint}\" is defined in {} places in {}{more} \u{2014} qualify it as <parent>.<name>", hits.len(), snap.rel),
            "candidates": shown.iter().map(|s| sym_path(s)).collect::<Vec<_>>(),
        }));
    }
    Ok(hits[0].clone())
}

/// A target ("name", "parent.name", "file#name", or "path:from-to"), resolved to OFFSETS in the
/// snapshot's own content.
fn resolve_target_in_snapshot(snap: &Snapshot, target: &str) -> std::result::Result<Target, Value> {
    if let Some((_, from, to)) = parse_range(target) {
        let total = snap.content.split('\n').count() as i64;
        // The START must be inside the file — refused, not clamped; the END clamps down.
        if from < 1 || from > total {
            return Err(json!({
                "ok": false,
                "reason": format!("{} has {total} lines \u{2014} {from}-{to} is outside it", snap.rel),
            }));
        }
        let end = to.min(total);
        let u: Vec<u16> = snap.content.encode_utf16().collect();
        let (s, e) = textu::line_range_to_offsets(&u, from, end);
        return Ok(Target { start: s, end: e, line_start: Some(from), line_end: Some(end), name: None, kind: json!("range") });
    }
    let hint = if target.contains('#') { target.split('#').nth(1).unwrap_or("") } else { target };
    let s = resolve_symbol_in_snapshot(snap, hint)?;
    let int = |k: &str| s.get(k).and_then(Value::as_f64).filter(|f| f.fract() == 0.0 && *f >= 0.0);
    let (Some(st), Some(en)) = (int("start"), int("end")) else {
        return Err(json!({
            "ok": false,
            "reason": format!("\"{hint}\" has no offset-addressable region \u{2014} read it and edit it as a range"),
        }));
    };
    Ok(Target {
        start: st as usize,
        end: en as usize,
        line_start: s.get("lineStart").and_then(Value::as_i64),
        line_end: s.get("lineEnd").and_then(Value::as_i64),
        name: Some(sym_path(&s)).filter(|n| !n.is_empty()),
        kind: s.get("kind").cloned().unwrap_or(Value::Null),
    })
}

/// commitGuarded's result: a lock refusal, or the facade's report (or a lost response).
struct Committed {
    locked: Option<String>,
    res: CommitResult,
}

fn re_styled() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)\.(html|css|ok\.js|ok\.mjs)$").unwrap())
}
fn re_vendor() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(^|/)(node_modules|vendor|dist|build)/").unwrap())
}

fn ext_of_rel(rel: &str) -> String {
    let base = rel.rsplit('/').next().unwrap_or(rel);
    match base.rfind('.') {
        Some(i) if i > 0 => base[i..].to_ascii_lowercase(),
        _ => String::new(),
    }
}

fn dirname(p: &str) -> String {
    match p.rfind('/') {
        None => ".".into(),
        Some(0) => "/".into(),
        Some(i) => p[..i].to_string(),
    }
}

impl Workspace {
    /// readOnlyRefusal()
    pub fn read_only_refusal(&self) -> Value {
        let why = if self.unindexed { "read-only (not indexed)" } else { "read-only (its access facade has no commit)" };
        json!({
            "ok": false,
            "readOnly": true,
            "code": "OKCODE_READ_ONLY",
            "reason": format!("workspace \"{}\" is {why} \u{2014} nothing in it can be edited", self.id),
        })
    }

    /// Which FILE a target names — ONLY to learn which file. No offset this returns is trusted.
    fn file_for_target(&self, target: &str) -> Result<Option<String>> {
        if let Some((file, _, _)) = parse_range(target) {
            return Ok(self.file_of(&file));
        }
        let r = self.resolve(target)?;
        if r.get("ok").and_then(Value::as_bool) == Some(true) || r.get("isFile").and_then(Value::as_bool) == Some(true) {
            return Ok(match r.get("file") {
                Some(Value::String(s)) => Some(s.clone()),
                Some(f) => opt_str(f, "rel").or_else(|| opt_str(f, "path")),
                None => None,
            });
        }
        Ok(None)
    }

    /// WHY a target did not resolve, in the caller's own terms — never blaming the file when the
    /// file resolves perfectly and only the suffix is wrong.
    fn explain_target_edit(&self, raw: &str) -> Result<String> {
        if let Some(cut) = raw.rfind(|c| c == ':' || c == '#') {
            let head = &raw[..cut];
            let tail = &raw[cut + 1..];
            if utf16_len(head) > 1 {
                if let Some(f) = self.file_of(head) {
                    return Ok(if is_range_tail(tail) {
                        format!("{f} IS indexed \u{2014} \"{raw}\" failed on the separator, not the file; address a line range as \"{f}:{tail}\"")
                    } else {
                        format!("{f} IS indexed, but has no symbol \"{tail}\" \u{2014} address a line range as \"{f}:<from>-<to>\", or read {f} to find the symbol's real name")
                    });
                }
            }
        }
        let r = self.resolve(raw)?;
        let reason = r.get("reason").and_then(Value::as_str).filter(|s| !s.is_empty());
        // A noSuchFile reason is already a complete sentence about the file.
        if r.get("noSuchFile").and_then(Value::as_bool) == Some(true) {
            if let Some(re) = reason {
                return Ok(re.to_string());
            }
        }
        Ok(match reason {
            Some(re) => format!("no indexed file matching \"{raw}\" \u{2014} {re}"),
            None => format!("no indexed file matching \"{raw}\""),
        })
    }

    /// palette(): THE PROJECT'S OWN COLOURS, read from the project (style-bearing files only),
    /// cached against the scan generation.
    pub fn palette(&self) -> Result<Vec<Palette>> {
        let stamp = {
            let ix = self.read_ix();
            format!("{}:{}", ix.files.len(), ix.generation)
        };
        if let Some((s, list)) = self.edit.palette_cache.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            if *s == stamp {
                return Ok(list.clone());
            }
        }
        let styled: Vec<String> = {
            let ix = self.read_ix();
            ix.files
                .values()
                .filter(|f| f.indexed && !okcode_analysis::is_secret(&f.rel) && re_styled().is_match(&f.rel) && !re_vendor().is_match(&f.rel))
                .map(|f| f.rel.clone())
                .collect()
        };
        // Text is only cached for files something has touched; an empty palette makes the rule
        // inert, so read what is missing.
        self.ensure_texts(&styled)?;
        let sources: Vec<Option<String>> = {
            let ix = self.read_ix();
            styled
                .iter()
                .map(|rel| ix.files.get(rel).and_then(|f| f.content.as_ref()).filter(|c| c.contains("--")).map(|c| c.to_string()))
                .collect()
        };
        let out = okcode_analysis::text::convention_color::palette_from(&sources);
        let stamp = {
            let ix = self.read_ix();
            format!("{}:{}", ix.files.len(), ix.generation)
        };
        *self.edit.palette_cache.lock().unwrap_or_else(|e| e.into_inner()) = Some((stamp, out.clone()));
        Ok(out)
    }

    /// resolveModuleKind(rel) → 'module' | 'commonjs' | 'ambiguous'. `.mjs`/`.cjs` decide
    /// outright; a bare `.js` is decided by the NEAREST ancestor package.json's "type", strictly
    /// re-verified. Only manifests inside the workspace are consulted.
    pub fn resolve_module_kind(&self, rel: &str) -> Result<String> {
        let ext = ext_of_rel(rel);
        if ext == ".mjs" {
            return Ok("module".into());
        }
        if ext == ".cjs" {
            return Ok("commonjs".into());
        }
        let mut candidates: Vec<String> = Vec::new();
        let mut dir = dirname(rel);
        for _ in 0..64 {
            candidates.push(if dir == "." || dir.is_empty() { "package.json".into() } else { format!("{dir}/package.json") });
            if dir == "." || dir.is_empty() || dir == "/" {
                break;
            }
            dir = dirname(&dir);
        }
        let Ok(found) = self.access.stat(&candidates, false) else { return Ok("ambiguous".into()) };
        let present = |c: &String| found.iter().any(|(p, r)| p == c && !matches!(r, StatRow::Missing));
        let Some(pkg_path) = candidates.iter().find(|c| present(c)).cloned() else { return Ok("ambiguous".into()) };
        if self.verify(&[pkg_path.clone()], true).is_err() {
            return Ok("ambiguous".into());
        }
        let src = self.read_ix().files.get(&pkg_path).and_then(|r| r.content.clone());
        if let Some(src) = src {
            if let Ok(pkg) = serde_json::from_str::<Value>(&src) {
                match pkg.get("type").and_then(Value::as_str) {
                    Some("module") => return Ok("module".into()),
                    Some("commonjs") => return Ok("commonjs".into()),
                    _ => {}
                }
            }
        }
        Ok("ambiguous".into()) // the nearest manifest controls, and declares no explicit type
    }

    /// The commit point shared by editBatch and writeWholeFile: optional lock, then the facade's
    /// atomic compare-and-swap (or exclusive create). A commit that fails is a lost response, not
    /// a refusal — the readback decides what happened.
    fn commit_guarded(&self, rel: &str, bytes: &[u8], opts: &CommitOpts, operation_id: &str) -> Committed {
        let mut guard = None;
        if self.caps.lock {
            match self.access.lock(&format!("{rel}.okcode-lock"), operation_id) {
                Ok(LockOutcome::Held(g)) => guard = Some(g),
                Ok(LockOutcome::Failed(reason)) => {
                    let reason = if reason.is_empty() { "locked".to_string() } else { reason };
                    return Committed { locked: Some(reason), res: CommitResult::of(Outcome::ResponseLost) };
                }
                Err(e) => return Committed { locked: Some(if e.is_empty() { "locked".to_string() } else { e }), res: CommitResult::of(Outcome::ResponseLost) },
            }
        }
        let res = match self.access.commit(rel, bytes, opts) {
            Some(Ok(r)) => r,
            Some(Err(e)) => CommitResult::err(Outcome::ResponseLost, e.chars().take(500).collect::<String>()),
            None => CommitResult::err(Outcome::ResponseLost, "the access facade has no commit"),
        };
        if let Some(g) = guard {
            g.release();
        }
        Committed { locked: None, res }
    }

    /// An INDEPENDENT readback, always attempted after a commit.
    fn read_back(&self, rel: &str) -> Option<Vec<u8>> {
        let got = self.access.read(&[rel.to_string()]).ok()?;
        got.into_iter().find(|(p, _)| p == rel).map(|(_, b)| b)
    }

    /// Re-sync from the files regardless of outcome — the index must reflect reality. An OWN
    /// WRITE: the bytes just published are already in hand, so the row is updated directly from
    /// them; one stat gets the authoritative size/mtime. Without a readback the row is dropped so
    /// the next verify() re-establishes it.
    fn ingest_own_write(&self, rel: &str, readback: Option<&[u8]>) {
        match readback {
            Some(buf) => {
                let stat = self.access.stat(&[rel.to_string()], false).ok().and_then(|rows| rows.into_iter().find(|(p, _)| p == rel));
                let (size, mtime) = match stat {
                    Some((_, StatRow::Found { size, mtime, .. })) => (size, Some(mtime)),
                    _ => (buf.len() as u64, None),
                };
                self.ingest_one(&Row { path: rel.to_string(), size, mtime, hash: None }, Some(buf));
            }
            None => {
                self.drop_file(rel);
            }
        }
        self.schedule_flush();
    }

    // ── api ─────────────────────────────────────────────────────────────

    pub fn edit_batch(&self, a: EditBatchArgs) -> Result<Value> {
        if self.read_only {
            return Ok(self.read_only_refusal());
        }
        let todo: Vec<&Value> = a
            .edits
            .iter()
            .filter(|e| match e {
                Value::Null | Value::Bool(false) => false,
                Value::String(s) => !s.is_empty(),
                Value::Number(n) => n.as_f64().map(|f| f != 0.0 && !f.is_nan()).unwrap_or(true),
                _ => true,
            })
            .collect();
        if todo.is_empty() {
            return Ok(json!({"ok": false, "reason": "editBatch needs at least one edit"}));
        }
        let at = match a.at.as_deref() {
            Some(t) if !t.is_empty() => t.to_string(),
            _ => {
                return Ok(json!({
                    "ok": false,
                    "reason": "at is required \u{2014} read the file or symbol first and pass back its token; find/ask locate, they do not authorize an edit",
                }))
            }
        };
        self.ensure_open()?;
        let target_of = |e: &Value| js_str_or_empty(e.get("target"));
        let first_target = target_of(todo[0]);
        let Some(first) = self.file_for_target(&first_target)? else {
            return Ok(json!({"ok": false, "reason": self.explain_target_edit(&first_target)?}));
        };

        // Strict verification BEFORE anything about this file is trusted.
        if let Err(err) = self.verify(&[first.clone()], true) {
            return Ok(json!({
                "ok": false,
                "reason": format!("could not verify {first} \u{2014} refusing rather than trusting a cached offset: {}", err.message),
            }));
        }
        let take = |ws: &Workspace| {
            ws.read_ix().files.get(&first).map(|r| (r.hash.clone(), r.content.clone(), r.symbols.clone(), r.indexed, r.lossless))
        };
        let Some(mut got) = take(self) else {
            return Ok(json!({"ok": false, "reason": format!("{first} no longer exists")}));
        };
        if got.0.as_deref() != Some(at.as_str()) {
            return Ok(json!({
                "ok": false,
                "stale": true,
                "reason": "the file changed since that token was issued",
                "at": got.0,
            }));
        }
        // Rust-only safeguard (JS returns "is not indexed as text" here; JS verify always rehydrates).
        if got.1.is_none() && got.3 {
            // an indexed file whose text was evicted: bring it back before judging it
            self.ensure_texts(&[first.clone()])?;
            if let Some(g) = take(self) {
                got = g;
            }
            if got.0.as_deref() != Some(at.as_str()) {
                return Ok(json!({
                    "ok": false,
                    "stale": true,
                    "reason": "the file changed since that token was issued",
                    "at": got.0,
                }));
            }
        }
        let (hash, Some(content), symbols, _, lossless) = got else {
            return Ok(json!({"ok": false, "reason": format!("{first} is not indexed as text \u{2014} it cannot be edited here")}));
        };
        if !lossless {
            return Ok(json!({
                "ok": false,
                "reason": format!("{first} could not be decoded losslessly as UTF-8 \u{2014} refusing to compose an edit on top of an uncertain decode"),
            }));
        }
        let snap = Snapshot { rel: first.clone(), hash, content, symbols };

        // Every target resolved against the SAME snapshot; the whole batch refused on the first
        // target that names a different file, is ambiguous, or does not resolve at all.
        struct Resolved {
            target: String,
            t: Target,
            body: String,
            body16: Vec<u16>,
            cand_start: usize,
            cand_end: usize,
        }
        let mut resolved: Vec<Resolved> = Vec::new();
        for e in &todo {
            let tgt = target_of(e);
            let f = self.file_for_target(&tgt)?;
            if f.as_deref() != Some(snap.rel.as_str()) {
                return Ok(json!({
                    "ok": false,
                    "crossFile": true,
                    "reason": format!(
                        "all edits in one batch must target the same file \u{2014} \"{}\" resolves to {}, not {}",
                        todo_target_display(e),
                        f.as_deref().unwrap_or("an unknown file"),
                        snap.rel
                    ),
                }));
            }
            let t = match resolve_target_in_snapshot(&snap, &tgt) {
                Ok(t) => t,
                Err(refusal) => return Ok(refusal),
            };
            let body = match e.get("body") {
                None => "undefined".to_string(),
                Some(b) => js_string(b),
            };
            resolved.push(Resolved { target: tgt, t, body, body16: Vec::new(), cand_start: 0, cand_end: 0 });
        }

        let mut by_start: Vec<usize> = (0..resolved.len()).collect();
        by_start.sort_by_key(|&i| resolved[i].t.start);
        for w in by_start.windows(2) {
            let (p, c) = (&resolved[w[0]], &resolved[w[1]]);
            if c.t.start < p.t.end {
                return Ok(json!({
                    "ok": false,
                    "overlap": true,
                    "reason": format!("edit targets overlap: \"{}\" and \"{}\"", p.target, c.target),
                }));
            }
        }

        // Compose against the snapshot, highest offset downward. Inserted text is normalised to
        // the file's own dominant line ending; everything outside an edit is sliced verbatim.
        let eol = textu::dominant_eol(&snap.content);
        let base16: Vec<u16> = snap.content.encode_utf16().collect();
        let mut cand16 = base16.clone();
        for &i in by_start.iter().rev() {
            let r = &mut resolved[i];
            r.body16 = textu::normalise_eol(&r.body, eol).encode_utf16().collect();
            let s = r.t.start.min(cand16.len());
            let e = r.t.end.min(cand16.len()).max(s);
            cand16.splice(s..e, r.body16.iter().copied());
        }
        let candidate = String::from_utf16_lossy(&cand16);
        // Each edit's span in the CANDIDATE.
        let mut shift: i64 = 0;
        for &i in &by_start {
            let r = &mut resolved[i];
            r.cand_start = (r.t.start as i64 + shift) as usize;
            r.cand_end = r.cand_start + r.body16.len();
            shift += r.body16.len() as i64 - (r.t.end as i64 - r.t.start as i64);
        }
        let spans: Vec<(i64, i64, i64)> =
            by_start.iter().map(|&i| (resolved[i].t.start as i64, resolved[i].cand_end as i64 - resolved[i].cand_start as i64, resolved[i].t.end as i64 - resolved[i].t.start as i64)).collect();
        let map_offset = move |off: i64| -> Option<i64> {
            let mut sh = 0;
            for &(start, new_len, old_len) in &spans {
                if start > off {
                    break;
                }
                sh += new_len - old_len;
            }
            Some(off + sh)
        };
        let diag_targets: Vec<DiagTarget> = resolved
            .iter()
            .map(|e| DiagTarget {
                name: e.t.name.clone().unwrap_or_else(|| e.target.clone()),
                baseline_start: e.t.start as i64,
                baseline_end: e.t.end as i64,
                candidate_start: e.cand_start as i64,
                candidate_end: e.cand_end as i64,
            })
            .collect();

        // Validate the CANDIDATE — text, never a path — before the target is touched. Commit iff
        // no NEW diagnostic, compared structurally against the baseline, never by count.
        let module_kind = self.resolve_module_kind(&snap.rel)?;
        let palette = self.palette()?;
        let gate = |text: &str| -> Result<Value> {
            validate::validate_source(&ValidateArgs {
                rel: snap.rel.clone(),
                text: text.to_string(),
                module_kind: module_kind.clone(),
                palette: palette.clone(),
            })
            .map_err(Error::msg)
        };
        let baseline_v = gate(&snap.content)?;
        let candidate_v = gate(&candidate)?;
        let diags = |v: &Value| v.get("diagnostics").and_then(Value::as_array).cloned().unwrap_or_default();
        let cmp = validate::compare_diagnostics(&diags(&baseline_v), &diags(&candidate_v), Some(&map_offset), &diag_targets);
        let new_diags = cmp["newDiagnostics"].as_array().cloned().unwrap_or_default();

        // The receipt reports convention-color's status from the NEW diagnostics (scoped to the
        // edited regions), so it only ever judges the lines this edit added.
        let new_color = new_diags.iter().any(|d| d.get("source").and_then(Value::as_str) == Some("convention-color"));
        let reported: Vec<Value> = candidate_v["validators"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|mut v| {
                if v.get("validator").and_then(Value::as_str) == Some("convention-color") {
                    v["status"] = json!(if new_color { "failed" } else { "passed" });
                }
                v
            })
            .collect();

        if !new_diags.is_empty() {
            let msgs: Vec<String> = new_diags.iter().take(3).map(|d| js_string(d.get("message").unwrap_or(&Value::Null))).collect();
            return Ok(json!({
                "ok": false,
                "reason": format!("candidate introduces {} new diagnostic(s): {}", new_diags.len(), msgs.join("; ")),
                "newDiagnostics": new_diags,
                "validation": reported,
                "moduleKind": module_kind,
            }));
        }

        // ── the commit point ────────────────────────────────────────────
        let candidate_buf = candidate.as_bytes();
        let candidate_hash = sha1_upper(candidate_buf);
        let operation_id = new_operation_id();
        let committed = self.commit_guarded(&snap.rel, candidate_buf, &CommitOpts { expected_hash: snap.hash.clone(), create: false }, &operation_id);
        if let Some(reason) = committed.locked {
            return Ok(json!({
                "ok": false,
                "locked": true,
                "reason": format!("could not acquire the write lock for {} ({reason})", snap.rel),
            }));
        }
        let commit = committed.res;
        match commit.outcome {
            Outcome::Stale => {
                return Ok(json!({
                    "ok": false,
                    "stale": true,
                    "reason": "the file changed since that token was issued",
                    "at": commit.current.clone().or(commit.hash.clone()),
                }))
            }
            Outcome::NoAtomic => {
                let why = commit.error.as_ref().map(|e| format!(" ({e})")).unwrap_or_default();
                return Ok(json!({
                    "ok": false,
                    "reason": format!("{} could not be published atomically{why} \u{2014} nothing was touched", snap.rel),
                }));
            }
            Outcome::Missing => return Ok(json!({"ok": false, "reason": format!("{} no longer exists", snap.rel)})),
            Outcome::Symlink => {
                return Ok(json!({
                    "ok": false,
                    "reason": format!("{} is a symlink \u{2014} refusing to replace the link itself rather than what it points to", snap.rel),
                }))
            }
            _ => {}
        }

        // Past this line the publish ran (outcome 'ok') OR its RESPONSE was lost — an independent
        // readback is ALWAYS attempted before anything is reported.
        let readback = self.read_back(&snap.rel);
        let readback_hash = readback.as_deref().map(sha1_upper);
        let ok_commit = commit.outcome == Outcome::Ok;
        let commit_reported_hash = if ok_commit { commit.hash.clone() } else { None };
        let outcome = classify(
            !ok_commit,
            commit.hash.as_deref(),
            Some(&candidate_hash),
            snap.hash.as_deref(),
            readback_hash.as_deref(),
            readback.is_some(),
        );
        self.ingest_own_write(&snap.rel, readback.as_deref());

        let edits16: Vec<okcode_analysis::text::text::Edit> = by_start
            .iter()
            .map(|&i| {
                let r = &resolved[i];
                okcode_analysis::text::text::Edit {
                    start: r.t.start as i64,
                    end: r.t.end as i64,
                    body: r.body16.clone(),
                    target: Some(r.t.name.clone().unwrap_or_else(|| r.target.clone())),
                }
            })
            .collect();
        let diff = textu_hunks(&base16, &cand16, &edits16);

        let mut receipt = Map::new();
        let mut put = |k: &str, v: Value| {
            receipt.insert(k.to_string(), v);
        };
        put("ok", json!(outcome == "ok"));
        put("outcome", json!(outcome));
        put("operationId", json!(operation_id));
        put("file", json!(snap.rel));
        put("beforeHash", json!(snap.hash));
        put("candidateHash", json!(candidate_hash));
        put("commitReportedHash", json!(commit_reported_hash));
        put("readbackHash", json!(readback_hash));
        put(
            "edits",
            Value::Array(
                resolved
                    .iter()
                    .map(|e| {
                        let mut m = serde_json::Map::new();
                        m.insert("target".into(), json!(e.target));
                        if let Some(n) = &e.t.name {
                            m.insert("name".into(), json!(n));
                        }
                        m.insert("before".into(), before_json(&e.t));
                        m.insert("after".into(), json!({"start": e.cand_start, "end": e.cand_end}));
                        Value::Object(m)
                    })
                    .collect(),
            ),
        );
        put("diff", Value::Array(diff));
        put("validation", json!(reported));
        put("coverage", candidate_v.get("coverage").cloned().unwrap_or(Value::Null));
        put("moduleKind", json!(module_kind));
        put("baselineBroken", cmp["baselineBroken"].clone());
        put("resolved", cmp["resolved"].clone());
        put("remaining", cmp["remaining"].clone());
        put("newDiagnostics", json!([]));
        if let Some(e) = &commit.error {
            put("commitError", json!(e));
        }
        if outcome != "ok" {
            put("reason", json!(format!("commit outcome: {outcome} \u{2014} read {} again before trying anything further", snap.rel)));
        }
        Ok(Value::Object(receipt))
    }

    /// A one-edit batch. `read` takes a symbol, a file, or `path:from-to`; so does this.
    pub fn edit(&self, query: &Value, new_body: Option<Value>, o: EditOpts) -> Result<Value> {
        let q = if matches!(query, Value::Null | Value::Bool(false)) { String::new() } else { js_string(query) };
        let truthy = |n: Option<i64>| n.filter(|&n| n != 0);
        let mut target = q.clone();
        match split_range(&q, &[':']) {
            Some((head, f, t)) if utf16_len(&head) > 1 => {
                let from = truthy(o.from).unwrap_or(f);
                let to = truthy(o.to).unwrap_or(t.unwrap_or(f));
                target = format!("{head}:{from}-{to}");
            }
            _ => {
                if let Some(from) = truthy(o.from) {
                    let qs = if query.is_null() { "null".to_string() } else { js_string(query) };
                    target = format!("{qs}:{from}-{}", truthy(o.to).unwrap_or(from));
                }
            }
        }
        self.edit_batch(EditBatchArgs { at: o.at, edits: vec![body_edit(target, new_body)] })
    }

    pub fn write_at(&self, file: &str, line_start: Value, line_end: Value, new_body: Option<Value>, o: WriteAtOpts) -> Result<Value> {
        let undef = |v: &Value| if v.is_null() { "undefined".to_string() } else { js_string(v) };
        let target = format!("{file}:{}-{}", undef(&line_start), undef(&line_end));
        self.edit_batch(EditBatchArgs { at: o.at, edits: vec![body_edit(target, new_body)] })
    }

    /// Whole-file writes. `create: true` publishes a brand-new file through the facade's atomic
    /// exclusive create; replacing an EXISTING file is editBatch with one target spanning the file.
    pub fn write_whole_file(&self, rel_path: &str, text: &str, o: WriteWholeOpts) -> Result<Value> {
        if self.read_only {
            return Ok(self.read_only_refusal());
        }
        self.ensure_open()?;
        if !o.create {
            let Some(expected) = o.expected_hash.filter(|h| !h.is_empty()) else {
                return Ok(json!({
                    "ok": false,
                    "reason": "replacing an existing file requires expectedHash \u{2014} read it first and pass back its token",
                }));
            };
            let Some(f) = self.file_of(rel_path) else {
                return Ok(json!({"ok": false, "reason": format!("{rel_path} does not exist \u{2014} pass create: true to create it")}));
            };
            return self.edit_batch(EditBatchArgs {
                at: Some(expected),
                edits: vec![json!({"target": format!("{f}:1-{}", 9007199254740991i64), "body": text})],
            });
        }

        let norm = rel_path.replace('\\', "/");
        let rel = norm.strip_prefix("./").unwrap_or(&norm).to_string();
        let drive = {
            let b = rel.as_bytes();
            b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
        };
        if rel.is_empty() || rel.starts_with('/') || drive || rel.split('/').any(|s| s == "..") {
            return Ok(json!({"ok": false, "reason": format!("\"{rel_path}\" is not a workspace-relative path")}));
        }
        let module_kind = self.resolve_module_kind(&rel)?;
        // A new file has no baseline, so ANY diagnostic refuses it.
        let v = validate::validate_source(&ValidateArgs {
            rel: rel.clone(),
            text: text.to_string(),
            module_kind: module_kind.clone(),
            palette: Vec::new(),
        })
        .map_err(Error::msg)?;
        let vdiags = v.get("diagnostics").and_then(Value::as_array).cloned().unwrap_or_default();
        if !vdiags.is_empty() {
            let msgs: Vec<String> = vdiags.iter().take(3).map(|d| js_string(d.get("message").unwrap_or(&Value::Null))).collect();
            return Ok(json!({
                "ok": false,
                "reason": format!("candidate does not validate: {}", msgs.join("; ")),
                "newDiagnostics": vdiags,
                "validation": v["validators"],
                "moduleKind": module_kind,
            }));
        }

        let buf = text.as_bytes();
        let candidate_hash = sha1_upper(buf);
        let operation_id = new_operation_id();
        let committed = self.commit_guarded(&rel, buf, &CommitOpts { expected_hash: None, create: true }, &operation_id);
        if let Some(reason) = committed.locked {
            return Ok(json!({
                "ok": false,
                "locked": true,
                "reason": format!("could not acquire the write lock for {rel} ({reason})"),
            }));
        }
        let commit = committed.res;
        match commit.outcome {
            Outcome::Exists => {
                return Ok(json!({
                    "ok": false,
                    "reason": format!("{rel} already exists \u{2014} use edit, or writeWholeFile with expectedHash, to replace it"),
                }))
            }
            Outcome::NoAtomic => {
                return Ok(json!({"ok": false, "reason": format!("{rel} could not be published atomically \u{2014} nothing was touched")}))
            }
            Outcome::Symlink => return Ok(json!({"ok": false, "reason": format!("{rel} is a symlink \u{2014} refusing")})),
            _ => {}
        }

        let readback = self.read_back(&rel);
        let readback_hash = readback.as_deref().map(sha1_upper);
        let ok_commit = commit.outcome == Outcome::Ok;
        let outcome = classify(!ok_commit, commit.hash.as_deref(), Some(&candidate_hash), None, readback_hash.as_deref(), readback.is_some());
        self.ingest_own_write(&rel, readback.as_deref());
        let mut out = json!({
            "ok": outcome == "ok",
            "outcome": outcome,
            "operationId": operation_id,
            "file": rel,
            "beforeHash": null,
            "candidateHash": candidate_hash,
            "commitReportedHash": if ok_commit { json!(commit.hash) } else { Value::Null },
            "readbackHash": readback_hash,
            "validation": v["validators"],
            "coverage": v["coverage"],
            "moduleKind": module_kind,
        });
        if outcome != "ok" {
            out["reason"] = json!(format!("commit outcome: {outcome} \u{2014} read {rel} again before trying anything further"));
        }
        Ok(out)
    }

    /// Language-appropriate proof ON THE TARGET that what landed is still valid. Opt-in: needs
    /// the facade's `exec` capability.
    pub fn syntax_check(&self, file: &str) -> Result<Value> {
        let rel = self.file_of(file).unwrap_or_else(|| file.replace('\\', "/"));
        if okcode_analysis::lang_of(&rel) != "javascript" {
            return Ok(json!({"ok": true, "out": "(no checker for this language)"}));
        }
        if !self.caps.exec {
            return Ok(json!({"ok": false, "unsupported": true, "out": "this workspace grants no exec capability"}));
        }
        let cmd = format!("node --check '{}'", rel.replace('\'', "'\\''"));
        let r = self.access.exec(&cmd).map_err(Error::msg)?;
        Ok(json!({"ok": r.ok, "out": r.out}))
    }
}

fn textu_hunks(before: &[u16], after: &[u16], edits: &[okcode_analysis::text::text::Edit]) -> Vec<Value> {
    // A diff-rendering failure must not hide the outcome: the commit already happened.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| okcode_analysis::text::text::hunks_for(before, after, edits, 3)))
        .unwrap_or_default()
}

/// 8 random bytes as hex (crypto.randomBytes(8)).
fn new_operation_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::new();
    for i in 0..2u32 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u32(i);
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        out.push_str(&format!("{:08x}", h.finish() as u32));
    }
    out
}
