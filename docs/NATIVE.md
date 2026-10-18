# The native parts

okcode 1.0 is native code behind a thin JavaScript layer. The package carries two binaries per supported platform, and
neither contains okdb.

## The addon — `okcode.node`

`require('@kedem/okcode')` loads `native/<platform>/okcode.node` from the package: okcode's whole core — workspaces,
the store over okdb, the access facades (local files, and the bash/PowerShell scripts of the shell facade), symbol
extraction (a JavaScript parser, tree-sitter grammars and queries for the other languages, compiled in), validation,
okjs analysis (okjs's own tooling in an embedded JavaScript engine), grep, and the model-facing tool renderers. `lib/`
only adapts it to JavaScript: it holds the host's functions (a facade's `run`, an embedder's `embed`, a logger) and
turns results back into Maps, Buffers and Float32Arrays.

- **Platform.** 1.x ships `linux-x64-gnu`: x86-64 Linux with glibc 2.28 or newer (Debian 11+, Ubuntu 20.04+, RHEL 8+).
  On any other platform (macOS, Windows, arm64, musl/Alpine) the `require` throws `OKCODE_UNSUPPORTED_PLATFORM`; on a
  supported platform whose binary is missing from the install it throws `OKCODE_NO_NATIVE` (reinstall).
- **Override.** `OKCODE_NATIVE_BINARY=/path/to/okcode.node` is tried before the bundled file — an addon built for
  another platform, or a local build. A path that does not exist falls through to the bundled one.

## The standalone CLI — `okcode`

`native/<platform>/okcode` is the `okcode` command as one executable: the same commands, flags and output as the Node
CLI (`bin/okcode.js`, which npm links as `okcode`), with no Node runtime needed. It still uses okdb's own native
addon, which it looks for in this order:

1. `$OKDB_NATIVE_BINARY`;
2. from the executable's directory upwards, in every `node_modules`: `@kedem/okdb/native/<platform>/okdb-node.<platform>.node`
   (where `@kedem/okdb` 3 installs it), then okdb's platform-package layouts.

Run in place inside an install (`node_modules/@kedem/okcode/native/linux-x64-gnu/okcode`) it finds the okdb next to
it; copied elsewhere, set `OKDB_NATIVE_BINARY`. Without either it exits with `OKCODE_OKDB_NOT_FOUND` and the paths it tried.

## How okcode reaches okdb

okdb is never compiled into okcode. okdb's `.node` exports a stable C ABI (`okdb_bridge`, a versioned function table),
and okcode makes every okdb call through it:

- **Library mode.** `okcode.open({ db })` attaches to the host's instance: okcode takes the bridge table from that
  instance's own addon (`db._native.bridgeTable()`), so it uses the very okdb image and store the host has open — one
  okdb per process, no second copy of the data in memory. `okcode.open({ path })` does the same on the instance it
  opens with its own `@kedem/okdb` dependency (npm dedupes it with the host's). An okdb without the bridge (v2) is
  refused with `OKCODE_OKDB_V3_REQUIRED`; an incompatible bridge version is refused when okcode attaches.
- **Standalone CLI.** The binary loads the installed okdb addon itself (above) and opens the store through the same
  bridge; no Node is involved.

Upgrading okdb within ^3 therefore needs no okcode rebuild; the bridge's version check settles compatibility.
