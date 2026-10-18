# okcode

Agentless workspace tools for code: **find, read and edit code by name** in local or remote workspaces. Part of the ok family — [okjs](https://www.npmjs.com/package/@kedem/okjs) (UI), [okdb](https://www.npmjs.com/package/@kedem/okdb) (data), okcode (working on code).

- **Find** — symbols by name, full-text over names/doc prose and over file content, semantic search (embeddings), references, outlines, dependency packages.
- **Read** — a symbol, a range, a whole file; every read hands back an `at` token (the file's hash).
- **Edit** — by symbol or range, guarded by `at`: a stale edit is refused, never rebased. Candidates are validated locally before the write (a change that introduces a new diagnostic is refused); the write is an atomic compare-and-swap on the target.
- **Anywhere** — okcode never opens a connection. You hand it an **access facade** (list / stat / read / commit). okcode ships a local filesystem facade and a `shell` facade that speaks bash/PowerShell over any `run(script, stdin)` — so `ssh`, `docker exec`, `kubectl exec` reach a remote workspace with **nothing installed on it** — plus two read-only ones over the host's own functions or an in-memory map.
- **No copies** — the index (symbols, full-text postings, vectors) lives in okdb; file content is never stored. okdb reads it through the facade when indexing and when showing a result ([resolved fields](https://github.com/kedemd/okdb/blob/main/docs/resolved-fields.md)).

> Status: **1.0.0**. okcode 1.0 is a native (Rust) implementation behind the same JavaScript API as 0.2; it requires
> [`@kedem/okdb`](https://www.npmjs.com/package/@kedem/okdb) `^3` and talks to okdb through okdb's stable C bridge, sharing
> the host's okdb instance instead of opening its own. Upgrading from 0.2: [docs/UPGRADING.md](docs/UPGRADING.md). What changed
> and by how much: [CHANGELOG.md](CHANGELOG.md).

## Requirements

- Node.js ≥ 20 (for the library and the `okcode` command), or no Node at all for the standalone native `okcode` binary.
- `@kedem/okdb` ^3 (a normal dependency; npm dedupes it with your own). A host that passes its own `db` must pass an
  okdb **v3** instance; a v2 `db` is refused (`OKCODE_OKDB_V3_REQUIRED`) — v2 hosts stay on okcode 0.2.
- linux-x64 with glibc 2.28+ (Debian 11+, Ubuntu 20.04+, RHEL 8+), the one platform 1.0.0 bundles a native addon for.
  Elsewhere `require('@kedem/okcode')` throws `OKCODE_UNSUPPORTED_PLATFORM`. See [docs/NATIVE.md](docs/NATIVE.md).

## Quick look

```js
const okcode = require('@kedem/okcode');

const oc = await okcode.open({
    path: '/var/lib/okcode', // okcode's own okdb store (or pass `db`)
    embedders: { qwen: { type: 'ollama', model: 'qwen3-embedding:0.6b' } },
    active: 'qwen',
});

// A local folder
await oc.addWorkspace('app', { access: okcode.access.localFs('/work/app') });

// A huge repo you only ever ask by name: skip the (bigger) content index
await oc.addWorkspace('monorepo', { access: okcode.access.localFs('/work/monorepo'), fts: 'symbols' });

// A remote folder — okcode supplies the scripts, you supply the transport
await oc.addWorkspace('prod-api', {
    access: okcode.access.shell({
        dialect: 'bash',
        root: '/srv/api',
        run: (script, stdin) => mySsh.run(script, stdin),
    }),
});

const ws = oc.workspace('app');
await ws.sync();
const hits = await ws.find('retryWithBackoff');
const { body, at } = await ws.read('retryWithBackoff');
await ws.edit('retryWithBackoff', newBody, { at });

// Semantic search; or with a vector the host already embedded (no embed call),
// once its identity — [type, endpoint, model, dims] — matches the profile's
const { identity } = oc.embedders().find((e) => e.active);
await ws.ask('where do we retry failed requests');
await ws.ask({ text: 'where do we retry failed requests', vector, identity });

// Listings from the index alone — no file content is read
await ws.files({ dir: 'src' }); // sizes, lines, languages, symbol counts
await ws.symbols('src/http.js'); // the stored symbol table of one file

await oc.close();
```

`mySsh`, `newBody` and `vector` are the host's: a transport, the replacement text, and the question embedded in the
profile's space (a `Float32Array` or a number array).

CLI — run it in a folder; the index lives in `.okcode/` there (workspace `default`), created and kept current automatically:

```sh
okcode find retryWithBackoff
okcode read retryWithBackoff            # prints the code; at=<hash> on stderr
okcode edit retryWithBackoff --file body.txt --at <hash>
okcode status
okcode find retry --store /data/okcode --id api   # a shared store holding many workspaces
```

**Licensing.** Without a license, okdb's free tier covers about three workspaces (5 envs, 2 embeddings pipelines per env). A license goes into okcode's store with `okcode.open({ path, license })` (idempotent — pass it on every open), `okcode --license FILE`, or `OKDB_LICENSE_FILE=FILE` for any process; `status().license` shows what is in effect. A host that passes its own `db` licenses it itself. See [DESIGN §5](docs/DESIGN.md#5-storage).

## API

`require('@kedem/okcode')` is `{ open, access, analysis }`. Behaviour and result shapes: [DESIGN §9](docs/DESIGN.md#9-management-surface).

**`okcode.open(options)` → `oc`**

| option      | meaning                                                                                                     |
| ----------- | ----------------------------------------------------------------------------------------------------------- |
| `path`      | okcode's own okdb store, opened (and closed on `close()`) by okcode. Pass `path` or `db`, not both          |
| `db`        | an open okdb v3 instance the host owns; okcode never closes it                                              |
| `role`      | with `path`: okdb's process roles for that store, `{ processors, engines, compaction }`                     |
| `license`   | with `path`: an okdb license (text, or `{ blob, activation }`), installed first ([DESIGN §5](docs/DESIGN.md#5-storage)) |
| `embedders` | named embedder profiles ([DESIGN §8](docs/DESIGN.md#8-embedders--profiles)); `active` names the one `ask()` uses |
| `workspace` | defaults for workspaces that don't say, e.g. `{ fts: 'symbols' }`                                           |
| `log`       | a logger (`{ info, warn, error, debug }`) or one function; okcode's warnings go to `warn`                   |

**`oc`** — `addWorkspace(id, { access, index, fts, options })` (resolves to the workspace), `workspace(id)`, `workspaces()`,
`removeWorkspace(id)`, `sync(id?, { force })`, `status(id?)`, `reset(id, { scope })`, `embedders()`, `active`,
`addEmbedder(name, cfg)`, `useEmbedder(name)`, `removeEmbedder(name)`, `removeOrphaned(id?)`, `compare(id, query, names?)`,
`close()`; `db` (the okdb instance) and `meta` (okcode's own `okcode` env). `addWorkspace`'s `options` tune one workspace:
`skip` (directory names to prune; default `okcode.access.DEFAULT_SKIP`), `textMax`, `libraryFiles`, `libraryParseFiles`,
`libraryScans`.

**A workspace** (`ws`) — properties `id`, `access`, `index` (`'full'` | `'none'`), `readOnly`, `store`; methods:

| group   | methods                                                                                                                       |
| ------- | ----------------------------------------------------------------------------------------------------------------------------- |
| look up | `find(q, { kind, path, limit, offset })`, `ask(q, { profile, path, limit })`, `refs(q, { limit, offset })`, `importers(file)`, `mentions(text)`, `grep(pattern, opts)`, `glob(patterns)`, `outline(file)`, `structure({ dir })`, `files({ dir, lang, stat })`, `symbols(file, { kind, stat })`, `packages({ refresh })`, `package(name)`, `isLibraryPath(rel)` |
| read    | `read(symbol \| file#name \| file \| file:a-b)`, `readFile(file, { maxLines })`, `readAt(file, a, b, at?)` — results carry `body` and `at` |
| edit    | `edit(target, body, { at })`, `editBatch({ at, edits: [{ target, body }] })`, `writeAt(file, a, b, body, { at })`, `writeWholeFile(file, text, { create \| expectedHash })`, `syntaxCheck(file)` (`node --check` on the target, through the facade's `exec`) |
| manage  | `sync({ force })`, `refresh()`, `flush()`, `stats()`, `scanWarnings()`, `close()`                                              |

`ws.store` (null when `index: 'none'`) is the workspace's okdb side: `envName`, `env` (the okdb env), `ask(q, { profile, limit })`
(one profile's raw vector hits), `search(q, limit, mode)` (the symbol full-text index), `profiles()`, and `settle({ profile })`,
which resolves once that profile's embeddings have caught up.

**`okcode.access`** — `localFs(root)`, `shell({ root, run, dialect })` (with `shell.quote(s)` and `shell.localRunner(dialect)`,
a `run` for a local shell), `fn({ list, read, stat? })`, `memory(map)`, and the `DEFAULT_SKIP` / `PRUNE_DOT_DIRS` lists.
Contract: [docs/ACCESS.md](docs/ACCESS.md).

**`okcode.analysis`** — the pure functions on text the workspace is built on: language detection (`langOf`, `extOf`,
`isTextual`, `isSecret`), symbol extraction (`extract`, `parseSource`), validation (`validateSource`, `compareDiagnostics`),
document sections (`regions`, `sectionOf`), text/EOL/diff utilities (`decode`, `normaliseEol`, `hunksFor`, …), template
balance, convention colours, okjs, and dependency surfaces (`scanPackages`, `readPackages`). See [DESIGN §2](docs/DESIGN.md#2-layers).

**Model-facing tools** — `require('@kedem/okcode/lib/tools').createTools({ workspace: async (hint) => ws, workspaces })` →
`{ schemas(), render(name, args), TOOLS }` for `code_map`, `code_find`, `code_grep`, `code_glob`, `code_outline`,
`code_read`, `code_package`, `code_ask`, `code_edit`, `code_write`. `render` resolves to text and never throws.

## CLI

`okcode <command> [args] [--root DIR] [--id NAME] [--store DIR] [--json]`; `okcode help <command>` describes one command.

- look up: `structure`, `find`, `refs`, `importers`, `grep`, `glob`, `outline`, `read`, `read-at`, `packages`, `ask`
- edit: `edit`, `edit-batch`, `write`, `check`
- manage: `status`, `sync [--force] [--no-wait]`, `reset --scope vectors|fts|all`, `embedders`, `workspaces`, `remove-workspace`, `stats`
- flags: `--root`, `--store`, `--id`, `--fts full|symbols`, `--embedder-config FILE`, `--license FILE`, `--json`
- environment: `OKCODE_VERBOSE=1` routes okdb's log to stderr; `OKDB_LICENSE_FILE` installs a license on every open.

The same commands run without Node from the standalone binary in the package ([docs/NATIVE.md](docs/NATIVE.md)).

## Docs

- [docs/DESIGN.md](docs/DESIGN.md) — what okcode is, its layers, storage, embedders, management surface, and the decisions behind them.
- [docs/ACCESS.md](docs/ACCESS.md) — the access facade contract (implement it to reach a new kind of workspace).
- [docs/NATIVE.md](docs/NATIVE.md) — the native addon, the standalone binary, and how okcode reaches okdb.
- [docs/UPGRADING.md](docs/UPGRADING.md) — 0.2 → 1.0 for hosts.
- [CHANGELOG.md](CHANGELOG.md) — what changed in each release.

## License

MIT — see [LICENSE](LICENSE). okcode runs on [okdb](https://www.npmjs.com/package/@kedem/okdb), which has its own license: its free tier covers okcode for about three workspaces (embeddings included); larger hosts install an okdb license (`OKDB_LICENSE_FILE`, or `okcode.open({ license })`).
