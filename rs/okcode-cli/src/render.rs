//! bin/okcode.js output helpers: out/err/j/pad/tree, with JS text semantics (JSON.stringify
//! number printing, String(v), padEnd by UTF-16 units).

use serde_json::Value;

pub fn out(s: &str) {
    println!("{s}");
}

pub fn err(s: &str) {
    eprintln!("{s}");
}

/// j(o): JSON.stringify(o, null, 2)
pub fn j(v: &Value) {
    out(&stringify(v));
}

/// JSON.stringify(v, null, 2). serde's pretty printer differs from JS only in numbers (a whole
/// f64 prints `1.0`, JS `1`), so numbers go through js_num.
pub fn stringify(v: &Value) -> String {
    let mut s = String::new();
    write(&mut s, v, 0);
    s
}

fn write(s: &mut String, v: &Value, depth: usize) {
    let ind = |n: usize| "  ".repeat(n);
    match v {
        Value::Number(n) => s.push_str(&num(n)),
        Value::Array(a) if a.is_empty() => s.push_str("[]"),
        Value::Object(o) if o.is_empty() => s.push_str("{}"),
        Value::Array(a) => {
            s.push_str("[\n");
            for (i, x) in a.iter().enumerate() {
                s.push_str(&ind(depth + 1));
                write(s, x, depth + 1);
                if i + 1 < a.len() {
                    s.push(',');
                }
                s.push('\n');
            }
            s.push_str(&ind(depth));
            s.push(']');
        }
        Value::Object(o) => {
            s.push_str("{\n");
            for (i, (k, x)) in o.iter().enumerate() {
                s.push_str(&ind(depth + 1));
                s.push_str(&serde_json::to_string(k).unwrap_or_default());
                s.push_str(": ");
                write(s, x, depth + 1);
                if i + 1 < o.len() {
                    s.push(',');
                }
                s.push('\n');
            }
            s.push_str(&ind(depth));
            s.push('}');
        }
        other => s.push_str(&serde_json::to_string(other).unwrap_or_default()),
    }
}

fn num(n: &serde_json::Number) -> String {
    if n.is_i64() || n.is_u64() {
        return n.to_string();
    }
    js_num(n.as_f64().unwrap_or(f64::NAN))
}

/// JS Number → string (String(1.0) is "1", 1e21 is "1e+21", 1e-7 is "1e-7").
pub fn js_num(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    if f == 0.0 {
        return "0".into();
    }
    let a = f.abs();
    if (1e-7..1e21).contains(&a) {
        // Rust's shortest round-trip repr matches JS's in this range, minus a trailing ".0"
        let s = format!("{f}");
        return s.strip_suffix(".0").map(String::from).unwrap_or(s);
    }
    // exponent form: Rust `{:e}` gives "1e21" / "1.5e-8"; JS "1e+21" / "1.5e-8"
    let s = format!("{f:e}");
    match s.split_once('e') {
        Some((m, e)) if !e.starts_with('-') => format!("{m}e+{e}"),
        _ => s,
    }
}

/// `${v}` / String(v) for a JSON value as the CLI prints it (undefined is handled by callers).
pub fn js_str(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => num(n),
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(join_item).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

/// Array.join element: null/undefined join as ''.
fn join_item(v: &Value) -> String {
    if v.is_null() { String::new() } else { js_str(v) }
}

/// pad(s, n): String(s ?? '').padEnd(n), lengths in UTF-16 units
pub fn pad(v: &Value, n: usize) -> String {
    let s = if v.is_null() { String::new() } else { js_str(v) };
    pad_str(&s, n)
}

pub fn pad_str(s: &str, n: usize) -> String {
    let len = s.encode_utf16().count();
    if len >= n { s.to_string() } else { format!("{s}{}", " ".repeat(n - len)) }
}

/// tree(o): the readable view of a management result.
pub fn tree(o: &Value, indent: &str) -> String {
    let entries: Vec<(String, &Value)> = match o {
        Value::Array(a) => a.iter().enumerate().map(|(i, v)| (format!("[{i}]"), v)).collect(),
        Value::Object(m) => m.iter().map(|(k, v)| (k.clone(), v)).collect(),
        other => return format!("{indent}{}", js_str(other)),
    };
    let mut lines = Vec::new();
    for (k, v) in entries {
        let nested = match v {
            Value::Object(_) => true,
            Value::Array(a) => !a.iter().all(|x| !x.is_object() && !x.is_array()),
            _ => false,
        };
        if nested {
            lines.push(format!("{indent}{k}:"));
            lines.push(tree(v, &format!("{indent}  ")));
        } else if let Value::Array(a) = v {
            let joined = a.iter().map(join_item).collect::<Vec<_>>().join(", ");
            lines.push(format!("{indent}{k}: {}", if joined.is_empty() { "(none)" } else { &joined }));
        } else {
            lines.push(format!("{indent}{k}: {}", js_str(v)));
        }
    }
    lines.join("\n")
}

/// new Date(ms).toISOString()
pub fn iso(ms: f64) -> String {
    let ms = ms as i64;
    let (secs, milli) = (ms.div_euclid(1000), ms.rem_euclid(1000));
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // civil_from_days (H. Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{milli:03}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stringify_like_js() {
        assert_eq!(stringify(&json!({"a": 1.0, "b": [], "c": {}, "d": [1, "x"]})), "{\n  \"a\": 1,\n  \"b\": [],\n  \"c\": {},\n  \"d\": [\n    1,\n    \"x\"\n  ]\n}");
        assert_eq!(js_num(1.5), "1.5");
        assert_eq!(js_num(1e21), "1e+21");
        assert_eq!(js_num(1e-8), "1e-8");
    }

    #[test]
    fn tree_like_js() {
        let v = json!({"id": "fx", "files": 3, "tags": ["a", null], "none": [], "ws": [{"x": 1}]});
        assert_eq!(tree(&v, ""), "id: fx\nfiles: 3\ntags: a, \nnone: (none)\nws:\n  [0]:\n    x: 1");
    }

    #[test]
    fn iso_like_js() {
        assert_eq!(iso(0.0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso(1_760_000_000_123.0), "2025-10-09T08:53:20.123Z");
    }
}
