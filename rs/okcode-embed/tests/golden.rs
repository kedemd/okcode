use okcode_golden::{cases, first_diff};

fn run(f: &str) {
    let cs = cases(f);
    let (mut pass, mut diffs) = (0, vec![]);
    for c in &cs {
        let got = okcode_embed::golden_call(f, &c.args);
        let d = match (&c.out, &c.err, got) {
            (Some(w), _, Ok(g)) => first_diff(w, &g, "$"),
            (None, Some(_), Err(_)) => None,
            (None, Some(e), Ok(g)) => Some(format!("want err {e}, got {g}")),
            (_, _, Err(e)) => Some(format!("err {e}")),
            (None, None, Ok(_)) => None,
        };
        match d { None => pass += 1, Some(d) => diffs.push(d) }
    }
    println!("golden {f}: {pass}/{}", cs.len());
    for d in diffs.iter().take(5) { println!("  {d}"); }
    assert_eq!(pass, cs.len());
}

#[test] fn endpoint_of() { run("identity.endpointOf") }
#[test] fn find_pipeline() { run("identity.findPipeline") }
#[test] fn identity_of() { run("identity.identityOf") }
#[test] fn is_ours() { run("identity.isOurs") }
#[test] fn parts_of() { run("identity.partsOf") }
#[test] fn pipeline_name() { run("identity.pipelineName") }
#[test] fn short_hash() { run("identity.shortHash") }
