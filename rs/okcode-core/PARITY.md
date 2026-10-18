# okcode-core / okcode-node parity notes

Per slice: JS tests (or parts) the native port deliberately does not match, with the reason.

## analysis (architect)
- Composed `extract` golden: 965/980. Remaining diffs are wave 1 engine differences (oxc vs acorn
  signature text for some JSX exports, tree-sitter python/yaml query details) — see the wave 1 crates' PARITY.md.

## okdb (slice)
- One `Okdb` per canonical path per process: a second `new OKDB(samePath, opts)` shares the first host; its role / processors / engines / auth options are ignored (the first instance's win).
- Values: a JS `Set` is stored as an array (no Set variant in the value model); typed arrays (Float32Array) and RegExp are stored as plain arrays / their JSON form, not msgpackr extension values.
- Sync shim jobs (env.get, fts.list, licenses.list, embedderCall) are served by the owner thread while it waits on a JS embedder, inside the outer okdb_core call. Only call paths that do not hold a RefCell borrow across the embedder callback are safe; they can overtake queued async jobs.
- `events.on('log')` is accepted and delivers nothing; native event subscriptions are not removed on off()/close(). `fts.indexer().flush` is NOT_IMPLEMENTED_NATIVE (store slice).
- `getEmbedderSchema` reads the native schema; `_ensureTypeEnv` resolves to the env name.

## api (slice)

- INTENDED behaviour change: `Okcode::open` closes an owned db when a host profile fails to translate or a persisted-profile read fails (okcode.rs open -> open_inner error path). src/okcode.js leaves the db open in those cases (only the meta-env failure and an unknown `active` close it). Reason: the owned db is created inside `open`, so the caller has no handle to close after the rejection; leaving it open leaks the environment lock and the next `open` on the same path deadlocks. Closing is the only way the failed open is retryable.
- ws.ask / okcodeToolWs are tied to the (ws, store) handles they were built for (okcodeWsAsk), as the JS closure is tied to its ws; a stale ws after remove / re-add / reset-all asks its own closed workspace.
- Error text for non-string values uses okcode::js_string (JS String()), not identity::js_string (which prints objects as JSON).
## perf (slice)
- Saved-file warnings (non-string symbol names) are logged once, natively; JS logs them in the batch attempt and again in the file-by-file retry. Intended difference (log lines only).
- A row the store cannot encode (a cycle) is logged by js/store.js synchronously, before saveFiles settles, and flagged `__logged` so native does not log it again; with no log passed to wrapStore (native-opened okcode stores) native logs it.
- ensure_fts builds the index in every process, draining or not, as okdb-node's `fts.ensure` does (its `_build` ignores the processor role); only waiting for `ready` is limited to draining processes.

## pure-ts (slice: analysis/treesitter.js, analysis/validate.js)
- acorn vs oxc: JS syntax-error message wording and positions can differ (tests asserting acorn text such as `Unexpected token`/`Unterminated` variants); `node --check` is emulated with oxc under node's goals (okcode-js/PARITY.md).
- `treesitter._resetForTest({ loader })` is a no-op: the grammars are compiled in, so the test "runtime unavailable (injected loader)" (test/analysis-treesitter.test.js) cannot simulate a missing web-tree-sitter runtime.
- `treesitter.buildImports(src, tree, query)` takes a live JS tree and query; it cannot cross and throws. Nothing in src/ or the tests outside treesitter.js calls it (extractFor/extractJsxFallback build imports natively).
- `syntaxDiagnostics`/`extractFor`/`jsxSyntaxDiagnostics` do not need a prior preload()/ensureLanguage() (always loaded), so the JS "[] until preloaded" behaviour does not exist; `jsxGrammarAvailable()` is always true; `currentVersionFor` never carries the `:none` suffix.
- `ensureLanguage(id)` resolves to `{ id }` (JS: the grammar object) or null for an unknown id; `LANGUAGES` carries only id/exts/tsxExts/queryVersion/importQueryVersion (no wasm names, comment/decorator tables).
- `compareDiagnostics`: the `mapOffset` closure is evaluated in JS once per baseline diagnostic (including ones inside a target, which JS skips), the matching is native.

## C1/C2 thin wrappers (release plan; OKCODE_NATIVE_SRC=1)

- **pure-ts: test/analysis-treesitter "runtime unavailable (injected loader)".** The test replaces the JS web-tree-sitter loader through `_resetForTest({ loader })` to simulate a missing runtime. The Rust build compiles its grammars in, so there is no runtime or loader to inject, and `_resetForTest` only resets the JS registry. By design. It is the only test failure with every wrapper on: 579 test runs pass.
- **pure-text: `chunk.walkInner`** takes a live acorn AST node, which can't cross as JSON. The wrapper's export throws. Only the old JS chunker used it.
- **pure-text: `String()` of null or object arguments.** A few pure exports' string coercion differs from JS `String()` for those inputs: `slug(null)` gives '' instead of 'null', and objects give JSON instead of '[object Object]'. No caller passes them.
