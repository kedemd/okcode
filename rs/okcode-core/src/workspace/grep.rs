//! grep and glob. OWNER: slice `grep`.
//!
//! JS (src/workspace.js): 1117-1209 indexReady/warmUp/grepUniverse/narrow/grepTexts,
//! 1660-1928 grepAll, api grep (3208), glob (3225). Matching itself is okcode-grep
//! (compile_matcher, match_text, tally_text, path_matcher, index_terms); content candidates come
//! from the store (`Store::content_candidates`) and the facade (`files_containing`).
//! A library path / unindexed workspace is listed and read directly (slice `library` helpers).
use super::Workspace;
use super::library::LibPath;
use crate::error::{Error, Result};
use okcode_analysis::grep::{compile_matcher, match_text, path_matcher, CaseMode, MatchOpts, Matcher, Tally};
use okcode_analysis::{is_secret, is_textual, lang_of};
use serde_json::{json, Map, Value};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub struct GrepState {}

/// JS string order (UTF-16 code units).
fn cmp_js(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

/// A file's text as grep sees it: the text and the hash it was read at.
struct Got {
    text: String,
    at: Option<String>,
}

/// Number(v) for the option values grep reads.
fn js_number(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                0.0
            } else {
                let low = t.to_ascii_lowercase();
                let radix = |p: &str, r: u32| low.strip_prefix(p).map(|d| u64::from_str_radix(d, r).map_or(f64::NAN, |n| n as f64));
                if let Some(n) = radix("0x", 16).or_else(|| radix("0o", 8)).or_else(|| radix("0b", 2)) {
                    n
                } else if low.contains("inf") || low.contains("nan") {
                    if t == "Infinity" || t == "+Infinity" { f64::INFINITY } else if t == "-Infinity" { f64::NEG_INFINITY } else { f64::NAN }
                } else {
                    t.parse::<f64>().unwrap_or(f64::NAN)
                }
            }
        }
        Value::Bool(b) => *b as u8 as f64,
        Value::Null => 0.0,
        _ => f64::NAN,
    }
}

fn int(v: Option<&Value>, dflt: usize, lo: usize, hi: usize) -> usize {
    let Some(v) = v else { return dflt };
    if v.is_null() || v.as_str() == Some("") {
        return dflt;
    }
    let n = js_number(v);
    if !n.is_finite() {
        return dflt;
    }
    n.floor().max(lo as f64).min(hi as f64) as usize
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64().map_or(true, |f| f != 0.0 && !f.is_nan()),
        Some(_) => true,
    }
}

fn js_string(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Object(o) if o.contains_key("$re") => {
            let src = o.get("$re").and_then(Value::as_str).unwrap_or("");
            let flags = o.get("flags").and_then(Value::as_str).unwrap_or("");
            format!("/{src}/{flags}")
        }
        Value::Object(_) => "[object Object]".into(),
        Value::Array(a) => a.iter().map(|x| if x.is_null() { String::new() } else { js_string(x) }).collect::<Vec<_>>().join(","),
        other => other.to_string(),
    }
}

/// `[].concat(v == null ? [] : v)`
fn as_list(v: Option<&Value>) -> Vec<Value> {
    match v {
        None | Some(Value::Null) => vec![],
        Some(Value::Array(a)) => a.clone(),
        Some(o) => vec![o.clone()],
    }
}

fn only_strings(v: Option<&Value>) -> Vec<String> {
    as_list(v).into_iter().filter_map(|x| x.as_str().map(String::from)).collect()
}

/// String(p || '') for a `paths` entry
fn path_string(v: &Value) -> String {
    if truthy(Some(v)) {
        js_string(v)
    } else {
        String::new()
    }
}

fn row_str(r: &Value, k: &str) -> String {
    r.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

fn lib_rows(listed: &Value) -> Vec<(String, Value)> {
    listed
        .get("rows")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|r| (row_str(r, "rel"), r.get("size").cloned().unwrap_or(Value::Null))).collect())
        .unwrap_or_default()
}

fn str_vec(v: Option<&Value>) -> Option<Vec<String>> {
    v.and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
}

impl Workspace {
    /// grepUniverse(ready) → the rels a grep may consider
    pub fn grep_universe(&self, ready: bool) -> Result<Vec<String>> {
        let mut out: Vec<String>;
        if ready {
            self.ensure_open()?;
            let ix = self.read_ix();
            out = ix.files.values().filter(|f| f.indexed && !is_secret(&f.rel)).map(|f| f.rel.clone()).collect();
        } else {
            self.warm_up();
            let rows = self.list_tree(&|_: &str| {})?;
            out = rows.into_iter().filter(|r| is_textual(&r.path, Some(r.size))).map(|r| r.path).collect();
        }
        out.sort_by(|a, b| cmp_js(a, b));
        Ok(out)
    }

    /// narrow(rels, matcher, ready) → (the superset to read, `via`). Every accelerator returns a
    /// superset of the matching files or nothing at all.
    fn narrow(&self, rels: Vec<String>, matcher: &Matcher, ready: bool) -> (Vec<String>, &'static str) {
        let fallback = if ready { "scan" } else { "direct" };
        let Some(literal) = matcher.literal.as_deref() else { return (rels, fallback) };
        if rels.is_empty() {
            return (rels, fallback);
        }
        if self.caps.files_containing {
            if let Ok(Some(hit)) = self.access.files_containing(&rels, literal, matcher.ignore_case) {
                let kept = rels.into_iter().filter(|r| hit.contains(r)).collect();
                return (kept, "target");
            }
        }
        if ready {
            if let Some(store) = &self.store {
                // Deliberately lenient: a store error falls back to a plain scan (JS would reject).
                if let Ok(Some(fts)) = store.content_candidates(literal) {
                    let moved = match self.moved_of(&rels) {
                        Ok(Value::Array(a)) => Some(a),
                        _ => None,
                    };
                    if let Some(moved) = moved {
                        let mut keep: HashSet<String> = fts.into_iter().collect();
                        keep.extend(moved.iter().filter_map(|v| v.as_str().map(String::from)));
                        {
                            let ix = self.read_ix();
                            keep.extend(ix.pending.keys().cloned());
                            keep.extend(ix.flushing.keys().cloned());
                            keep.extend(ix.store_errors.keys().cloned());
                        }
                        let kept = rels.into_iter().filter(|r| keep.contains(r)).collect();
                        return (kept, "index");
                    }
                }
            }
        }
        (rels, fallback)
    }

    /// grepTexts(rels, ready) → the text of each readable rel. Ready: verified first, then served
    /// from the records. Direct: read as-is, nothing recorded.
    fn grep_texts(&self, rels: &[String], ready: bool) -> Result<HashMap<String, Got>> {
        let mut out = HashMap::new();
        if ready {
            self.verify(rels, false)?;
            let ix = self.read_ix();
            for rel in rels {
                if let Some(f) = ix.files.get(rel) {
                    if let Some(c) = &f.content {
                        out.insert(rel.clone(), Got { text: c.to_string(), at: f.hash.clone() });
                    }
                }
            }
            return Ok(out);
        }
        let noop = |_: &str, _: &str| {};
        let got = self.read_many(rels, &|_: &str| 0, Some(&noop))?;
        for (rel, buf) in got {
            let (text, _) = okcode_analysis::text::text::decode(&buf);
            out.insert(rel, Got { text, at: Some(super::state::sha1_upper(&buf)) });
        }
        Ok(out)
    }

    /// grepAll(pattern, o) — `o` is the JS options object as given (paths, glob, regex,
    /// caseSensitive, context, limit, offset, count, …); parse it in this file.
    pub fn grep_all(&self, pattern: &Value, o: &Value) -> Result<Value> {
        let t0 = std::time::Instant::now();
        let none = Value::Null;
        let o = if o.is_object() { o } else { &none };
        let g = |k: &str| o.get(k);
        let cs = match g("caseSensitive") {
            Some(Value::String(s)) if s == "smart" => CaseMode::Smart,
            None | Some(Value::Null) => CaseMode::Sensitive(false),
            v => CaseMode::Sensitive(truthy(v)),
        };
        let matcher = compile_matcher(&js_string(pattern), truthy(g("regex")), &cs)
            .map_err(|m| Error::code("GREP_BAD_PATTERN", m))?;
        let output = match g("output").and_then(Value::as_str) {
            Some("files") => "files",
            Some("matches") => "matches",
            _ => "lines",
        };
        let context = int(g("context"), 0, 0, 50);
        let before = int(g("before"), context, 0, 50);
        let after = int(g("after"), context, 0, 50);
        let max_matches = int(g("maxMatches"), 200, 1, 100000);
        let max_per_file = int(g("maxPerFile"), 50, 1, 100000);
        let max_files_opt = if g("maxFiles").map_or(true, |v| v.is_null() || v.as_str() == Some("")) || !js_number(g("maxFiles").unwrap()).is_finite() {
            None
        } else {
            Some(int(g("maxFiles"), 1, 1, usize::MAX))
        };
        let max_files = max_files_opt.unwrap_or(usize::MAX);
        let offset = int(g("offset"), 0, 0, 9007199254740991);
        let page = int(g("page"), 1, 1, 9007199254740991);
        let ready = self.index_ready();

        let given: Vec<String> = as_list(g("paths")).iter().map(path_string).collect();
        // `paths` entries are strings in practice; keep the raw JS list for the matcher
        let named_in: Vec<String> = if self.unindexed && given.is_empty() { vec![String::new()] } else { given.clone() };
        let mut named: Vec<(String, Option<String>)> = Vec::new();
        for p in &named_in {
            match self.library_path_ex(p) {
                LibPath::No => named.push((p.clone(), None)),
                LibPath::Rel(r) => named.push((p.clone(), Some(r))),
                LibPath::Error(m) => return Err(Error::code("OKCODE_BAD_PATH", format!("{p}: {m}"))),
            }
        }
        let lib_roots: Vec<(String, String)> = named.iter().filter_map(|(p, l)| l.clone().map(|l| (p.clone(), l))).collect();
        let globs = only_strings(g("glob"));

        let mut universe_n: usize;
        let mut rels: Vec<String>;
        let mut via: &'static str;
        let mut lib_set: Option<HashSet<String>> = None;
        let mut lib_list: Option<Value> = None;
        let mut lib_files = 0usize;
        let mut lib_roots_out: Vec<String> = vec![];
        if lib_roots.is_empty() {
            let in_path = path_matcher(&globs, &given).map_err(Error::msg)?;
            let uni: Vec<String> = self.grep_universe(ready)?.into_iter().filter(|r| in_path.as_ref().map_or(true, |m| m.matches(r))).collect();
            universe_n = uni.len();
            (rels, via) = self.narrow(uni, &matcher, ready);
        } else {
            let own: Vec<String> = named.iter().filter(|(_, l)| l.is_none()).map(|(p, _)| p.clone()).collect();
            let in_path = if own.is_empty() { None } else { path_matcher(&globs, &own).map_err(Error::msg)? };
            let uni: Vec<String> = if own.is_empty() {
                vec![]
            } else {
                self.grep_universe(ready)?.into_iter().filter(|r| in_path.as_ref().map_or(true, |m| m.matches(r))).collect()
            };
            universe_n = uni.len();
            (rels, via) = self.narrow(uni, &matcher, ready);
            let roots: Vec<String> = lib_roots.iter().map(|(_, l)| l.clone()).collect();
            let listed = self.list_library(&roots, self.opts.library_files.max(1))?;
            let in_glob = path_matcher(&globs, &[]).map_err(Error::msg)?;
            let lib_universe: Vec<String> =
                lib_rows(&listed).into_iter().map(|(r, _)| r).filter(|r| in_glob.as_ref().map_or(true, |m| m.matches(r))).collect();
            lib_files = lib_universe.len();
            let (lib_rels, lib_via) = self.narrow(lib_universe, &matcher, false);
            lib_set = Some(lib_rels.iter().cloned().collect());
            rels.extend(lib_rels);
            rels.sort_by(|a, b| cmp_js(a, b));
            if own.is_empty() {
                via = lib_via;
            }
            universe_n += lib_files;
            lib_roots_out = str_vec(listed.get("roots")).unwrap_or_else(|| {
                let mut u: Vec<String> = vec![];
                for r in &roots {
                    if !u.contains(r) {
                        u.push(r.clone());
                    }
                }
                u
            });
            lib_list = Some(listed);
        }

        // Every candidate is read to the end: the counts are exact whatever is shown. Only the
        // lines of the requested page are materialized (the pager: see workspace.js grepAll).
        struct Count {
            file: String,
            count: usize,
            at: Option<String>,
        }
        let mut counts: Vec<Count> = vec![];
        let mut tally: Option<Vec<(String, Tally)>> = if output == "matches" { Some(vec![]) } else { None };
        let mut tally_ix: HashMap<String, usize> = HashMap::new();
        let mut matches: Vec<Value> = vec![];
        let mut per_file: Vec<Value> = vec![];
        let mut stream = 0usize;
        let mut pg = 1usize;
        let mut in_page = 0usize;
        let mut files_in_page = 0usize;
        let mut page_start: Option<usize> = None;
        let mut page_end: Option<usize> = None;
        let mut begun = false;
        let b = self.opts.read_batch_files.max(1);
        for chunk in rels.chunks(b) {
            let mut texts;
            match &lib_set {
                None => texts = self.grep_texts(chunk, ready)?,
                Some(ls) => {
                    // Library files are always read directly: never verified into the working
                    // set, never recorded.
                    let (lib, own): (Vec<String>, Vec<String>) = chunk.iter().cloned().partition(|r| ls.contains(r));
                    texts = self.grep_texts(&own, ready)?;
                    texts.extend(self.grep_texts(&lib, false)?);
                }
            }
            for rel in chunk {
                let Some(got) = texts.get(rel) else { continue };
                let c = if let Some(t) = tally.as_mut() {
                    // tally this file alone, then fold it in (same order and counts as one Map)
                    let mut local: Vec<(String, Tally)> = vec![];
                    let n = okcode_analysis::grep::tally_text(&got.text, &matcher, &mut local, Some(rel));
                    for (k, lt) in local {
                        match tally_ix.get(&k) {
                            Some(&i) => {
                                let e = &mut t[i].1;
                                e.count += lt.count;
                                e.lines += lt.lines;
                                e.files += lt.files;
                            }
                            None => {
                                tally_ix.insert(k.clone(), t.len());
                                t.push((k, Tally { first: Some(rel.clone()), ..lt }));
                            }
                        }
                    }
                    n
                } else {
                    match_text(&got.text, &matcher, &MatchOpts { max_per_file: 0, ..Default::default() }).1
                };
                if c == 0 {
                    continue;
                }
                let start = stream;
                stream += c;
                counts.push(Count { file: rel.clone(), count: c, at: got.at.clone() });
                if output != "lines" {
                    continue;
                }
                let mut k = offset.saturating_sub(start);
                let mut shown_in_file = 0usize;
                let mut on_page = false;
                let mut win: Option<(usize, usize, usize)> = None;
                while k < c {
                    if !begun {
                        begun = true;
                    } else if in_page >= max_matches || (!on_page && files_in_page >= max_files) {
                        pg += 1;
                        in_page = 0;
                        files_in_page = 0;
                        shown_in_file = 0;
                        on_page = false;
                    }
                    if pg == page && page_start.is_none() {
                        page_start = Some(start + k);
                    }
                    if pg > page && page_end.is_none() {
                        page_end = Some(start + k);
                    }
                    if !on_page {
                        on_page = true;
                        files_in_page += 1;
                    }
                    let room = (max_matches.saturating_sub(in_page)).min(max_per_file.saturating_sub(shown_in_file));
                    let take = room.min(c - k);
                    if take == 0 {
                        break;
                    }
                    if pg == page {
                        win = Some((k, take, 0));
                    }
                    in_page += take;
                    shown_in_file += take;
                    k += take;
                    // Past the per-file cap the rest of this file is passed over on this page.
                    if shown_in_file >= max_per_file && k < c {
                        if pg == page {
                            if let Some(w) = win.as_mut() {
                                w.2 = c - k;
                            }
                        }
                        k = c;
                    }
                }
                let Some((skip, take, passed)) = win else { continue };
                let (ms, _) = match_text(&got.text, &matcher, &MatchOpts { before, after, skip, max_per_file: take });
                let shown = ms.len();
                for m in ms {
                    matches.push(json!({
                        "file": rel, "line": m.line, "col": m.col, "text": m.text,
                        "before": m.before, "after": m.after, "n": m.n,
                    }));
                }
                let mut row = Map::new();
                row.insert("file".into(), json!(rel));
                row.insert("count".into(), json!(c));
                row.insert("shown".into(), json!(shown));
                row.insert("at".into(), json!(got.at));
                if skip > 0 {
                    row.insert("from".into(), json!(skip));
                }
                if passed > 0 {
                    row.insert("passed".into(), json!(passed));
                }
                per_file.push(Value::Object(row));
            }
        }
        let total_lines = stream;
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("id".into(), json!(self.id));
        out.insert("pattern".into(), json!(matcher.source));
        out.insert("regex".into(), json!(matcher.regex));
        out.insert("ignoreCase".into(), json!(matcher.ignore_case));
        out.insert("output".into(), json!(output));

        let count_rows: Vec<Value> =
            counts.iter().map(|c| json!({"file": c.file, "count": c.count, "shown": 0, "at": c.at})).collect();
        let tail = |out: &mut Map<String, Value>, counts: &[Count], start_of: &[usize]| {
            let rows: Vec<Value> = counts
                .iter()
                .zip(start_of)
                .map(|(c, s)| json!({"file": c.file, "count": c.count, "start": s, "at": c.at}))
                .collect();
            out.insert("counts".into(), Value::Array(rows));
            out.insert("searched".into(), json!(universe_n));
            out.insert("candidates".into(), json!(rels.len()));
            out.insert("via".into(), json!(via));
            out.insert("indexed".into(), json!(ready));
            if let Some(l) = &lib_list {
                let mut lib = Map::new();
                lib.insert("paths".into(), json!(lib_roots_out));
                lib.insert("files".into(), json!(lib_files));
                lib.insert("truncated".into(), json!(l.get("truncated").and_then(Value::as_bool).unwrap_or(false)));
                lib.insert("cap".into(), json!(self.opts.library_files));
                if let Some(m) = str_vec(l.get("missing")).filter(|m| !m.is_empty()) {
                    lib.insert("missing".into(), json!(m));
                }
                out.insert("library".into(), Value::Object(lib));
            }
            out.insert("elapsedMs".into(), json!(t0.elapsed().as_millis() as u64));
        };
        // starts of each counted file (the stream index its first line has)
        let mut starts = Vec::with_capacity(counts.len());
        let mut acc = 0usize;
        for c in &counts {
            starts.push(acc);
            acc += c.count;
        }

        if output == "files" {
            let per = match max_files_opt {
                Some(m) => m,
                None => counts.len().max(1),
            };
            let from = offset.saturating_add((page - 1).saturating_mul(per));
            let rows: Vec<Value> = count_rows.iter().skip(from).take(per).cloned().collect();
            let end = from + rows.len();
            out.insert("matches".into(), json!([]));
            out.insert("files".into(), Value::Array(rows.clone()));
            out.insert("total".into(), json!({"lines": total_lines, "files": counts.len()}));
            out.insert("page".into(), json!(page));
            out.insert("pages".into(), json!(counts.len().saturating_sub(offset).div_ceil(per)));
            out.insert("offset".into(), json!(from));
            out.insert("next".into(), if end < counts.len() { json!({"page": page.saturating_add(1), "offset": end}) } else { Value::Null });
            out.insert("truncated".into(), json!(rows.len() < counts.len()));
            tail(&mut out, &counts, &starts);
            return Ok(Value::Object(out));
        }
        if let Some(mut all) = tally {
            all.sort_by(|(ka, a), (kb, b)| b.count.cmp(&a.count).then_with(|| cmp_js(ka, kb)));
            let from = offset.saturating_add((page - 1).saturating_mul(max_matches));
            let rows: Vec<Value> = all
                .iter()
                .skip(from)
                .take(max_matches)
                .map(|(k, t)| json!({"text": k, "count": t.count, "lines": t.lines, "files": t.files, "first": t.first}))
                .collect();
            let end = from + rows.len();
            let occurrences: usize = all.iter().map(|(_, t)| t.count).sum();
            out.insert("matches".into(), json!([]));
            out.insert(
                "distinct".into(),
                Value::Array(rows.clone()),
            );
            out.insert("files".into(), json!([]));
            out.insert(
                "total".into(),
                json!({"lines": total_lines, "files": counts.len(), "distinct": all.len(), "occurrences": occurrences}),
            );
            out.insert("page".into(), json!(page));
            out.insert("pages".into(), json!(all.len().saturating_sub(offset).div_ceil(max_matches)));
            out.insert("offset".into(), json!(from));
            out.insert("next".into(), if end < all.len() { json!({"page": page.saturating_add(1), "offset": end}) } else { Value::Null });
            out.insert("truncated".into(), json!(rows.len() < all.len()));
            tail(&mut out, &counts, &starts);
            return Ok(Value::Object(out));
        }
        let shown = matches.len();
        out.insert("matches".into(), Value::Array(matches));
        out.insert("files".into(), Value::Array(per_file));
        out.insert("total".into(), json!({"lines": total_lines, "files": counts.len()}));
        out.insert("page".into(), json!(page));
        out.insert("pages".into(), json!(if begun { pg } else { 0 }));
        out.insert("offset".into(), json!(page_start.unwrap_or_else(|| offset.max(total_lines))));
        out.insert("next".into(), match page_end {
            None => Value::Null,
            Some(e) => json!({"page": page.saturating_add(1), "offset": e}),
        });
        out.insert("truncated".into(), json!(shown < total_lines));
        tail(&mut out, &counts, &starts);
        Ok(Value::Object(out))
    }

    // ── api ─────────────────────────────────────────────────────────────

    pub fn grep(&self, pattern: &Value, o: &Value) -> Result<Value> {
        self.grep_all(pattern, o)
    }

    /// glob(pattern, { limit, offset })
    pub fn glob(&self, pattern: &Value, o: &Value) -> Result<Value> {
        let pats: Vec<String> = as_list(Some(pattern)).into_iter().filter_map(|p| p.as_str().filter(|s| !s.trim().is_empty()).map(String::from)).collect();
        if pats.is_empty() {
            return Ok(json!({"ok": false, "reason": "glob needs a pattern, e.g. \"src/**/*.js\" or \"*.test.js\""}));
        }
        let m = path_matcher(&pats, &[]).map_err(Error::msg)?;
        let ready = self.index_ready();
        // (file, size, lines, lang, indexed)
        let mut rows: Vec<(String, Value, Value, Value, bool)> = vec![];
        let opt_size = |s: u64| json!(s);
        if self.unindexed {
            let listed = self.list_library(&[String::new()], self.opts.library_files.max(1))?;
            for r in listed.get("rows").and_then(Value::as_array).cloned().unwrap_or_default() {
                let rel = row_str(&r, "rel");
                let lang = json!(lang_of(&rel));
                rows.push((rel, r.get("size").cloned().unwrap_or(Value::Null), Value::Null, lang, false));
            }
        } else if ready {
            self.ensure_open()?;
            let ix = self.read_ix();
            for f in ix.files.values() {
                let lines = json!(f.lines);
                rows.push((f.rel.clone(), opt_size(f.size), lines, json!(f.lang), f.indexed));
            }
        } else {
            self.warm_up();
            for r in self.list_tree(&|_: &str| {})? {
                let ind = is_textual(&r.path, Some(r.size));
                let lang = json!(lang_of(&r.path));
                rows.push((r.path.clone(), opt_size(r.size), Value::Null, lang, ind));
            }
        }
        rows.retain(|r| m.as_ref().map_or(true, |m| m.matches(&r.0)));
        rows.sort_by(|a, b| cmp_js(&a.0, &b.0));
        let total = rows.len();
        let num = |k: &str| o.get(k).map(js_number).filter(|n| *n > 0.0).map_or(0usize, |n| n.floor() as usize);
        let (off, n) = (num("offset"), num("limit"));
        let rows: Vec<_> = if off > 0 || n > 0 {
            rows.into_iter().skip(off).take(if n > 0 { n } else { usize::MAX }).collect()
        } else {
            rows
        };
        let truncated = off + rows.len() < total;
        let files: Vec<Value> = rows
            .into_iter()
            .map(|(file, size, lines, lang, indexed)| json!({"file": file, "size": size, "lines": lines, "lang": lang, "indexed": indexed}))
            .collect();
        Ok(json!({
            "ok": true,
            "id": self.id,
            "pattern": pats,
            "total": total,
            "offset": off,
            "truncated": truncated,
            "next": if truncated { json!(off + files.len()) } else { Value::Null },
            "via": if ready { "index" } else { "direct" },
            "files": files,
        }))
    }
}
