use okcode_chunk::golden_call;

fn run(name: &str, threshold: f64) {
    let cases = okcode_golden::cases(name);
    let total = cases.len();
    let mut pass = 0;
    let mut diffs = Vec::new();
    for (i, c) in cases.iter().enumerate() {
        let got = golden_call(name, &c.args);
        let diff = match (&c.out, &c.err, got) {
            (Some(want), _, Ok(g)) => okcode_golden::first_diff(want, &g, "$"),
            (None, Some(_), Err(_)) => None,
            (_, _, Err(e)) => Some(format!("error {e}")),
            (None, None, Ok(g)) => okcode_golden::first_diff(&serde_json::Value::Null, &g, "$"),
            (None, Some(e), Ok(_)) => Some(format!("expected error {e}")),
        };
        match diff {
            None => pass += 1,
            Some(d) => diffs.push(format!("case {i}: {d}")),
        }
    }
    println!("golden {name}: {pass}/{total}");
    for d in diffs.iter().take(5) {
        println!("  {d}");
    }
    assert!(total > 0 && pass as f64 / total as f64 >= threshold, "{name} below threshold");
}

#[test]
fn regions() { run("regions", 0.95); }
#[test]
fn section_of() { run("sectionOf", 0.95); }
#[test]
fn chunk_code() { run("chunkCode", 0.95); }
#[test]
fn prepare_text() { run("prepareText", 0.95); }
