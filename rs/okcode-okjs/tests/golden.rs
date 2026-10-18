use okcode_golden::{cases, first_diff};
use serde_json::Value;
use std::time::Instant;

#[test]
fn golden_extract_okjs() {
    let (mut pass, mut total, mut skipped) = (0, 0, 0);
    let mut diffs = Vec::new();
    let mut inputs = Vec::new();
    for c in cases("extract") {
        let Some(path) = c.args.first().and_then(Value::as_str) else { continue };
        if !okcode_okjs::matches(path) {
            continue;
        }
        let Some(want) = c.out.as_ref().and_then(okcode_okjs::project_expected) else {
            skipped += 1;
            continue;
        };
        total += 1;
        match okcode_okjs::golden_call("extract", &c.args) {
            Ok(got) => match first_diff(&want, &got, path) {
                None => pass += 1,
                Some(d) => diffs.push(d),
            },
            Err(e) => diffs.push(format!("{path}: error {e}")),
        }
        inputs.push(c.args);
    }
    // Median per-file analyse time (runtime already warm), 20 reps over every case.
    let mut times = Vec::new();
    for _ in 0..20 {
        for a in &inputs {
            let t = Instant::now();
            okcode_okjs::golden_call("extract", a).unwrap();
            times.push(t.elapsed().as_secs_f64() * 1000.0);
        }
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let t0 = Instant::now();
    let _ = okcode_okjs::Analyzer::new().unwrap();
    println!("runtime init: {:.1} ms", t0.elapsed().as_secs_f64() * 1000.0);
    println!("median analyse: {:.3} ms (n={})", times[times.len() / 2], times.len());
    println!("golden extract(okjs): {pass}/{total} (skipped {skipped} recorded without okjs tooling)");
    for d in diffs.iter().take(5) {
        println!("  diff: {d}");
    }
    assert!(total == 0 || pass as f64 / total as f64 >= 0.95);
}
