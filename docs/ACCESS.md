# The access facade

okcode never touches a filesystem or opens a connection itself. Every byte in or out of a workspace goes through an **access facade** — an object the host supplies when it adds a workspace. The facade is the whole contract between okcode and "where the files are".

okcode ships four implementations (`okcode.access.localFs`, `okcode.access.shell`, and the read-only `okcode.access.fn` and `okcode.access.memory`); anything else — a remote API, an in-memory fixture — implements the same shape.

## Conventions

- **Async.** Every method returns a Promise. (The brain's original access layer was synchronous; remote transports are not.)
- **Batched.** Methods that take paths take many — one round trip per call, not per file. A remote facade must not turn a batch into N round trips if it can avoid it.
- **Paths are workspace-relative**, `/`-separated, no leading `/`, never containing `..` segments (`src/loop.js`). The facade maps them to its root. A facade MUST reject a path that escapes its root. Rejection is lexical (`..`, absolute, NUL) and follows the facade's path **dialect** (`checkPath`, exposed on the facade): on a POSIX facade (localFs off Windows, the bash shell) backslash and `X:` are ordinary filename characters, so `d:\x.html` is a valid relative name; on a Windows facade (PowerShell) drive-letter, rooted and UNC forms are absolute and `\` also separates. A name the scan cannot address is skipped with a scan warning (`stats().skipped`, `status()` `warnings`), never a failed workspace. A symlinked directory inside the workspace is followed.
- **Bytes, not text.** Content is `Buffer`. Text decoding is okcode's job, so a transport that mangles encodings (console codepages) cannot corrupt a file.
- **Hashes** are uppercase hex SHA-1 of the file's bytes. They are computed **where the file is** (a remote facade hashes remotely — never pulls bytes just to hash them).
- **mtime** is a number, compared only for equality against earlier values from the same facade (any stable unit; it is never interpreted as a date).

## Required methods

### `list({ skip, under, limit }) → [{ path, size, mtime }]`

Every regular file under the root. `skip` is a list of directory names to prune at any depth (default: `node_modules .git .idea dist build coverage .next vendor`); it matches directories only (a _file_ named `dist` is listed). A FIXED list of known non-source dot-directories below the root is also pruned — `PRUNE_DOT_DIRS` in `src/access/common.js` (`.git .hg .svn .okcode .adcode .next .nuxt .svelte-kit .turbo .cache .parcel-cache .venv .tox .mypy_cache .pytest_cache .ruff_cache .gradle .idea .vs .terraform .serverless .yarn .pnpm-store .expo`), mirrored literally in `scripts/bash/list.sh`'s `$dotprune` and `scripts/powershell/list.ps1`'s `$dotPrune` (a shell script cannot `require()` common.js; `test/access-contract.test.js` asserts all three stay in exact agreement) — **not** every dot-directory: one not on this list (`.github/`, `.storybook/`, an `app/.well-known/` route) is walked normally, same as any other directory, and left to `.gitignore` (already honored at the root) to decide. Symlinks are neither followed nor listed. Unreadable entries are skipped, never fatal — one locked file must cost that file, not the listing. No hashing.

- `under` (a workspace-relative directory or file) lists only that subtree, paths still relative to the root. `skip` and the dot-directory prune list apply **below** it, never to its own segments — that is how an explicitly named `node_modules/<pkg>` is reached at all. A file `under` lists that one file (if regular, readable and not a link); a missing one lists nothing. It is confined like every path: `checkPath` lexically, and by **real path** — a symlink on the way that resolves outside the root makes the call reject (`escapes the root`), never followed out. localFs and bash compare real paths (a symlinked directory that stays inside the root, as in a pnpm `node_modules`, is walked); PowerShell refuses any reparse point on the way.
- `limit` stops the walk after that many files; the returned array then carries `truncated: true`. A facade may stop early on the target (bash: `head -z`) or cap after the transfer — either way at most `limit` rows come back.

### `stat(paths, { hash = false }) → Map<path, { size, mtime, hash? } | { missing: true }>`

Metadata for named files; `hash: true` adds the content hash. A path that is absent or not a regular file (directories, broken links) is `{ missing: true }`; with `hash: true`, so is a file that cannot be read. `stat` and `read` follow a symlink to a regular file.

### `read(paths) → Map<path, Buffer>`

Contents of named files. A missing or unreadable file is simply absent from the result.

### `commit(path, bytes, { expectedHash = null, create = false }) → { outcome, hash?, current? , error? }` — optional

The only write. **Optional:** a facade without `commit` is **read-only** — okcode finds, reads, greps and outlines through it as usual, and refuses every edit (`editBatch`, `edit`, `writeAt`, `writeWholeFile`) before resolving anything, with `{ ok: false, readOnly: true, code: 'OKCODE_READ_ONLY', reason }`. An unindexed workspace (`index: 'none'`) is read-only whatever its facade has. Atomic for one file: readers see the old bytes or the new bytes, never a mix.

| call              | behaviour                                                             |
| ----------------- | --------------------------------------------------------------------- |
| `create: true`    | publish only if `path` does not exist; parent directories are created |
| `expectedHash: H` | replace only if the current content hashes to `H` (compare-and-swap)  |

`outcome`:

| outcome         | meaning                                                                                                                                                                    |
| --------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `ok`            | written; `hash` is the hash of what is now on disk                                                                                                                         |
| `stale`         | current content ≠ `expectedHash`; `current` is its hash; nothing written                                                                                                   |
| `exists`        | `create` but the path exists; nothing written                                                                                                                              |
| `missing`       | replace but the path does not exist; nothing written                                                                                                                       |
| `symlink`       | the target is a symlink/reparse point; refused                                                                                                                             |
| `noatomic`      | the facade cannot publish atomically here (e.g. temp and target on different devices); nothing written; `error` may say why (`mkdir`, `write`, `link`, `device`, `rename`) |
| `response-lost` | the transport failed after sending; the write may or may not have happened — okcode re-reads to find out                                                                   |

Replace preserves the target's permission bits. Implementations write a temp file in the target's directory and rename it into place. The shell facade also sends the payload's hash and verifies it on the target before publishing: bytes damaged in transit come back as `response-lost` (`error: 'payload corrupted in transit; nothing was written'`), never as a written file.

## Optional capabilities

okcode checks for these and degrades without them.

### `lock(path, id) → { ok: true, release() } | { ok: false, reason }`

Advisory, cross-process lock around a commit (narrows the window between the compare and the rename against another okcode writer). Without it, `commit`'s compare-and-swap is the only guard. `release()` returns a Promise. The lock records the caller's `{ pid, host, startTime, id }`; a stale lock is broken only when its recorded host is the machine holding the files and its pid is provably dead there — never on elapsed time. A lock recorded by another host is always `locked` (release it manually).

### `exec(command) → { ok, out }`

Run a command in the workspace root (`out` = stdout, or stdout+stderr on failure; PowerShell merges stderr into `out`). Used only for opt-in checks (e.g. `node --check` on the target machine). **A capability, not a default:** a host grants it per workspace; okcode never needs it to find, read or edit.

### `filesContaining(paths, text, { ignoreCase = false }) → Set<path> | null`

A pre-filter for grep: which of `paths` contain the literal `text` (one line, no NUL), decided where the files live so a remote search moves only the files that match. It must return a **superset** of the files okcode's own matcher would accept — byte-exact for a case-sensitive search, ASCII-only case folding for `ignoreCase` (okcode only asks that for an ASCII literal) — or `null` when it cannot answer; `null` makes okcode read every file, so a facade must never answer "none" when it means "don't know". The bash shell facade implements it with a C-locale `grep -l -a -s -F --null` (probed first; GNU, BSD and ugrep); PowerShell and localFs do not (a local read is already cheap).

### `remove(paths) → void`

Delete files (best effort). Not used by edits today; reserved for file deletion verbs.

## The shipped facades

### `okcode.access.fn({ list, read, stat? })` — read-only

A facade over the host's own functions, for material that is not a folder anywhere (the brain's kept reads). `list(opts)` returns `[{ path, size, mtime }]` (it may ignore `opts`); `read(paths)` returns a `Map` or a plain object of `path → Buffer | string`; `stat(paths, { hash })`, when given, answers as the contract's `stat`, and when absent is derived from one `list()` (plus a `read` for hashes, taken here). The facade does the rest of the contract itself: paths are checked (posix dialect, `checkPath` exposed), a listing is confined to `under`, skip names and the fixed `PRUNE_DOT_DIRS` list are pruned below it, `limit` is honoured (`truncated: true`), and text comes back as Buffers. No `commit`, `lock`, `exec` or `remove`.

### `okcode.access.memory(map)` — read-only

`fn` over a `Map<path, { bytes, mtime }>` (`bytes` a Buffer or a string; a bare Buffer or string is taken as `{ bytes, mtime: 0 }`). Read live: entries the host adds or replaces are seen on the next call.

### `okcode.access.localFs(root)`

Plain Node `fs`. Atomic commit = temp file + `rename`; compare-and-swap re-hashes before the rename. `lock` via an exclusive-create lock file.

### `okcode.access.shell({ root, run, dialect = 'bash' | 'powershell' })`

Speaks shell scripts over any transport. `run(script, stdin) → Promise<stdout>` executes `script` with `stdin` on the target and rejects on a non-zero exit. That is the entire transport contract — okcode never learns whether it is local bash, `ssh host bash -s`, `docker exec`, or `kubectl exec`.

- Bytes travel as base64 in both directions; paths and payloads go on stdin, not interpolated into the script.
- One script per method call; batches are one call.
- Works against a stock bash (GNU or BSD userland) or Windows PowerShell — nothing to install on the target.

Example — a remote workspace over OpenSSH:

```js
const { spawn } = require('child_process');
const run = (script, stdin) =>
    new Promise((resolve, reject) => {
        // ssh joins its arguments into one remote command line, so the script must be quoted for the remote shell.
        const p = spawn('ssh', [
            '-o',
            'BatchMode=yes',
            'dev-box',
            `bash --noprofile --norc -c ${okcode.access.shell.quote(script)}`,
        ]);
        let out = '',
            err = '';
        p.stdout.on('data', (d) => (out += d));
        p.stderr.on('data', (d) => (err += d));
        p.on('close', (code) => (code === 0 ? resolve(out) : reject(new Error(err || `exit ${code}`))));
        p.stdin.end(stdin ?? '');
    });
const access = okcode.access.shell({ root: '/srv/api', run });
```

`okcode.access.shell.quote(s)` is POSIX single-quote escaping (`'` → `'\''`) for transports that go through a remote shell's command line. Scripts are static per method — no path or payload is ever interpolated into them (both travel on stdin) — so quoting the script is the only quoting a transport needs.

For many calls, keep the connection warm (OpenSSH `ControlMaster`/`ControlPersist`) — each method call is one round trip.
