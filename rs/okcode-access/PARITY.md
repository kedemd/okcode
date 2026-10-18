# okcode-access parity notes (deliberate divergences from the JS)

- `fn` list sorts by UTF-8 bytes; JS sorts by UTF-16 code units (differs only for astral vs. BMP-high characters).
- Non-UTF-8 filenames/outputs are handled lossily (Strings).
- The `Access` trait is synchronous (JS is async); `Runner` is a sync closure.
- Lock-file JSON keys are written alphabetically (serde_json without preserve_order), not pid, host, startTime, id. Readers are order-insensitive.
- `StatRow` results are a Vec in request order; duplicate request paths are not collapsed as a JS Map would.
- PowerShell dialect is ported (scripts verbatim) but untested: no pwsh here.
- Malformed sizes (parse_stat_records, Shell::list) become 0 via `as u64`; JS yields NaN.
- LocalFs/Shell `exec` on failure with no output returns `Command failed: <command>`; Node's message also appends details.
- A falsy (0/false) lock startTime is not distinguished from absent.
- Walk order of LocalFs follows readdir order (JS order depends on Promise.all completion; also nondeterministic).
- No goldens exist for this crate; `golden_call` returns Err.

## access wrappers (rs/okcode-node/js/src/access, access_api.rs)

- `shell.localRunner`: a non-zero exit used to reject with an Error carrying `.code = exitCode` and message `bin exited N`; the native runner returns only a message string, so the rejection is a plain `Error(message)` with no `.code`.
- `shell.DIALECTS` exposes only `{ pathDialect }` per dialect; the JS object also exposed scripts, frame, hashCase, parseStat, bin and args (now internal to the native crate).
- Every facade rejection goes through `_facade.js:plain`, which drops napi's `code` and `status` properties; only `.message` survives.
- `access.fn` hosts: list/stat `mtime` is coerced to a number (JS side `Number(..) || 0`, native `as_f64` else 0), so a string, BigInt or other non-number mtime becomes a number (0 if not numeric) where the JS facade passed the host value through unchanged.
- Shell/fn facade calls run on dedicated threads (not the libuv pool) so host callouts cannot starve it; localFs uses the pool.
- `access/common` no longer exports `acquireLock`/`selfIdentity` (unused; locking is native).
