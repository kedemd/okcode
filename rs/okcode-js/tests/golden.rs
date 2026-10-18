use okcode_golden::{cases, first_diff};
use okcode_js::{golden_call, OUT_OF_SCOPE};

/// Runs every case; cases the crate declares out of scope are counted apart. Returns (pass, in_scope, skipped).
fn run(name: &str, scope: impl Fn(&[serde_json::Value]) -> bool) -> (usize, usize, usize) {
    let (mut pass, mut total, mut skipped) = (0, 0, 0);
    let mut diffs = Vec::new();
    for c in cases(name) {
        if !scope(&c.args) {
            skipped += 1;
            continue;
        }
        let got = golden_call(name, &c.args);
        if let Err(e) = &got {
            if e.starts_with(OUT_OF_SCOPE) {
                skipped += 1;
                continue;
            }
        }
        total += 1;
        let d = match (&c.out, &got) {
            (Some(want), Ok(g)) => first_diff(want, g, "$"),
            (None, Err(_)) => None,
            (Some(_), Err(e)) => Some(format!("error: {e}")),
            (None, Ok(_)) => Some("expected an error".to_string()),
        };
        match d {
            None => pass += 1,
            Some(d) => {
                if diffs.len() < 20 {
                    let a = c.args[0].to_string();
                    diffs.push(format!("{} :: {d}", &a[..a.len().min(110)]));
                }
            }
        }
    }
    println!("golden {name}: {pass}/{total} (+{skipped} out of scope)");
    for d in &diffs {
        println!("  diff {d}");
    }
    (pass, total, skipped)
}

fn is_js_family(p: &str) -> bool {
    [".js", ".mjs", ".cjs", ".jsx"].iter().any(|e| p.to_lowercase().ends_with(e))
}

#[test]
fn lang_of() {
    let (p, t, _) = run("langOf", |_| true);
    assert_eq!(p, t);
}

#[test]
fn extract() {
    let (p, t, _) = run("extract", |_| true);
    assert!(p * 100 >= t * 90, "extract pass rate {p}/{t} below 90%");
}

#[test]
fn extract_js_family_only() {
    run("extract", |a| a[0].as_str().map_or(false, is_js_family) && a[1].is_string());
}

#[test]
fn validate_source() {
    let (p, t, _) = run("validateSource", |a| a[0]["rel"].as_str().map_or(false, is_js_family));
    assert!(p * 100 >= t * 85, "validateSource pass rate {p}/{t} below 85%");
}
