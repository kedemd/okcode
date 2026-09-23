# okcode

Agentless workspace tools for code: **find, read and edit code by name** in local or remote workspaces. Part of the ok family — [okjs](https://www.npmjs.com/package/@kedem/okjs) (UI), [okdb](https://www.npmjs.com/package/@kedem/okdb) (data), okcode (working on code).

- **Find** — symbols by name, full-text over names/doc prose and over file content, semantic search (embeddings), references, outlines, dependency packages.
- **Read** — a symbol, a range, a whole file; every read hands back an `at` token (the file's hash).
- **Edit** — by symbol or range, guarded by `at`: a stale edit is refused, never rebased. Candidates are validated locally before the write (a change that introduces a new diagnostic is refused); the write is an atomic compare-and-swap on the target.
- **Anywhere** — okcode never opens a connection. You hand it an **access facade** (list / stat / read / commit). okcode ships two: plain local `fs`, and a `shell` facade that speaks bash/PowerShell over any `run(script, stdin)` — so `ssh`, `docker exec`, `kubectl exec` reach a remote workspace with **nothing installed on it**.
- **No copies** — the index (symbols, full-text postings, vectors) lives in okdb; file content is never stored. okdb reads it through the facade when indexing and when showing a result ([resolved fields](https://github.com/kedemd/okdb/blob/main/docs/resolved-fields.md)).

> Status: 0.1. Depends on [`@kedem/okdb`](https://www.npmjs.com/package/@kedem/okdb) `^2.3.0` from npm (resolved fields, reconcile-based embeddings).

## Quick look

```js
const okcode = require('@kedem/okcode');

const oc = await okcode.open({
    path: '/var/lib/okcode', // okcode's own okdb store (or pass `db`)
    embedders: { qwen: { type: 'ollama', model: 'qwen3-embedding:0.6b' } },
    active: 'qwen',
});

// A local folder
await oc.addWorkspace('app', { access: okcode.access.localFs('/home/me/app') });

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
const { text, at } = await ws.read('retryWithBackoff');
await ws.edit('retryWithBackoff', newBody, { at });

// Semantic search; or with a vector the host already embedded (no embed call),
// once its identity — [type, endpoint, model, dims] — matches the profile's
const { identity } = oc.embedders().find((e) => e.active);
await ws.ask('where do we retry failed requests');
await ws.ask({ text: 'where do we retry failed requests', vector, identity });

// Listings from the index alone — no file content is read
await ws.files({ dir: 'src' }); // sizes, lines, languages, symbol counts
await ws.symbols('src/http.js'); // the stored symbol table of one file
```

CLI — run it in a folder; the index lives in `.okcode/` there (workspace `default`), created and kept current automatically:

```sh
okcode find retryWithBackoff
okcode read retryWithBackoff            # prints the code; at=<hash> on stderr
okcode edit retryWithBackoff --file body.txt --at <hash>
okcode status
okcode find retry --store /data/okcode --id api   # a shared store holding many workspaces
```

See `okcode --help`.

**Licensing.** Without a license, okdb's free tier covers about three workspaces (5 envs, 2 embeddings pipelines per env). A license goes into okcode's store with `okcode.open({ path, license })` (idempotent — pass it on every open), `okcode --license FILE`, or `OKDB_LICENSE_FILE=FILE` for any process; `status().license` shows what is in effect. A host that passes its own `db` licenses it itself. See [DESIGN §5](docs/DESIGN.md#5-storage).

## Docs

- [docs/DESIGN.md](docs/DESIGN.md) — what okcode is, its layers, storage, embedders, management surface, and the decisions behind them.
- [docs/ACCESS.md](docs/ACCESS.md) — the access facade contract (implement it to reach a new kind of workspace).

Lineage: extracted from the brain's code index (`brain/src/codeindex`, spec `brain/docs/CODE-INDEX-SPEC.md`), which remains the reference for edit-safety and freshness rationale.

## License

MIT — see [LICENSE](LICENSE). okcode runs on [okdb](https://www.npmjs.com/package/@kedem/okdb), which has its own license: its free tier covers okcode for about three workspaces (embeddings included); larger hosts install an okdb license (`OKDB_LICENSE_FILE`, or `okcode.open({ license })`).
