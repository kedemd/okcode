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
3. **okcode**: depend on `@kedem/okdb ^2.3.0` from npm (drop `file:../okdb-src`) → decide `private`/`license` → version `0.1.0` → full suite + e2e (Ollama) → **owner go-ahead** → `npm publish --access public`.
4. **brain**: adapter at `/data/okcode` (worker with roles, main passive), delete `src/codeindex/` and its `code` env data; delete `src/vector-views.js` + `snapshotEveryChanges` overrides if okdb 2.3 makes them unnecessary.

## Rules for every owner

- Node ≥ 20, CommonJS, `node:test` + `node:assert/strict`, no new dependencies without the manager.
- `@kedem/okdb` resolves to `../okdb-src` (unpublished 2.3). If okdb misbehaves, write a minimal repro and report it — do not patch okdb from okcode work.
- Keep the brain's hard-won comments where the code they explain moves with it; drop brain-only context.
- Tests must be hermetic (temp dirs, throwaway sshd), run per file: `node --test --test-force-exit test/<file>`.
- Do not commit; the manager reviews and commits.

## Known gaps

- A named function assigned to `module.exports` (`module.exports = function createX(…)`) is found as `module.exports`, not by its name.
- Methods of an anonymous `export default { … }` object are not extracted as symbols (the file exports `default` only).

## Status

| phase                                                                                                                  | status                                                                                                                                   |
| ---------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
| docs (README, DESIGN, ACCESS, WORKPLAN)                                                                                | done                                                                                                                                     |
| A1 access                                                                                                              | done (48 tests: localFs, shell/local bash, shell/real ssh; PowerShell + BSD fallbacks ported, unexercised — no pwsh/BSD here)            |
| A2 analysis                                                                                                            | done — `0b25c28` (39 tests; okjs present + absent)                                                                                       |
| A3 okdb batch resolvers                                                                                                | done — okdb `8ed63a5` (`resolveField(…, { batch, batchSize })`, embeddings prefetch)                                                     |
| B1 workspace + store                                                                                                   | done (79 tests; workspace suites on localFs + real ssh; no-content proof; found okdb d195f80)                                            |
| C1 okcode                                                                                                              | done (11 tests incl. passive cross-process status; secrets-never-on-disk proof)                                                          |
| C2 surfaces                                                                                                            | done (tools 15, CLI 10)                                                                                                                  |
| okdb fixes from okcode (FTS nested txn, pipeline cleanup, cross-process rebuild, resolver-before-open, factory getter) | done in okdb-src; okcode workarounds removed (`6f1a633`)                                                                                 |
| D integration                                                                                                          | okcode↔okdb e2e green (`test/e2e-ollama.test.js`: real qwen3-embedding, local + ssh, find/grep/ask/edit/stale/reset); brain adapter next |
