# Upgrading from okcode 0.2 to 1.0

okcode 1.0 keeps 0.2's API, CLI and store layout; what changes is what it runs on.

## 1. okdb v3

okcode 1.0 depends on `@kedem/okdb` `^3`. If your host uses okdb itself, move it to v3 first (okdb's own upgrading
notes cover that); npm then shares one okdb between your host and okcode.

- **You pass `db`** (`okcode.open({ db })`): pass your okdb v3 instance, opened as usual. okcode uses it through okdb's
  C bridge — the same instance, the same processors, no second copy of the store in memory. okcode never closes it.
  A v2 instance fails with `OKCODE_OKDB_V3_REQUIRED`.
- **You pass `path`** (`okcode.open({ path })`): okcode opens the store with its own `@kedem/okdb` dependency and closes
  it on `close()`, as before.
- **Stores**: okdb v3 keeps v2's on-disk format, so an existing okcode store opens as is; okcode's own data (files,
  symbols, packages, full-text and embedding pipelines) is unchanged.

If you cannot move to okdb v3 yet, stay on okcode 0.2.

## 2. The native addon

okcode 1.0 is native. The package bundles the addon (and a standalone `okcode` binary) for linux-x64 with glibc 2.28+
(Debian 11+, Ubuntu 20.04+, RHEL 8+); `OKCODE_NATIVE_BINARY` overrides it. On other platforms `require('@kedem/okcode')`
throws `OKCODE_UNSUPPORTED_PLATFORM`. Details: [NATIVE.md](NATIVE.md).

## 3. Things that differ

- **Deep requires**: 1.0.0 keeps the JavaScript under `src/` as 0.2 did; later 1.x releases have it under `lib/`
  (`@kedem/okcode/lib/tools`, and so on). `require('@kedem/okcode')` itself is unchanged.
- **Validation messages** for JavaScript come from oxc, not acorn: a refused edit's diagnostic reads differently; which
  edits are refused does not change. The diagnostic `source` names (`javascript-acorn`, `javascript-node-check`) and
  `extract()`'s `extractor: 'acorn' | 'treesitter-tsx'` keep their 0.2 values.
- **Grammars** are built in: the `web-tree-sitter` optional dependency and the "runtime unavailable" state are gone.
  In `lib/analysis/treesitter`, `preload()` / `ensureLanguage()` only resolve (`ensureLanguage(id)` gives `{ id }`, or
  null for an unknown id), and `LANGUAGES` entries carry only `id`, `exts`, `tsxExts` and the query versions.
- **`analysis` internals** that took live AST or tree-sitter objects (`chunk.walkInner`,
  `treesitter.buildImports`, `lib/edges`' `createEdgeIndex`) throw; `analysis.parseSource` still throws on a syntax
  error but returns a stub AST (`{ type: 'Program', sourceType, body: [] }`) instead of acorn's.
- **`analysis.registerExtension` / `registerExtractor`** apply to `analysis.extract()` and `analysis.validateSource()`
  in the calling process; a workspace's own indexing uses the built-in extractors (okjs included) and does not
  consult them.
- **A failed `open({ path })`** (a profile that cannot be translated, an unreadable persisted profile) now closes the
  store okcode opened, so the open can be retried on the same path.
- **CLI**: the same commands and output; a standalone `okcode` binary is available that needs no Node. `OKCODE_VERBOSE`
  still routes okdb's log to stderr. The standalone binary words a JSON syntax error in an `edit-batch` file
  differently (same prefix and exit code), and reads `0b`/`0o` numeric flag values as invalid (the default applies).
