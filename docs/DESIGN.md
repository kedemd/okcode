# okcode — design

Settled decisions, and the reasons for them. Rationale inherited from the brain's code index (`brain/docs/CODE-INDEX-SPEC.md`, esp. §2.3 the `at` token, §6 freshness, §7 edit safety, §14–17 amendments) is referenced, not repeated.

## 1. What okcode is

A library (plus CLI, plus model-facing tool renderers) that lets a program — an agent, a person at a terminal, the brain — **locate, read and edit code by name** in a workspace, without reading whole files to find things, and without ever corrupting or silently overwriting a file.

It is **host-agnostic**: it does not know about the brain, machines, hosts, credentials or connections. The host tells it _which_ workspaces exist, _how_ to reach their files (an access facade), and _when_ to sync.

## 2. Layers

| layer         | module(s)                       | job                                                                                                                                                                                 | knows about                                        |
| ------------- | ------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------- |
| **access**    | `src/access/`                   | bytes in and out of one workspace: list, stat(+hash), read, atomic commit; optional lock, exec                                                                                      | nothing above it — see [ACCESS.md](ACCESS.md)      |
| **analysis**  | `src/analysis/`                 | pure functions on text: language detection, symbol extraction, validation (syntax, template balance, convention colors, okjs), text/EOL/diff utilities, dependency package surfaces | nothing (packages scanning takes an access facade) |
| **workspace** | `src/workspace.js`              | one workspace's graph: scan/freshness, `at` tokens, find/outline/refs/read, guarded edits                                                                                           | access + analysis + store                          |
| **store**     | `src/store.js`                  | persistence of one workspace's graph in okdb; FTS; embeddings profiles; resolved `content`                                                                                          | okdb                                               |
| **okcode**    | `src/okcode.js`                 | the host-facing surface: open, workspaces, embedders, management (status/sync/reset)                                                                                                | all of the above                                   |
| **surfaces**  | `src/tools.js`, `bin/okcode.js` | model-facing tool renderers (`code_find`, `code_read`, …) and the CLI                                                                                                               | okcode                                             |

## 3. Access: a facade, not a connection

okcode never opens connections. The host passes an access facade per workspace ([ACCESS.md](ACCESS.md)): `list`, `stat`, `read`, `commit`, optional `lock` / `exec` / `remove`. Everything is async and batched.

Two facades ship:

- `localFs(root)` — Node `fs`; the fast path for local folders.
- `shell({ root, run, dialect })` — bash/PowerShell scripts with base64 on the wire, over any `run(script, stdin) → Promise<stdout>`. This is what makes remote workspaces **agentless**: the target needs only a shell. The host brings the transport (`ssh`, `docker exec`, …) — e.g. the brain's existing ssh driver.

Why a facade: the host already owns connections, credentials and policy (the brain's machine handles, `writeRoots`, identity files). Duplicating that inside okcode would be a second, divergent copy of the same thing. Why okcode still ships `shell`: the byte-exact scripts (base64 round trip, remote hashing, atomic compare-and-swap publish) are the hard, reusable part; every host would otherwise rewrite them.

## 4. Identity

A workspace is identified by a **host-chosen id** (`'app'`, the brain's workspace memory key), never by a path: two machines can have the same path. Everything okcode stores is scoped by that id.

## 5. Storage

okcode keeps its own okdb store (`open({ path })`), or uses one the host passes (`open({ db })`).

**Placement** (settled): okcode's data is its own store, never mixed into a host's database — code indexing is heavy and bursty, and a separate store compacts, backs up, syncs and resets on its own schedule.

- **Standalone (CLI):** `<root>/.okcode`, one workspace with id `default`, created and kept up to date automatically (like `.git`; added to `.gitignore`; never indexed — access facades prune dot-directories). `--store DIR --id NAME` points several workspaces at one shared store.
- **Hosts with many workspaces** (the brain): one shared store at the host's chosen path (e.g. `/data/okcode` beside `/data/brain`), one env per workspace. Several processes may open it (okdb is multi-process on one path); the one holding the processing roles indexes and embeds. Every process that touches a workspace registers it (`addWorkspace`) so its access facade and resolvers exist there.
- Idle cost: an inactive workspace's vector index unloads after 5 minutes (okdb local views); the rest of an open env's footprint is small. Env residency (close idle envs) was considered and deferred as over-optimization. **Never two okdb instances on one path in one process** — the brain measured a native deadlock doing that; a host that already has an okdb open on the same path must pass `db`.

**Licensing.** okdb licenses live in the store (okdb's `~system` env), not in okcode. Unlicensed, okdb's free tier covers about three workspaces: `envs` 5 counts `default` + the `okcode` management env + one env per workspace (a pipeline's internal `~…` envs are not counted), and `pipelinesPerEnv` 2 allows two embedder profiles per workspace (or one plus the orphan a model switch leaves). Past that, the store needs a license:

- `open({ path, license })` installs it with `db.licenses.add(license)` right after okdb opens and before the management env, workspaces or pipelines are created. `license` is the license text (a blob, `"<blob>\n<token>"`) or `{ blob, activation }`; it is idempotent, so passing it on every open is fine. An invalid license refuses the open (okdb's `LICENSE_INVALID`, the store closed again).
- A standard (node-bound) license needs a one-time activation: the open still succeeds (free tier until then), a warning names the PIN, and `status().license` is `{ status: 'needs-activation', id, pin, … }`. The host sends the PIN to the vendor, then applies the token with `oc.db.licenses.activate(token)` (live) or reopens with `{ blob, activation }`.
- `open({ db })`: the host owns licensing; `license` is ignored (with a warning).
- okdb itself installs `OKDB_LICENSE_FILE` at every open, so a host that sets it (the brain does) needs no okcode code at all.
- `status().license` = `{ status: 'active' | 'free' | 'needs-activation', type, licensee, expiresAt, enforced, id?, pin? }` — never the license text. The CLI takes `--license FILE`.

**One okdb environment per workspace**: `okcode_<slug>` — the id lowercased to `[a-z0-9_]` (≤ 40 chars), plus `_<sha1(id)[:8]>` whenever slugging changed it, so `App`/`app` never collide. (Not `okcode:<id>`: okdb splits scoped engine names at the first `:`, and a leading `~` is reserved for okdb's own envs.) Removing a workspace = one `removeEnvironment`. Nothing mixes with host data.

Types inside a workspace env:

| type       | key                  | holds                                                                                                               |
| ---------- | -------------------- | ------------------------------------------------------------------------------------------------------------------- |
| `files`    | `rel`                | `rel, hash, size, mtime, lang, lines, parsed, reason, imports, exports, analyzerVersion, lossless` — **no content** |
| `symbols`  | `rel::symPath::line` | `file, name, kind, lineStart, lineEnd, start, end, path, parent, signature, doc, exported`                          |
| `packages` | `name`               | dependency surface (name, version, description, exports/types)                                                      |

- **Content is a resolved field** (`env.resolveField('files', …, { batch })`, okdb ≥ 2.3), batched through the facade; okdb never stores it. FTS reads `content`. Embeddings read a second resolved field, `prepared` (`//@ <rel>\n` + text): okdb's chunkers see only the field value, and the symbol-aware chunker needs the path for its language seams and chunk headers. Both resolvers answer from the workspace's text cache first (by rel + hash), so a scan's FTS drain re-reads nothing. The row's `hash` is its version: a changed hash rewrites the row, and the change feed re-indexes it.
- **FTS**: `symbols` on `name, path, doc, signature`; `files` on `rel, content`; both with the brain's stopword list (a question phrased naturally must not demand that "where/is/the" appear in the code).
- The in-memory working set (per open workspace) may hold file text as a **cache** for parsing and slicing; it is rebuilt from the facade, never persisted.

## 6. Freshness (inherited)

- **Open** walks once: `list` (size+mtime), then `stat({hash})` only for rows whose size or mtime moved, re-parse only if the hash moved. `.gitignore` at the root is honored.
- **Verify-on-read**: every read/find/outline result re-`stat`s the files it is about to quote and re-ingests any whose hash moved (and re-runs a result set once if anything changed). A lookup never quotes a stale file.
- **Sync** is host-triggered (`ws.sync()`), not a watcher. A watcher may _hint_ a sync; it is never the source of truth.

## 7. Editing (inherited, now on the facade)

- `at` = the file's SHA-1 (from any read). Every edit requires it. A mismatch is refused (`stale`, with the current `at`); nothing is ever rebased.
- `editBatch({ at, edits: [{ target, body }] })` is the one writer: targets (symbol path/name or line range) resolve against the snapshot the `at` names; overlapping targets are refused; bodies are normalized to the file's EOL.
- **Validation before writing, locally**: baseline and candidate are validated on the text (syntax via acorn/`node --check` in-process, template balance, convention colors, okjs); a candidate that adds a _new_ diagnostic is refused. The target machine needs no toolchain.
- **Write**: optional `lock`, then `commit(path, bytes, { expectedHash: at })` — compare-and-swap, atomic per file. Then read back, classify (`ok`, `committed_verified_with_warning`, `committed_unverified`, `not_committed`, `diverged_after_commit`, `commit_outcome_unknown`), re-ingest.
- **Create**: `writeWholeFile(rel, text, { create: true })` validates (any diagnostic refuses) and commits with `create`.
- **Single-file atomicity.** A batch edits one file. There is no multi-file transaction.

## 8. Embedders — profiles

Embedding is okdb's job; _which_ embedder is okcode's configuration:

```js
okcode.open({
    embedders: {
        qwen:  { type: 'ollama', model: 'qwen3-embedding:0.6b', url },              // okdb built-in driver
        oa3:   { type: 'openai', model: 'text-embedding-3-small', apiKey: () => … }, // secret supplied at runtime
        local: { embed: async (texts) => vectors, id: 'my-embedder@2', dims: 768 },  // anything custom
    },
    active: 'qwen',
});
```

- Each named **profile** gets its own okdb pipeline per workspace, addressed by the profile's **identity** — the vector space it embeds into (`src/identity.js`):

    `identity = JSON.stringify([type, endpoint, model, dims])`

    | part       | built-in provider                                      | custom `embed`                  |
    | ---------- | ------------------------------------------------------ | ------------------------------- |
    | `type`     | the provider (`'ollama'`, `'openai'`, a factory type)  | `'custom'`                      |
    | `endpoint` | its `url` / `base_url` (`''` = the provider's default) | the profile's required **`id`** |
    | `model`    | `model` (`''` when unset)                              | `model` (`''` when unset)       |
    | `dims`     | the vector length (given, or learned from the model)   | same                            |

    The pipeline is named `code_<slug(model)>_<dims>_<sha1(identity)[:8]>` — readable parts for a person scanning okdb's admin, and a hash of the whole identity that actually separates spaces. Vectors from two spaces are not comparable, and a store that mixes them returns nonsense while looking healthy — so the same model name at two endpoints (ollama vs an OpenAI-compatible server), or a model swapped behind the same name on another host, is **two stores**, and changing any part of a profile addresses a **new** store: old vectors are never served for the new space. Reopening with an unchanged profile computes the same name and keeps its store (nothing re-embedded). The identity string has the same shape the brain computes from its own embedder config, so a host decides by plain equality whether a vector it holds lives in okcode's space; there is no normalisation (`http://h:11434` ≠ `http://h:11434/`). `apiKey` and other provider fields are not part of it; a keyed profile's identity names the provider, not okcode's derived factory type.

- **Custom profiles must name their space**: `id` is required (`OKCODE_NEEDS_ID` without it). okcode cannot inspect what a function embeds into, so the host says — change `id` whenever the function starts producing different vectors. The profile _name_ is deliberately not part of the identity: it is the host's handle for switching profiles, not a statement about the vectors.
- **Orphans.** A store no profile addresses any more (its profile's url/model/dims changed, or it was named by an older scheme) is not deleted behind the host's back — another process may still be configured for it — but it is not served either. `status()` lists it per workspace under `orphaned: [{ pipeline, done, vectors }]`, and `removeOrphaned(id?)` drops it (its indexer otherwise keeps embedding changes against the old embedder). There was no released okcode before identity naming, so there is no migration: stores named `code_<model>_<dims>` simply show up as orphaned.
- `apiKey` may be a function, so secrets stay in the host (never written to okdb). A plain string is accepted and stored by okdb (masked in its admin UI).
- A custom `embed` is registered as an okdb embedder factory, so okdb's batching, content-hash dedupe and retries apply unchanged.
- Profiles are independent: adding one builds its vectors alongside; symbols, FTS and file rows are shared.
- No embedders configured → symbols + FTS only (`ask` unavailable).

## 9. Management surface

Library, CLI and (optionally) tools expose the same operations:

| operation                                                                          | effect                                                                                                            |
| ---------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------- |
| `status()`                                                                         | per workspace: files, symbols, FTS state; per embedder profile: done/pending/failed, dims, active; `license` (§5) |
| `ws.sync({ force })`                                                               | rescan now (changed files; `force` = re-hash everything)                                                          |
| `reset(id, { scope })`                                                             | `'vectors'` re-embed · `'fts'` rebuild text search · `'all'` drop the env and rescan                              |
| `addEmbedder(name, cfg)` / `useEmbedder(name)` / `removeEmbedder(name)`            | build a profile alongside; switch queries once it is ready; drop its vectors                                      |
| `ws.ask(q, { profile })`, `compare(id, q, [a, b])`                                 | query one profile / several side by side; `q` is a string or `{ text?, vector, identity? }`                       |
| `embedders()`                                                                      | `[{ name, type, endpoint, model, dims, identity, active, state }]` — no functions, no secrets                     |
| `removeOrphaned(id?)`                                                              | drop stores no profile addresses any more (`status().workspaces[].orphaned`)                                      |
| `ws.files({ dir, lang, limit, stat })` / `ws.symbols(file, { kind, limit, stat })` | cheap listings from the stored rows — never read file content (below)                                             |
| `addWorkspace(id, { access })` / `removeWorkspace(id)`                             | register (and scan) / drop the env                                                                                |

**Querying with a precomputed vector.** A host that embeds each question once and shares the vector across several indexes passes it instead of the text: `ws.ask({ vector, text?, identity? }, opts)` (also `store.ask`, `compare`, and the `code_ask` tool's `vector`/`identity` args — host-only, not in the model-facing schema). `vector` is a `Float32Array` (a number array is accepted). With a vector **no embed call is made** for the query. The lexical eyes (names, doc prose, file text) need words: with a vector and no `text` the answer is semantic hits only; with both, the three eyes fuse exactly as for a string. The vector must have the profile's dims (`OKCODE_DIMS_MISMATCH`, never searched); when the host passes the `identity` it computed the vector in, it must equal the profile's (`OKCODE_IDENTITY_MISMATCH`). A host decides compatibility up front by comparing its identity with `embedders()[i].identity` and passes a vector only on a match. A query with neither text nor vector is `OKCODE_BAD_QUERY`.

**Listing without reading.** A host lists by metadata and never opens bodies to list. `ws.files({ dir, lang, limit })` → `{ total, truncated, asOf, files: [{ file, size, lines, lang, symbols, indexed, hash }] }` and `ws.symbols(file, { kind, limit })` → `{ file, size, lines, lang, hash, total, symbols: [{ name, path, kind, parent, lineStart, lineEnd, span, signature, exported, doc }] }` answer from the stored rows with **no facade call at all** — as fresh as the last scan (`asOf`; on a warm store, the open-time walk). `stat: true` adds one metadata `stat` and flags rows that `moved` or are `gone` since, still reading nothing. `hash` is a locator, not an `at`: read/outline verify. `symbols()` is the cheap sibling of `outline()`, the **rich** outline: outline carves an extension's structural regions (okjs template/style) or chunks (files without symbols) from the text, so it reads those files; for a plain file with symbols it now answers from the rows with a stat-first check (reads only if the file moved), as do `structure()` and the result-set checks of find/refs/mentions. So `code_outline` and `code_map` read nothing on a warm store for files that did not move; their output is unchanged.

Notes: `reset('fts')` needs the workspace **open** in this process (the content index reads through its access facade); `'vectors'` works from any process. `sync()` returns `[{ id, scanned, changed, removed, ms }]`. `compare()` returns raw per-profile vector hits (not the merged `ask` results, which would hide the differences). Error codes: `OKCODE_UNKNOWN_WORKSPACE`, `OKCODE_WORKSPACE_NOT_OPEN`, `OKCODE_UNKNOWN_EMBEDDER`, `OKCODE_EMBEDDER_EXISTS`, `OKCODE_NEEDS_CONFIG`, `OKCODE_NEEDS_ID`, `OKCODE_UNKNOWN_EMBEDDER_TYPE`, `OKCODE_NO_EMBEDDINGS`, `OKCODE_DIMS_MISMATCH`, `OKCODE_IDENTITY_MISMATCH`, `OKCODE_BAD_QUERY`.

**Persistence** (okdb env `okcode`): `workspaces` (id → env, added, lastSync, access kind/root), `embedders` (name → provider/fields (url, model)/dims, a custom profile's `id`, never a function or a function-sourced secret; a profile whose function was not re-supplied on open reports `needs-config`), `settings` (`active`).

**Roles.** okdb runs embedding (and processor) work only in a process with those roles. Management calls act on durable state (records, cursors, rebuild requests) so they work from any process; the process holding the roles does the work. okcode never assumes the caller embeds.

## 10. What the host keeps (the brain, for example)

- Which workspaces exist and where (workspace memories → `addWorkspace(key, { access })`), including building the facade: local → `localFs`, remote machine → `shell({ run: sshDriver })`.
- When to sync (cycle open), secrets (`apiKey` functions), recording stats in its own memory.
- Licensing (§5): the host supplies the license — `open({ path, license })`, or on its own `db` when it passes one. The brain's existing `OKDB_LICENSE_FILE` (inherited by its child processes) already covers okcode's store: okdb installs it at every open, with no okcode code.
- Its model-facing tool names/policy (it may use `okcode/tools` renderers or its own).

## 11. Compatibility

okcode 0.1 requires okdb 2.3 (resolved fields incl. batch resolvers, reconcile-based embeddings, async `describeChunk`). During development `@kedem/okdb` is `file:../okdb-src`; the two are released together once both work end to end.
