# okcode — design

Settled decisions, and the reasons for them. Rationale inherited from the brain's code index (`brain/docs/CODE-INDEX-SPEC.md`, esp. §2.3 the `at` token, §6 freshness, §7 edit safety, §14–17 amendments) is referenced, not repeated.

## 1. What okcode is

A library (plus CLI, plus model-facing tool renderers) that lets a program — an agent, a person at a terminal, the brain — **locate, read and edit code by name** in a workspace, without reading whole files to find things, and without ever corrupting or silently overwriting a file.

It is **host-agnostic**: it does not know about the brain, machines, hosts, credentials or connections. The host tells it *which* workspaces exist, *how* to reach their files (an access facade), and *when* to sync.

## 2. Layers

| layer | module(s) | job | knows about |
| --- | --- | --- | --- |
| **access** | `src/access/` | bytes in and out of one workspace: list, stat(+hash), read, atomic commit; optional lock, exec | nothing above it — see [ACCESS.md](ACCESS.md) |
| **analysis** | `src/analysis/` | pure functions on text: language detection, symbol extraction, validation (syntax, template balance, convention colors, okjs), text/EOL/diff utilities, dependency package surfaces | nothing (packages scanning takes an access facade) |
| **workspace** | `src/workspace.js` | one workspace's graph: scan/freshness, `at` tokens, find/outline/refs/read, guarded edits | access + analysis + store |
| **store** | `src/store.js` | persistence of one workspace's graph in okdb; FTS; embeddings profiles; resolved `content` | okdb |
| **okcode** | `src/okcode.js` | the host-facing surface: open, workspaces, embedders, management (status/sync/reset) | all of the above |
| **surfaces** | `src/tools.js`, `bin/okcode.js` | model-facing tool renderers (`code_find`, `code_read`, …) and the CLI | okcode |

## 3. Access: a facade, not a connection

okcode never opens connections. The host passes an access facade per workspace ([ACCESS.md](ACCESS.md)): `list`, `stat`, `read`, `commit`, optional `lock` / `exec` / `remove`. Everything is async and batched.

Two facades ship:

- `localFs(root)` — Node `fs`; the fast path for local folders.
- `shell({ root, run, dialect })` — bash/PowerShell scripts with base64 on the wire, over any `run(script, stdin) → Promise<stdout>`. This is what makes remote workspaces **agentless**: the target needs only a shell. The host brings the transport (`ssh`, `docker exec`, …) — e.g. the brain's existing ssh driver.

Why a facade: the host already owns connections, credentials and policy (the brain's machine handles, `writeRoots`, identity files). Duplicating that inside okcode would be a second, divergent copy of the same thing. Why okcode still ships `shell`: the byte-exact scripts (base64 round trip, remote hashing, atomic compare-and-swap publish) are the hard, reusable part; every host would otherwise rewrite them.

## 4. Identity

A workspace is identified by a **host-chosen id** (`'app'`, the brain's workspace memory key), never by a path: two machines can have the same path. Everything okcode stores is scoped by that id.

## 5. Storage

okcode keeps its own okdb store (`open({ path })`), or uses one the host passes (`open({ db })`). **Never two okdb instances on one path in one process** — the brain measured a native deadlock doing that; a host that already has an okdb open on the same path must pass `db`.

**One okdb environment per workspace**: `okcode_<slug>` — the id lowercased to `[a-z0-9_]` (≤ 40 chars), plus `_<sha1(id)[:8]>` whenever slugging changed it, so `App`/`app` never collide. (Not `okcode:<id>`: okdb splits scoped engine names at the first `:`, and a leading `~` is reserved for okdb's own envs.) Removing a workspace = one `removeEnvironment`. Nothing mixes with host data.

Types inside a workspace env:

| type | key | holds |
| --- | --- | --- |
| `files` | `rel` | `rel, hash, size, mtime, lang, lines, parsed, reason, imports, exports, analyzerVersion, lossless` — **no content** |
| `symbols` | `rel::symPath::line` | `file, name, kind, lineStart, lineEnd, start, end, path, parent, signature, doc, exported` |
| `packages` | `name` | dependency surface (name, version, description, exports/types) |

- **Content is a resolved field** (`env.resolveField('files', …, { batch })`, okdb ≥ 2.3), batched through the facade; okdb never stores it. FTS reads `content`. Embeddings read a second resolved field, `prepared` (`//@ <rel>\n` + text): okdb's chunkers see only the field value, and the symbol-aware chunker needs the path for its language seams and chunk headers. Both resolvers answer from the workspace's text cache first (by rel + hash), so a scan's FTS drain re-reads nothing. The row's `hash` is its version: a changed hash rewrites the row, and the change feed re-indexes it.
- **FTS**: `symbols` on `name, path, doc, signature`; `files` on `rel, content`; both with the brain's stopword list (a question phrased naturally must not demand that "where/is/the" appear in the code).
- The in-memory working set (per open workspace) may hold file text as a **cache** for parsing and slicing; it is rebuilt from the facade, never persisted.

## 6. Freshness (inherited)

- **Open** walks once: `list` (size+mtime), then `stat({hash})` only for rows whose size or mtime moved, re-parse only if the hash moved. `.gitignore` at the root is honored.
- **Verify-on-read**: every read/find/outline result re-`stat`s the files it is about to quote and re-ingests any whose hash moved (and re-runs a result set once if anything changed). A lookup never quotes a stale file.
- **Sync** is host-triggered (`ws.sync()`), not a watcher. A watcher may *hint* a sync; it is never the source of truth.

## 7. Editing (inherited, now on the facade)

- `at` = the file's SHA-1 (from any read). Every edit requires it. A mismatch is refused (`stale`, with the current `at`); nothing is ever rebased.
- `editBatch({ at, edits: [{ target, body }] })` is the one writer: targets (symbol path/name or line range) resolve against the snapshot the `at` names; overlapping targets are refused; bodies are normalized to the file's EOL.
- **Validation before writing, locally**: baseline and candidate are validated on the text (syntax via acorn/`node --check` in-process, template balance, convention colors, okjs); a candidate that adds a *new* diagnostic is refused. The target machine needs no toolchain.
- **Write**: optional `lock`, then `commit(path, bytes, { expectedHash: at })` — compare-and-swap, atomic per file. Then read back, classify (`ok`, `committed_verified_with_warning`, `committed_unverified`, `not_committed`, `diverged_after_commit`, `commit_outcome_unknown`), re-ingest.
- **Create**: `writeWholeFile(rel, text, { create: true })` validates (any diagnostic refuses) and commits with `create`.
- **Single-file atomicity.** A batch edits one file. There is no multi-file transaction.

## 8. Embedders — profiles

Embedding is okdb's job; *which* embedder is okcode's configuration:

```js
okcode.open({
    embedders: {
        qwen:  { type: 'ollama', model: 'qwen3-embedding:0.6b', url },              // okdb built-in driver
        oa3:   { type: 'openai', model: 'text-embedding-3-small', apiKey: () => … }, // secret supplied at runtime
        local: { embed: async (texts) => vectors, dims: 768 },                       // anything custom
    },
    active: 'qwen',
});
```

- Each named **profile** gets its own okdb pipeline per workspace, named after the profile's *model and dims* (vectors from two models are not comparable — the name makes mixing impossible).
- `apiKey` may be a function, so secrets stay in the host (never written to okdb). A plain string is accepted and stored by okdb (masked in its admin UI).
- A custom `embed` is registered as an okdb embedder factory, so okdb's batching, content-hash dedupe and retries apply unchanged.
- Profiles are independent: adding one builds its vectors alongside; symbols, FTS and file rows are shared.
- No embedders configured → symbols + FTS only (`ask` unavailable).

## 9. Management surface

Library, CLI and (optionally) tools expose the same operations:

| operation | effect |
| --- | --- |
| `status()` | per workspace: files, symbols, FTS state; per embedder profile: done/pending/failed, dims, active |
| `ws.sync({ force })` | rescan now (changed files; `force` = re-hash everything) |
| `reset(id, { scope })` | `'vectors'` re-embed · `'fts'` rebuild text search · `'all'` drop the env and rescan |
| `addEmbedder(name, cfg)` / `useEmbedder(name)` / `removeEmbedder(name)` | build a profile alongside; switch queries once it is ready; drop its vectors |
| `ws.ask(q, { embedder })`, `compare(q, [a, b])` | query one profile / several side by side |
| `addWorkspace(id, { access })` / `removeWorkspace(id)` | register (and scan) / drop the env |

**Roles.** okdb runs embedding (and processor) work only in a process with those roles. Management calls act on durable state (records, cursors, rebuild requests) so they work from any process; the process holding the roles does the work. okcode never assumes the caller embeds.

## 10. What the host keeps (the brain, for example)

- Which workspaces exist and where (workspace memories → `addWorkspace(key, { access })`), including building the facade: local → `localFs`, remote machine → `shell({ run: sshDriver })`.
- When to sync (cycle open), secrets (`apiKey` functions), recording stats in its own memory.
- Its model-facing tool names/policy (it may use `okcode/tools` renderers or its own).

## 11. Compatibility

okcode 0.1 requires okdb 2.3 (resolved fields incl. batch resolvers, reconcile-based embeddings, async `describeChunk`). During development `@kedem/okdb` is `file:../okdb-src`; the two are released together once both work end to end.
