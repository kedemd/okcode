# okcode-cli parity notes (Rust `okcode` vs bin/okcode.js)

Checked by `node rs/golden/cli-diff.js <binary>` (57 argv cases, both CLIs on their own fixture copy) and by
`OKCODE_CLI_BIN=<binary> node --test test/cli.test.js` (11/11).

Known differences, by design or waiting on okdb.

Resolved: `license.enforced` now matches JS (`true`). okdb-core builds with `licensed-build` (rs/Cargo.toml), like the published `@kedem/okdb`, so cli-diff.js no longer normalises it. With enforcement on, the free tier's `pipelinesPerEnv: 2` applies. test/okcode-embedders.test.js then skips its two 3-profile cases, as it does against the published bundle.


1. *(resolved)* **`OKCODE_VERBOSE`** routes okdb's own log to stderr through the bridge's open option `{log: {console: 'stderr'}}` (off otherwise), as bin/okcode.js does with `db.log.attach`.
2. **A JSON syntax error in an `edit-batch` file** prints serde's wording (e.g. `EOF while parsing a value at line 1 column 8`), not V8's (`Unexpected end of JSON input`). The `edit-batch: <file>: ` prefix and exit 1 are the same.
3. **Number() parsing of flag values** (`--limit`, `--offset`, read-at line numbers) covers decimal, hex, Infinity, empty strings and bare flags. JS also accepts `0b`/`0o` prefixes and surrounding Unicode whitespace; here those read as NaN and fall back to the default.
