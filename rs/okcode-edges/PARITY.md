# Parity notes

- packages.readPackages (8) and packages.scanPackages (6): the recorded `out` is `{}` (the JS functions are async, so the
  recorder serialised a Promise) and `access` is a mock/localFs descriptor. `golden_call` returns `{}` for them, so these
  14 "passes" are vacuous. The real Rust ports (`packages::read_packages`, `scan_packages`) are untested.
- `read_packages` takes an `extract_exports` callback in place of parse.js `extract(...).exports` (TODO(wave2)).
- scan_packages' "package.json does not parse" reason carries serde's message, not V8's.
- JS multiline `^` also matches after \r,  ,  ; the Rust regexes only after \n.
- packages.readPackages / scanPackages are skipped in tests/golden.rs (golden_call returns Err); real coverage is tests/packages.rs.
- from_manifest: non-string truthy version/main/module/types are dropped (JS keeps the raw value); description of an array/object is "[object]" rather than JS String(x). A non-object manifest (e.g. `null`) yields a package where JS throws.
- Edges: the re-export pass clones `direct` (JS iterates a live array; differs only when e.to == rel); Edge `kind` stays Some("") where JS gives null; JS Object.entries orders integer-like tsconfig `paths`/psr-4 keys first, Rust keeps source order; psr-4 given as an array is ignored (JS yields index keys); `$` patterns in a tsconfig paths target are not interpreted by replacen; include_dirs sorts by bytes, JS by UTF-16 units.
