use okcode_golden::{cases, first_diff};
use serde_json::Value;

fn run(f: &str) -> f64 {
    let cs = cases(&format!("grep.{f}"));
    let total = cs.len();
    let mut pass = 0;
    let mut shown = 0;
    for (i, c) in cs.iter().enumerate() {
        let got = okcode_grep::golden_call(f, &c.args);
        let diff = match (&c.err, &c.out, &got) {
            (Some(e), _, Err(g)) => if e == g { None } else { Some(format!("err want {e:?} got {g:?}")) },
            (Some(e), _, Ok(v)) => Some(format!("want err {e:?} got {v}")),
            (None, _, Err(g)) => Some(format!("unexpected err {g:?}")),
            (None, out, Ok(v)) => first_diff(out.as_ref().unwrap_or(&Value::Null), v, "$"),
        };
        match diff {
            None => pass += 1,
            Some(d) => {
                if shown < 5 {
                    shown += 1;
                    eprintln!("  case {i}: {d}");
                }
            }
        }
    }
    println!("golden {f}: {pass}/{total}");
    pass as f64 / total as f64
}

#[test]
fn golden() {
    let mut bad = vec![];
    for f in ["globToRegExp", "pathMatcher", "compileMatcher", "matchText", "tallyText", "indexTerms", "splitList"] {
        if run(f) < 0.95 {
            bad.push(f);
        }
    }
    assert!(bad.is_empty(), "below 95%: {bad:?}");
}
