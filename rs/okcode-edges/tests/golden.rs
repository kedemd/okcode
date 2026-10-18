use okcode_edges::golden_call;
use okcode_golden::{cases, first_diff};

fn run(name: &str) -> (usize, usize) {
    let cs = cases(name);
    let mut pass = 0;
    let mut shown = 0;
    for c in &cs {
        let got = golden_call(name, &c.args);
        let ok = match (&got, &c.out, &c.err) {
            (Ok(g), Some(w), _) => match first_diff(w, g, "$") {
                None => true,
                Some(d) => {
                    if shown < 5 {
                        eprintln!("  {name}: {d}");
                        shown += 1;
                    }
                    false
                }
            },
            (Err(_), _, Some(_)) => true,
            _ => false,
        };
        pass += ok as usize;
    }
    println!("golden {name}: {pass}/{}", cs.len());
    (pass, cs.len())
}

#[test]
fn packages_goldens() {
    let (mut p, mut t) = (0, 0);
    for f in ["declared", "membersFromDts", "namesFromDts", "reExports"] {
        let (a, b) = run(&format!("packages.{f}"));
        p += a;
        t += b;
    }
    for f in ["readPackages", "scanPackages"] {
        println!("golden packages.{f}: SKIPPED (recorder serialised a Promise; access mock not rebuildable)");
    }
    assert!(p * 100 >= t * 95, "{p}/{t}");
}
