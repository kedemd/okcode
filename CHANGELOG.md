# Changelog

## [Unreleased]

**Changes**

- Requires `@kedem/okdb` ^3.0.1, which fixes the FTS registration that a removed environment left behind. okcode's
  workaround for it in 1.0.1 (dropping the stale index when a recreated environment's `fts.ensure` was refused) is
  removed; `oc.reset(id, { scope: 'all' })` keeps full-text search on through okdb itself.

## [1.0.1] - 2026-10-19

**Fixes**

- `analysis.registerExtension` / `registerExtractor` drive workspace indexing again, as in 0.2: a textual path a host
  extension or extractor claims is indexed (found, outlined, re-analysed on a version change) through it. 1.0.0 used
  them only in `analysis.extract()`.
- `oc.reset(id, { scope: 'all' })` no longer leaves full-text search off for the rest of the process (okdb 3.0.0
  kept the removed environment's FTS registration; okcode now clears it when it recreates the environment).

**Changes**

- The package's JavaScript moved from `src/` to `lib/` (`main` is `lib/okcode.js`). `require('@kedem/okcode')` is
  unchanged; deep requires change path (`@kedem/okcode/src/tools` is now `@kedem/okcode/lib/tools`).
- The bash/PowerShell access scripts are no longer shipped as files: they were already compiled into the native addon.
- New [docs/NATIVE.md](docs/NATIVE.md): the addon, the standalone binary, and how okcode reaches okdb through its C
  bridge. The README documents every export and CLI command; the docs are corrected for 1.0's behaviour.
- Repository: the Cargo workspace is at the root with the crates in `crates/`; the JS-era leftovers (vendored `.wasm`
  grammars, the grammar vendoring script, the JS benchmark, the recorded test corpus) are gone.

## [1.0.0] - 2026-10-18

okcode 1.0 is a native implementation of okcode 0.2: the same JavaScript API (`require('@kedem/okcode')`, `open`,
`access`, `analysis`, the tools), the same CLI, the same store layout — rewritten in Rust, with the JavaScript in `src/`
reduced to thin wrappers over the native addon.

**Breaking**

- Requires `@kedem/okdb` **^3** (was ^2). A host that passes `db` must pass a v3 instance; okcode uses that very
  instance through okdb's C bridge (it never opens a second one). A v2 `db` is refused with `OKCODE_OKDB_V3_REQUIRED`.
  Hosts on okdb v2 stay on okcode 0.2. See [docs/UPGRADING.md](docs/UPGRADING.md).
- A native addon is required: bundled in the package for linux-x64 (glibc 2.28+); other platforms fail with `OKCODE_UNSUPPORTED_PLATFORM`.
- Dropped dependencies: `acorn`, `web-tree-sitter` (and the vendored `.wasm` grammars in the package).

**Changed**

- JavaScript is parsed and validated with oxc instead of acorn / `node --check`: syntax-error messages are worded
  differently (the edit gate's rule — refuse only a *new* diagnostic — is unchanged).
- tree-sitter grammars are compiled in: no optional runtime, no "runtime unavailable" degradation.
- A standalone `okcode` binary (no Node) does what `bin/okcode.js` does, with the same output.
- `analysis` helpers that took live JavaScript AST or tree-sitter objects are not available
  (`chunk.walkInner`, `treesitter.buildImports`); `analysis.parseSource` still throws on a syntax error
  but returns a stub AST (`{ type: 'Program', sourceType, body: [] }`).
- A failed `open({ path })` closes the store it opened, so it can be retried. The full list of differences:
  [docs/UPGRADING.md §3](docs/UPGRADING.md#3-things-that-differ).

**Performance** (a 578-file TypeScript application; okcode 1.0 on okdb 3.0.0 vs okcode 0.2 on okdb 2.6.4; median of 5,
linux-x64, a shared machine at load 6–8 — CPU time and peak RSS are the fair figures)

| | okcode 0.2 | okcode 1.0 |
|---|---|---|
| cold index, full-text content: CPU / wall / peak RSS | 7.98 s / 7.44 s / 466 MB | 5.47 s / 3.96 s / 309 MB |
| cold index, symbols only: CPU / wall / peak RSS | 4.54 s / 4.30 s / 349 MB | 4.07 s / 2.48 s / 284 MB |
| warm reopen: CPU / wall / peak RSS | 0.56 s / 0.43 s / 153 MB | 0.34 s / 0.36 s / 182 MB |
| idle (open, nothing to do): CPU per minute | 51–138 ms | 32 ms |

Sizes (linux-x64, stripped): the addon 24.1 MB (5.2 MB gzipped), the standalone CLI 23.3 MB (4.9 MB gzipped); okdb's own
native package is shared with the host.

## [0.2.0]

The JavaScript implementation (last release on okdb v2).
