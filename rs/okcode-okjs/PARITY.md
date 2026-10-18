# okcode-okjs parity

Golden: `extract` cases whose path matches `.ok.js|.ok.mjs|.ok.html` (9 recorded).

- Compared on a PROJECTION: parse.js-owned fields (`lang`, `lines`, `indexed`, `analyzerVersion`, `reason`) are
  dropped, and the generic acorn extractor's symbols (nested under the component and top-level helpers) are
  dropped from `symbols`, leaving only `kind: "component"`. They come from okcode-js; `analyze_with_generic`
  takes them through a callback and ports `nestComponentMembers`/shift logic, untested here.
- 3 of the 9 cases were recorded with the okjs tooling unavailable (`analyzerVersion "0:0"`, "okjs tooling
  unavailable (not installed)"): the JS generic fallback path. They are skipped, not passed.
- okjs's analyser runs unchanged in QuickJS. QuickJS has no `URL`, so `src/js/prelude.js` is a small URL
  polyfill (hierarchical URLs only: `new URL(spec, base).href`, validity of absolute specifiers). Exotic URL
  edge cases (IDNA hosts, ports normalisation, percent-encoding subtleties) are not reproduced.
- Embedded sources are read from `node_modules` at build time (`include_str!`), versions @kedem/okjs 1.3.1, acorn as installed.
- Error messages from a throwing analyser come from QuickJS, not V8.
