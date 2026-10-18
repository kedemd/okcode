//! Golden cases recorded from the JS build (rs/golden/record.js → build.js).
//! A port's test is one loop: for each case, run the Rust fn on `args`, compare with `out` (or `err`).
use serde_json::Value;
use std::io::Read;
use std::path::PathBuf;

pub struct Case {
    pub args: Vec<Value>,
    pub out: Option<Value>,
    pub err: Option<String>,
}

pub fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../golden/data")
}

/// All recorded cases for one JS entry point (`extract`, `validateSource`, `text.decode`, …).
pub fn cases(fn_name: &str) -> Vec<Case> {
    let path = data_dir().join(format!("{fn_name}.jsonl.gz"));
    let f = std::fs::File::open(&path).unwrap_or_else(|e| panic!("golden {}: {e}", path.display()));
    let mut body = String::new();
    flate2::read::GzDecoder::new(f).read_to_string(&mut body).expect("gunzip golden");
    body.lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let v: Value = serde_json::from_str(l).expect("golden line");
            Case {
                args: v["args"].as_array().cloned().unwrap_or_default(),
                out: v.get("out").cloned(),
                err: v.get("err").and_then(|e| e.as_str()).map(str::to_owned),
            }
        })
        .collect()
}

/// First differing JSON path between expected and actual, for a one-line failure message.
pub fn first_diff(want: &Value, got: &Value, at: &str) -> Option<String> {
    match (want, got) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, va) in a {
                match b.get(k) {
                    None if va.is_null() => {}
                    None => return Some(format!("{at}.{k}: missing (want {va})")),
                    Some(vb) => {
                        if let Some(d) = first_diff(va, vb, &format!("{at}.{k}")) {
                            return Some(d);
                        }
                    }
                }
            }
            b.keys().find(|k| !a.contains_key(*k) && !b[*k].is_null()).map(|k| format!("{at}.{k}: unexpected"))
        }
        (Value::Array(a), Value::Array(b)) => {
            if a.len() != b.len() {
                return Some(format!("{at}: len {} vs {}", a.len(), b.len()));
            }
            a.iter().zip(b).enumerate().find_map(|(i, (x, y))| first_diff(x, y, &format!("{at}[{i}]")))
        }
        _ if want == got => None,
        _ => Some(format!("{at}: want {want} got {got}")),
    }
}
