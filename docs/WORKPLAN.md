# Work plan

Build order, ownership boundaries, and status. Each phase's modules have one owner; owners do not edit each other's directories. Source of truth for behaviour: [DESIGN.md](DESIGN.md), [ACCESS.md](ACCESS.md), and — for anything ported — the brain's code index (`~/github/brain/src/codeindex/`, tests in `~/github/brain/test/codeindex*.js`, spec `~/github/brain/docs/CODE-INDEX-SPEC.md`).

## Phase A — foundations (parallel)

| work            | owner dir                                              | deliverable                                                                                                                                                                                                                                                                                                                                                      |
| --------------- | ------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **A1 access**   | `src/access/`, `test/access*.test.js`, `test/helpers/` | `localFs`, `shell` (bash + powershell dialects ported from `brain/src/codeindex/shell.js` to the async batched contract, static scripts, args on stdin), `shell.quote`; one contract test suite run against every facade: localFs, shell over local bash, shell over real ssh (throwaway sshd — `test/helpers/sshd.js`; skipped when `/usr/sbin/sshd` is absent) |
| **A2 analysis** | `src/analysis/`, `test/analysis*.test.js`              | pure modules ported from brain: `parse`, `validate`, `text`, `extensions`, `template-balance`, `convention-color`, `ext-okjs` (okjs optional); `packages` rewritten on the async facade (`read(paths)`); no env vars, no brain imports; their unit tests ported                                                                                                  |
| **A3 okdb**     | `../okdb-src`                                          | batch resolvers: `resolveField(type, field, fn, { batch })` so FTS/embeddings resolve many rows per call (one facade `read` per batch)                                                                                                                                                                                                                           |

## Phase B — workspace + store

| work             | owner dir                                                                            | deliverable                                                                                                                                                                                                                                                                                                                                          |
| ---------------- | ------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **B1 workspace** | `src/workspace.js`, `src/store.js`, `test/workspace*.test.js`, `test/store*.test.js` | `brain/src/codeindex/index.js` ported onto the async facade and per-workspace okdb storage (DESIGN §5): no stored content (resolved field), FTS on symbols + files; embeddings pipeline per profile hook; edit semantics unchanged (DESIGN §7); ported tests (`codeindex.js`, `codeindex-freshness.js`) green against `localFs` and `shell`-over-ssh |

## Phase C — surface (parallel)

| work            | owner dir                                                                   | deliverable                                                                                                                                                                      |
| --------------- | --------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **C1 okcode**   | `src/okcode.js`, `src/embedders.js`, `test/okcode*.test.js`                 | `open`, workspaces, embedder profiles (built-in / `apiKey` fn / custom `embed`), management (`status`, `sync`, `reset`, `addEmbedder`/`useEmbedder`/`removeEmbedder`, `compare`) |
| **C2 surfaces** | `src/tools.js`, `bin/okcode.js`, `test/tools*.test.js`, `test/cli*.test.js` | model-facing renderers from `brain/src/codeindex/lookup.js` minus brain; CLI from `brain/scripts/code-index.js` on okcode                                                        |

## Phase D — integration

Manager: end-to-end over ssh with real okdb, then the brain adapter (separate work in `~/github/brain`), then release okdb 2.3 + okcode 0.1 together.

## Phase E — joint release (okdb 2.3.0 + okcode 0.1.0), then the brain

The brain moves onto okcode only AFTER both are published (the brain's own sequencing). Order:

1. **Brain-reported items closed**: okdb vector-view bugs (writer view misses live updates after unload; writer reloaded as reader), and okcode's brain API (precomputed-vector search, embedder identity incl. type + endpoint, sizes/outlines without reading content).
2. **okdb**: full suite green (one file at a time) → merge `fix/embeddings-queue-junk` into `main` (ff) → version `2.3.0` + date the CHANGELOG → `npm run build:release` (+ `verify-dist`) → run okcode's full suite against the built `release/` package (not `src/`) → **owner go-ahead** → `npm run release` (pushes main + tags, publishes npm + public repo).
3. **okcode**: depend on `@kedem/okdb ^2.3.0` from npm (drop `file:../okdb-src`) — done: okdb 2.3.0 is published, and okcode's suite + e2e (Ollama) pass per file against it, the free-tier-bound tests skipping (DESIGN §11) → decide `private`/`license` → version `0.1.0` → **owner go-ahead** → `npm publish --access public`.
4. **brain**: adapter at `/data/okcode` (worker with roles, main passive), delete `src/codeindex/` and its `code` env data; delete `src/vector-views.js` + `snapshotEveryChanges` overrides if okdb 2.3 makes them unnecessary.

## Phase F — languages and resolved edges

Why: symbols exist for JavaScript only (`registerExtractor('javascript', …)` is the one registrant; `.ts`, `.py`, `.go`, `.rs`, … are "searchable, no symbol extractor yet"), and `refs()` is a substring scan (`confidence: 'textual'`). Prior art reviewed: adcode (`~/forgejo/adcode`, Python + tree-sitter + SQLite). Taken from it: only the per-language extraction model and resolved import/call edges. Not taken: its storage, freshness, CLI or symbol-id format (okcode's are stronger on each).

**Approach: one generic tree-sitter extractor, data per language — no per-language code.**

- `web-tree-sitter` (WASM): no native build, runs wherever Node runs. One generic extractor runs a grammar's **`tags.scm`** query (the upstream query GitHub code navigation uses; most grammars ship one) and maps captures to the existing symbol shape: `@definition.{function,method,class,interface,module,constant,…}` + `@name` → `{ name, kind, lineStart, lineEnd, start, end }`; `parent`/`path` (`Class.method`) from range containment; `doc` from `@doc` where the query captures it, else the comment block directly above. `@reference.call` / `@reference.class` → reference sites (F3).
- A language = `{ id, exts, wasm, tags.scm, imports.scm? }`. Adding Go or Rust is a data entry plus fixtures. No AST-walking code is written per language, so the maintenance is upstream's queries. A local `tags.scm` override exists only where upstream's misses something we test for.
- **Packaging:** grammars are optional and lazy. The core keeps acorn and no WASM. Grammars ship in an optional package (like `@kedem/okjs` today). It is built by a script that pins each grammar's version, its `.wasm` and its `tags.scm` (0.5–3 MB per grammar, loaded only when a file of that language is seen). Absent grammar → today's behaviour ("searchable, no symbol extractor").
- **Edit safety for free:** a tree-sitter parse marks syntax errors (`ERROR`/`MISSING` nodes). They become a `syntax` diagnostic in `validate`, and the existing baseline-vs-candidate rule refuses only new ones. So every supported language gets the same guarded edits JS has, with no toolchain on the target.
- JavaScript stays on acorn: it is richer (exports, `module.exports`, `node --check`) and already tested. TypeScript moves to tree-sitter.

| work                     | deliverable |
| ------------------------ | ----------- |
| **F0 spike**             | `web-tree-sitter` current version + TS, Python, Go, Rust grammars: ABI compatibility (grammar `.wasm` must match the runtime's ABI; prebuilt bundles such as `tree-sitter-wasms` may lag), load time, memory per grammar, parse speed on a large repo vs acorn. Go/no-go before F1. |
| **F1 generic extractor** | `src/analysis/treesitter.js`: lazy grammar loading, `tags.scm` → symbols, containment → `path`, doc capture; registered per language via `registerExtractor`; `analyzerVersion` includes grammar + query version, so a grammar upgrade reparses. First set: TypeScript/TSX, Python, Go, Rust. Next by demand: Java, C#, C/C++, Ruby, PHP, Kotlin, Swift. Fixtures per language (adcode's `tests/fixtures` show the shapes worth covering: decorators, nested classes, methods, docstrings, re-exports). |
| **F2 syntax gate**       | `validate`: tree-sitter `ERROR`/`MISSING` → diagnostics for every tree-sitter language; existing `compareDiagnostics` unchanged. |
| **F3 resolved edges**    | **done.** `src/edges.js`: imports resolved to a workspace file (and named symbol when the import names one) — JS/TS relative specifiers + `tsconfig.json` `compilerOptions.paths` (nested configs, one-hop `extends`, JSONC via a string-aware stripper), Python source roots, Go `go.mod` module paths, Rust `crate::`/`super::`/`self::` + `mod` files. Re-exports are their own kind so barrels are followed one level, by both `importers()` and `refs()`. `refs()` keeps the text scan as its universe but **ranks and labels** each hit: `local`, `imported`, `textual`; `call: true` from a stored reference site (tree-sitter `@reference.call`, and a new full acorn call-site walk for JS). New `importers(file)` (reverse imports, surfaced in `code_outline` and as its own CLI command). No new store types: edges derive from the stored `files`/`imports`/`symbols` rows, cached per workspace and invalidated by the existing `generation` counter. See DESIGN §12 for per-language coverage and the conservative gaps. Tests: `test/edges.test.js` (one fixture per language + okjs + cache invalidation). |
| **F4 okjs**              | member symbols, normalized regions (done — F4 items 1–2); component-tag refs (F4 item 3 — **done**, part of F3's `src/edges.js`: a `component`/`component-tag` dependency resolves against a tag → defining-file index, so "who uses `<setting-group>`" is `importers()`, not a grep). |

**okjs follow-ups (F4).** The okjs extension's shape is right: optional peer, fail-soft fallback to acorn, analyser-sourced dependencies, regions for piecewise outline/edit, and the template-balance gate born from a measured failure. Three gaps:

1. **One symbol per component.** With okjs installed, a `.ok.js` file's only symbol is the tag (`setting-group.ok.js`, 88 lines, 22 named members → 1 symbol). The extension outranks acorn instead of adding to it, so `find('rows')` can only reach a component member by grep/FTS. Fix: run the generic extractor too and nest its symbols under the component (`setting-group.rows`); the extension contributes the tag + regions on top.
2. **The okjs region shape leaks into the workspace.** `workspace.js` knows `source: 'definition'|'block'`, `contentRange`/`valueRange`, `role` and `okAnalysis`, despite `ext-okjs.js` promising "everything OKJS-specific lives here". Fix: extensions return normalized regions (`{ name, kind, lineStart, lineEnd, doc }`) and an opaque `analysis`; carving stays generic.
3. **Component usage.** The analyser's `component-tag` dependencies make "who uses `<setting-group>`" a resolved ref (F3), not a grep for the tag string.

Also check `.ok` → `html` in `LANGS`: okjs's docs only use `.ok.js` / `.ok.html`.

## Phase G (later) — grammars on demand

v1 ships every grammar inside the package (`grammars/`, ~16 MB) — simple, and fine for today's hosts. Later, when okcode has users who want a light install or the set grows past ~50 MB:

- Each language's grammar (`.wasm` + queries + LICENSE) is published on its own — a GitHub release asset or a small per-language npm package — and is no longer in okcode's tarball.
- okcode's `grammars/manifest.json` pins, per language, the version, the URL and the **SHA-256** of each file. On the first file of a language, okcode fetches it, verifies the hash, and caches it (per user or per store); a file that fails verification is never loaded. Only bytes okcode shipped knowing about ever run.
- Offline, unreachable or not yet fetched → exactly today's behaviour without that grammar ("searchable, no symbol extractor"), and a status line says which languages are pending. A host that must not fetch at runtime pre-installs them (`okcode grammars install [lang…]`) or points `grammarsDir` at its own copy.
- Grammar upgrades bump the manifest; the existing analyzer-version rule reparses affected rows.

## Rules for every owner

- Node ≥ 20, CommonJS, `node:test` + `node:assert/strict`, no new dependencies without the manager.
- `@kedem/okdb` is the published `^2.3.0` from npm (a production build: free-tier limits enforced). If okdb misbehaves, write a minimal repro and report it — do not patch okdb from okcode work.
- Keep the brain's hard-won comments where the code they explain moves with it; drop brain-only context.
- Tests must be hermetic (temp dirs, throwaway sshd), run per file: `node --test --test-force-exit test/<file>`.
- Do not commit; the manager reviews and commits.

## Known gaps

- A named function assigned to `module.exports` (`module.exports = function createX(…)`) is found as `module.exports`, not by its name.
- Methods of an anonymous `export default { … }` object are not extracted as symbols (the file exports `default` only).
- Symbols for JavaScript only; `refs()` is textual; component members of `.ok.js` files are not symbols — Phase F.
- Licensing is done (okdb `db.licenses` + `OKDB_LICENSE_FILE`; okcode `open({ path, license })`, `status().license`, CLI `--license`) — DESIGN §5. okcode has no activate call of its own: a node-bound license is activated with `oc.db.licenses.activate(token)` on the live store, or by reopening with `{ blob, activation }`.

## Status

| phase                                                                                                                                          | status                                                                                                                                                                                                                          |
| ---------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| docs (README, DESIGN, ACCESS, WORKPLAN)                                                                                                        | done                                                                                                                                                                                                                            |
| A1 access                                                                                                                                      | done (48 tests: localFs, shell/local bash, shell/real ssh; PowerShell + BSD fallbacks ported, unexercised — no pwsh/BSD here)                                                                                                   |
| A2 analysis                                                                                                                                    | done — `0b25c28` (39 tests; okjs present + absent)                                                                                                                                                                              |
| A3 okdb batch resolvers                                                                                                                        | done — okdb `8ed63a5` (`resolveField(…, { batch, batchSize })`, embeddings prefetch)                                                                                                                                            |
| B1 workspace + store                                                                                                                           | done (79 tests; workspace suites on localFs + real ssh; no-content proof; found okdb d195f80)                                                                                                                                   |
| C1 okcode                                                                                                                                      | done (11 tests incl. passive cross-process status; secrets-never-on-disk proof)                                                                                                                                                 |
| C2 surfaces                                                                                                                                    | done (tools 15, CLI 10)                                                                                                                                                                                                         |
| okdb fixes from okcode (FTS nested txn, pipeline cleanup, cross-process rebuild, resolver-before-open, factory getter)                         | done in okdb-src; okcode workarounds removed (`6f1a633`)                                                                                                                                                                        |
| D integration                                                                                                                                  | okcode↔okdb e2e green (`test/e2e-ollama.test.js`: real qwen3-embedding, local + ssh, find/grep/ask/edit/stale/reset); brain adapter next                                                                                        |
| brain-facing API: query by precomputed vector, identity-named stores (type, endpoint, model, dims) + orphans, cheap `files`/`symbols` listings | done (`test/okcode-identity`, `test/workspace-listing` over localFs + ssh, vector cases in `test/store-embeddings`)                                                                                                             |
| Phase F: F0 spike, F1 generic extractor, F2 syntax gate, F3 resolved edges, F4.1/F4.2/F4.3 okjs                                                | done (`test/analysis-treesitter`, `test/workspace-treesitter`, `test/edges`; grammars/<lang>/{tags,imports}.scm per language)                                                                                                  |
| E okcode on published okdb                                                                                                                     | `@kedem/okdb ^2.3.0` from npm (real install, not a link); suite green per file on the prod build — free-tier-bound tests skip (2 need 3 profiles in one workspace; 3 need a test-signed license); `private`/`license` undecided |
