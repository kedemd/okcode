# okcode

Agentless workspace tools for code: **find, read and edit code by name** in local or remote workspaces. Part of the ok family — [okjs](../okjs) (UI), [okdb](../okdb-src) (data), okcode (working on code).

- **Find** — symbols by name, full-text over names/doc prose and over file content, semantic search (embeddings), references, outlines, dependency packages.
- **Read** — a symbol, a range, a whole file; every read hands back an `at` token (the file's hash).
- **Edit** — by symbol or range, guarded by `at`: a stale edit is refused, never rebased. Candidates are validated locally before the write (a change that introduces a new diagnostic is refused); the write is an atomic compare-and-swap on the target.
- **Anywhere** — okcode never opens a connection. You hand it an **access facade** (list / stat / read / commit). okcode ships two: plain local `fs`, and a `shell` facade that speaks bash/PowerShell over any `run(script, stdin)` — so `ssh`, `docker exec`, `kubectl exec` reach a remote workspace with **nothing installed on it**.
- **No copies** — the index (symbols, full-text postings, vectors) lives in okdb; file content is never stored. okdb reads it through the facade when indexing and when showing a result ([resolved fields](../okdb-src/docs/resolved-fields.md)).

> Status: 0.1 in development. Requires the unpublished okdb 2.3 (resolved fields, reconcile-based embeddings) — `@kedem/okdb` is linked from `../okdb-src` until both are released together.

## Quick look

```js
const okcode = require('@kedem/okcode');

const oc = await okcode.open({
    path: '/var/lib/okcode',                       // okcode's own okdb store (or pass `db`)
    embedders: { qwen: { type: 'ollama', model: 'qwen3-embedding:0.6b' } },
    active: 'qwen',
});

// A local folder
await oc.addWorkspace('app', { access: okcode.access.localFs('/home/me/app') });

// A remote folder — okcode supplies the scripts, you supply the transport
await oc.addWorkspace('prod-api', {
    access: okcode.access.shell({ dialect: 'bash', root: '/srv/api', run: (script, stdin) => mySsh.run(script, stdin) }),
});

const ws = oc.workspace('app');
await ws.sync();
const hits = await ws.find('retryWithBackoff');
const { text, at } = await ws.read('retryWithBackoff');
await ws.edit('retryWithBackoff', newBody, { at });
```

CLI: `okcode find <query>`, `okcode read <symbol>`, `okcode edit <symbol> --file body.txt --at HASH`, `okcode status`, … (see `okcode --help`).

## Docs

- [docs/DESIGN.md](docs/DESIGN.md) — what okcode is, its layers, storage, embedders, management surface, and the decisions behind them.
- [docs/ACCESS.md](docs/ACCESS.md) — the access facade contract (implement it to reach a new kind of workspace).
- [docs/WORKPLAN.md](docs/WORKPLAN.md) — build phases and status.

Lineage: extracted from the brain's code index (`brain/src/codeindex`, spec `brain/docs/CODE-INDEX-SPEC.md`), which remains the reference for edit-safety and freshness rationale.
