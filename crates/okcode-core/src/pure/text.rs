//! pure/text: the pure exports of lib/analysis/{text,chunk,template-balance,convention-color}.js
//! (see pure.rs). The crates' dispatch already speaks the JS
//! shapes; the exports it lacks (slug, boundaries*, fromBoundaries, markLength, headedChunks) are here.
use super::g;
use crate::error::{Error, Result};
use okcode_analysis::chunk as ck;
use okcode_analysis::text as tx;
use serde_json::{json, Value};

fn str_arg(args: &[Value], i: usize) -> String {
    match args.get(i) {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(v) => v.to_string(),
    }
}

/// slug(s): lowercase, runs of non [a-z0-9] -> `_`, one leading/trailing `_` dropped.
fn slug(s: &str) -> String {
    let mut out = String::new();
    let mut run = false;
    for c in s.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if run {
                out.push('_');
                run = false;
            }
            out.push(c);
        } else {
            run = true;
        }
    }
    if run {
        out.push('_');
    }
    let t = out.strip_prefix('_').unwrap_or(&out);
    t.strip_suffix('_').unwrap_or(t).to_string()
}

fn marks_json(m: Option<Vec<usize>>) -> Value {
    match m {
        Some(v) => json!(v),
        None => Value::Null,
    }
}

fn chunks_json(c: Vec<ck::Chunk>) -> Value {
    Value::Array(c.into_iter().map(|c| json!({"text": c.text, "start": c.start, "end": c.end})).collect())
}

fn u16s(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// chunk.js#headedChunks: each chunk prefixed with `// rel › owner.path` and the owner's doc line.
fn headed_chunks(text: &str) -> Value {
    let (mut rel, mut offset, mut body) = (None::<String>, 0usize, text.to_string());
    if let Some(rest) = text.strip_prefix(ck::MARK) {
        let nl = rest.find('\n');
        let (r, after) = match nl {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            // JS: indexOf -> -1 slices oddly; a stamp always ends in \n, so treat as all-rel/empty body
            None => (rest, ""),
        };
        offset = ck::MARK.len() + u16s(r).len() + 1;
        rel = Some(r.to_string());
        body = after.to_string();
    }
    let chunks = ck::chunk_code(&body, rel.as_deref().filter(|s| !s.is_empty()));
    let Some(rel) = rel.filter(|s| !s.is_empty()) else {
        return chunks_json(chunks);
    };
    // chunk.js#headedChunks: a failing extract means no headers, never a failed chunking
    let rec = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| okcode_analysis::parse::extract(&rel, Some(&body)))).unwrap_or(Value::Null);
    let symbols: Vec<&Value> = rec["symbols"].as_array().map(|a| a.iter().collect()).unwrap_or_default();
    let b = u16s(&body);
    let mut line_at = vec![0usize];
    for (i, &c) in b.iter().enumerate() {
        if c == 10 {
            line_at.push(i + 1);
        }
    }
    let line_of = |pos: usize| {
        let (mut lo, mut hi) = (0usize, line_at.len() - 1);
        while lo < hi {
            let mid = (lo + hi + 1) >> 1;
            if line_at[mid] <= pos {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        lo as i64 + 1
    };
    let out: Vec<Value> = chunks
        .into_iter()
        .map(|c| {
            let line = line_of(c.start);
            let mut owner: Option<&Value> = None;
            for s in &symbols {
                let (ls, le) = (s["lineStart"].as_i64().unwrap_or(0), s["lineEnd"].as_i64().unwrap_or(0));
                if line < ls || line > le {
                    continue;
                }
                let better = match owner {
                    None => true,
                    Some(o) => le - ls < o["lineEnd"].as_i64().unwrap_or(0) - o["lineStart"].as_i64().unwrap_or(0),
                };
                if better {
                    owner = Some(s);
                }
            }
            let mut head = format!("// {rel}");
            if let Some(o) = owner {
                let name = o["path"].as_str().filter(|s| !s.is_empty()).or_else(|| o["name"].as_str()).unwrap_or("");
                head.push_str(&format!(" \u{203a} {name}"));
            }
            head.push('\n');
            if let Some(d) = owner.and_then(|o| o["doc"].as_str()).filter(|s| !s.is_empty()) {
                let d16 = u16s(d);
                let d200 = String::from_utf16_lossy(&d16[..d16.len().min(200)]);
                head.push_str(&format!("// {d200}\n"));
            }
            json!({"text": format!("{head}\n{}", c.text), "start": c.start + offset, "end": c.end + offset})
        })
        .collect();
    Value::Array(out)
}

/// None = not handled here (the router reports OKCODE_NOT_IMPLEMENTED).
pub fn call(module: &str, name: &str, args: &[Value]) -> Option<Result<Value>> {
    let key = match module {
        "analysis/text" => format!("text.{name}"),
        "analysis/template-balance" => format!("templateBalance.{name}"),
        "analysis/convention-color" => format!("conventionColor.{name}"),
        "analysis/chunk" => {
            let t = str_arg(args, 0);
            let r = |v: Value| Some(Ok(v));
            return match name {
                "chunkCode" | "regions" | "sectionOf" | "prepareText" => g(ck::dispatch(name, args)),
                "slug" => r(json!(slug(&str_arg(args, 0)))),
                "markLength" => r(json!(ck::mark_length(&str_arg(args, 0)))),
                "boundaries" => r(marks_json(ck::boundaries(&t))),
                "paragraphBoundaries" => r(json!(ck::paragraph_boundaries(&t))),
                "markdownBoundaries" => r(json!(ck::markdown_boundaries(&t))),
                "htmlBoundaries" => r(json!(ck::html_boundaries(&t))),
                "jsonBoundaries" => r(json!(ck::json_boundaries(&t))),
                "boundariesFor" => r(marks_json(ck::boundaries_for(&t, &str_arg(args, 1)))),
                "fromBoundaries" => {
                    let marks: Vec<usize> = match args.get(1).and_then(|v| v.as_array()) {
                        Some(a) => a.iter().map(|v| v.as_f64().unwrap_or(0.0).max(0.0) as usize).collect(),
                        None => return Some(Err(Error::msg("fromBoundaries: marks must be an array"))),
                    };
                    r(chunks_json(ck::from_boundaries(&u16s(&t), &marks)))
                }
                "headedChunks" => r(headed_chunks(&t)),
                _ => None,
            };
        }
        _ => return None,
    };
    let num = |i: usize| args.get(i).and_then(|v| v.as_f64()).unwrap_or(0.0) as i64;
    match name {
        "lineStartOf" => return Some(Ok(json!(tx::text::line_start_of(&u16s(&str_arg(args, 0)), num(1))))),
        "lineEndOf" => return Some(Ok(json!(tx::text::line_end_of(&u16s(&str_arg(args, 0)), num(1))))),
        "widenToContext" => {
            let w = tx::text::widen_to_context(&u16s(&str_arg(args, 0)), num(1), num(2), num(3));
            return Some(Ok(json!({"start": w.start, "end": w.end, "lineStart": w.line_start, "lineEnd": w.line_end})));
        }
        _ => {}
    }
    match name {
        "decode" | "dominantEol" | "normaliseEol" | "lineOffsets" | "lineRangeToOffsets" | "lineColAt" | "offsetAtLineCol"
        | "hunksFor" | "check" | "templateSpans" | "rgbOf" | "paletteFrom" | "nearest" | "colorLiterals" => {
            g(tx::dispatch(&key, args))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slug_cases() {
        assert_eq!(slug("--A.b--"), "a_b");
        assert_eq!(slug("qwen3-embedding:0.6b"), "qwen3_embedding_0_6b");
        assert_eq!(slug(""), "");
    }
}
