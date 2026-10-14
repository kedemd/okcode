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

okcode never opens connections. The host passes an access facade per workspace ([ACCESS.md](ACCESS.md)): `list`, `stat`, `read`, optional `commit` / `lock` / `exec` / `remove`. Everything is async and batched. A facade without `commit` is **read-only**: every edit is refused (`OKCODE_READ_ONLY`, in the edit's result) before anything is resolved.

Four facades ship:

- `localFs(root)` — Node `fs`; the fast path for local folders.
- `shell({ root, run, dialect })` — bash/PowerShell scripts with base64 on the wire, over any `run(script, stdin) → Promise<stdout>`. This is what makes remote workspaces **agentless**: the target needs only a shell. The host brings the transport (`ssh`, `docker exec`, …) — e.g. the brain's existing ssh driver.
- `fn({ list, read, stat? })` — read-only, over the host's own functions (`stat` derived from `list` when absent).
- `memory(map)` — read-only, over a live `Map<path, { bytes, mtime }>`.

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
        local: { embed: async (texts, { workspace }) => vectors, id: 'my-embedder@2', dims: 768 },  // anything custom
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
- **Context.** A custom function is called as `embed(texts, { workspace })` (and `embedBatch(texts, { workspace })`): one profile embeds for every workspace, and which one's text it is — the host's id — is what a host needs to decide where that text may be sent (a private workspace must not reach an untrusted embedder). okdb hands a factory only its embedder config, so the store writes the workspace id into each workspace's embedder config (custom profiles only; it is not part of okdb's fingerprint `{ type, model, dims }`, so nothing is re-embedded); a pipeline created before this is patched and its embedder restarted where engines run. `workspace` is `null` for an engine not tied to one. A function that takes one argument ignores it.
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
| `addWorkspace(id, { access, index })` / `removeWorkspace(id)`                      | register (and scan) / drop the env; `index: 'none'` = unindexed (below): no env, no scan, nothing to drop          |

**Querying with a precomputed vector.** A host that embeds each question once and shares the vector across several indexes passes it instead of the text: `ws.ask({ vector, text?, identity? }, opts)` (also `store.ask`, `compare`, and the `code_ask` tool's `vector`/`identity` args — host-only, not in the model-facing schema). `vector` is a `Float32Array` (a number array is accepted). With a vector **no embed call is made** for the query. The lexical eyes (names, doc prose, file text) need words: with a vector and no `text` the answer is semantic hits only; with both, the three eyes fuse exactly as for a string. The vector must have the profile's dims (`OKCODE_DIMS_MISMATCH`, never searched); when the host passes the `identity` it computed the vector in, it must equal the profile's (`OKCODE_IDENTITY_MISMATCH`). A host decides compatibility up front by comparing its identity with `embedders()[i].identity` and passes a vector only on a match. A query with neither text nor vector is `OKCODE_BAD_QUERY`.

**Listing without reading.** A host lists by metadata and never opens bodies to list. `ws.files({ dir, lang, limit })` → `{ total, truncated, asOf, files: [{ file, size, lines, lang, symbols, indexed, hash }] }` and `ws.symbols(file, { kind, limit })` → `{ file, size, lines, lang, hash, total, symbols: [{ name, path, kind, parent, lineStart, lineEnd, span, signature, exported, doc }] }` answer from the stored rows with **no facade call at all** — as fresh as the last scan (`asOf`; on a warm store, the open-time walk). `stat: true` adds one metadata `stat` and flags rows that `moved` or are `gone` since, still reading nothing. `hash` is a locator, not an `at`: read/outline verify. `symbols()` is the cheap sibling of `outline()`, the **rich** outline: outline carves an extension's structural regions (okjs template/style) or chunks (files without symbols) from the text, so it reads those files; for a plain file with symbols it now answers from the rows with a stat-first check (reads only if the file moved), as do `structure()` and the result-set checks of find/refs/mentions. So `code_outline` and `code_map` read nothing on a warm store for files that did not move; their output is unchanged.

**grep and glob — the index is an accelerator, never a gate.** `ws.grep(pattern, { regex, caseSensitive: false|true|'smart', glob, paths, context|before|after, maxMatches = 200, maxPerFile = 50, maxFiles })` is rg over the workspace: every matching line, in path order, as `{ file, line, col, text, before[], after[] }`, plus per-file `{ file, count, shown, at, from?, passed? }`. Every candidate is read to the end, so `total: { lines, files }` and every file's `count` (in `counts`, with its `start` in the stream) are exact however little is shown. **Paging:** the matching lines of all files, in path order, form one stream; a page holds up to `maxMatches` lines from at most `maxFiles` files, at most `maxPerFile` per file (the rest of that file is `passed` over on that page, the next file follows); `page` (1-based) and `offset` (a stream index — where page 1 starts) address it deterministically, and `next: { page, offset }` says where the following page starts (`null` at the end). **Output modes:** `output: 'lines'` (default, the above), `'files'` (the page of `{ file, count }` rows, rg -c) and `'matches'` (`distinct: [{ text, count, lines, files, first }]` — every non-empty matched string with its occurrence count, rg -o | sort | uniq -c, count-descending; `total.distinct`/`total.occurrences`), both paged the same way over their own rows. `code_grep` renders all three under a size budget (`max_chars`, the tool host's `grepMaxChars`, default 12 000): it stops at a whole line and ends with the exact continuation (`page=N`, or `offset=N` when the budget cut mid-page), since a host that clips output clips the end. Literal by default; `regex: true` is JS `RegExp` syntax (a meta-free regex is treated as a literal); matching is per line (CR dropped; a pattern with a newline is refused, `GREP_BAD_PATTERN`). Globs are rg's (`*`, `**`, `?`, `[..]`, `{a,b}`, `!x` excludes; no `/` = a name at any depth; a glob that matches a directory takes its subtree); `paths` are directories or files. Secret and opaque files are never searched. `ws.glob(pattern | [patterns], { limit })` lists matching paths with size/lines/lang and reads nothing. Which files get READ is decided by the cheapest **sound** source (`via`): `target` — the facade's optional `filesContaining` greps where the files live (bash shell facade: a C-locale `grep -lF`), so only matching files cross the transport; `index` — the content full-text index, queried in prefix mode with only the terms every containing file must hold (a word run of the literal bounded on the left by punctuation; a single bare word has none, since it may be the tail of a longer token), used only when the index has caught up, and widened by every file it cannot vouch for (unflushed, in flight, refused by the store, or moved on disk per one metadata stat); `scan` — every searchable file, verified stat-first and served from the records (read once, then cached). Before a cold workspace's first walk has finished, grep and glob answer from the facade's listing directly (`indexed: false`, nothing recorded) and start that walk in the background; a store that loaded rows counts as ready. Future: a regex pre-filter on the target (dialect differences make it unsound today), shipping matching lines instead of files, and a PowerShell `filesContaining`.

**Library code — searched on demand, never indexed.** A path with a skip-named segment (`node_modules`, `vendor`, `dist`, … — `options.skip` or the facade default; never a dot-directory) is _library_: reference material, huge, rewritten on every install, almost never asked about, so the walk never enters it and nothing there is indexed, full-text indexed or embedded. When a question is about one, the caller names it, and it is read the way rg reads it — listed fresh (`list({ under })`, just that subtree, nested `node_modules` pruned below it, `dist`/`vendor` inside it kept), read, matched, forgotten:

- `ws.grep(pattern, { paths: ['node_modules/<pkg>'] })` — a `paths` entry inside a skipped directory (a folder or a file) adds that subtree to **this call's** universe, grepped in direct mode (the target pre-filter still applies); project paths in the same call search as before, results merged in path order. Globs filter inside a named library but never reach one on their own. The answer carries `library: { paths, files, truncated, cap }`. Without a library path the call is exactly the old one.
- `ws.read('node_modules/<pkg>/x.js')` (also `:from-to`, and `#name`, which parses that one file in memory), `readFile`, `readAt` — read directly, same caps, `library: true`, nothing recorded. A bare symbol name stays index-only.
- `ws.outline(path)` — a library file: its symbols (or chunks), parsed in memory; a library folder: `by: 'files'`, its files with their top-level symbols. `ws.find(name, { path })` looks names (and doc prose) up in that subtree — by name, never by meaning. Both come from a scan kept **in memory only**: the parsed symbols (no text) of the last `libraryScans` (4) subtrees, LRU, re-listed on each use and re-parsed only where size/mtime moved. `ws.ask(q, { path })` with a library path throws `OKCODE_LIBRARY_NOT_INDEXED`: semantic search over library code is not supported.
- Bounds: grep lists at most `libraryFiles` (20 000) files per call, an outline or name lookup parses at most `libraryParseFiles` (2 000); over the cap the answer is partial and says so (`truncated` / `capped`; the tools print `PARTIAL … narrow the path` under the heading). Grep's output caps apply as usual. A `..` or a symlink out of the root is refused (`OKCODE_BAD_PATH` from grep/find, `ok: false` from read/outline).
- The tools take the same paths: `code_grep` `paths`, `code_read`, `code_outline` `file`, `code_find` `path`; a library read prints no `at` (it cannot be edited through okcode).

**Unindexed workspaces — `addWorkspace(id, { access, index: 'none' })`.** The whole workspace is served the way library code is (above), for material a host wants navigated but never indexed (the brain's kept reads of one task): no okdb env, no store, no registry record, no open-time walk, never embedded; it exists in this process only, until `removeWorkspace(id)`. Every verb goes to the facade on every call, so what the facade holds now is what is answered:

- `grep(pattern, opts)` — every file (no `paths`) or the named ones, read and matched directly (`indexed: false`, `via: 'direct'` or `'target'`); `glob`, `files()` list fresh (no line or symbol counts).
- `read(path | path:from-to | path#Heading)` — every name is a path (there is no symbol index); `outline(path)` — a file's symbols, sections (below) or chunks; `outline('.')` or a folder — its files with their top-level names (headings for documents); `find(name, { path? })` — by name (symbols and headings) in an in-memory parse.
- `ask` throws `OKCODE_NOT_INDEXED`, as do `refs`, `mentions`, `structure`, and `reset`/`compare` on it; `symbols()` answers `{ ok: false, code: 'OKCODE_NOT_INDEXED' }`; every edit is refused (`OKCODE_READ_ONLY`). `status()` and `workspaces()` list it with `index: 'none'`, `env: null`.
- Text is read up to `textMax` (default 32 MB here; the index's own cap, 512 KB, elsewhere). An id an indexed workspace holds is refused (`OKCODE_WORKSPACE_EXISTS`). The default, `index: 'full'`, is unchanged.

**Documents — sections, not chunks.** A file with no symbols that is not code (markdown, plain text, HTML, JSON, logs — `analysis.regions(path, text)`) is outlined as titled **sections** (`by: 'section'`): `[{ kind: 'heading' | 'section' | 'paragraph' | 'member' | 'page' | 'lines', title, lineStart, lineEnd, level? }]` — markdown/HTML headings (nested: a heading's section holds its subsections; fenced code is not a heading; text before the first heading is a `section`), a JSON document's top-level members (array elements grouped up to 80 lines), pages of an extracted text (form feeds), prose paragraphs merged up to 80 lines, and runs of 200 lines through a long block without blank lines (a log). Each is addressed by range; a heading also by title: `read('guide.md#Install')` reads that whole section (matched as written, then ignoring case and leading `#`s; a title two headings share is ambiguous, with both ranges). Code without symbols keeps its chunks. In an indexed workspace `file#name` is still a symbol first; a heading only when no symbol has that name.

Notes: `reset('fts')` needs the workspace **open** in this process (the content index reads through its access facade); `'vectors'` works from any process. `sync()` returns `[{ id, scanned, changed, removed, ms }]`. `compare()` returns raw per-profile vector hits (not the merged `ask` results, which would hide the differences). Error codes: `OKCODE_UNKNOWN_WORKSPACE`, `OKCODE_WORKSPACE_NOT_OPEN`, `OKCODE_UNKNOWN_EMBEDDER`, `OKCODE_EMBEDDER_EXISTS`, `OKCODE_NEEDS_CONFIG`, `OKCODE_NEEDS_ID`, `OKCODE_UNKNOWN_EMBEDDER_TYPE`, `OKCODE_NO_EMBEDDINGS`, `OKCODE_DIMS_MISMATCH`, `OKCODE_IDENTITY_MISMATCH`, `OKCODE_BAD_QUERY`, `OKCODE_NOT_INDEXED`, `OKCODE_WORKSPACE_EXISTS`; an edit result's `code: 'OKCODE_READ_ONLY'`.

**Persistence** (okdb env `okcode`): `workspaces` (id → env, added, lastSync, access kind/root), `embedders` (name → provider/fields (url, model)/dims, a custom profile's `id`, never a function or a function-sourced secret; a profile whose function was not re-supplied on open reports `needs-config`), `settings` (`active`).

**Roles.** okdb runs embedding (and processor) work only in a process with those roles. Management calls act on durable state (records, cursors, rebuild requests) so they work from any process; the process holding the roles does the work. okcode never assumes the caller embeds. A process without engines never creates an embedder or pipeline: it attaches to the one an indexing process created. Opened before that exists, its profile is `pending` in `status()` (not an error, not logged) and attaches on the next `status()` or `ask()` — no restart; until then `ask()` throws `OKCODE_NO_EMBEDDINGS` with `err.pending = true`.

## 10. What the host keeps (the brain, for example)

- Which workspaces exist and where (workspace memories → `addWorkspace(key, { access })`), including building the facade: local → `localFs`, remote machine → `shell({ run: sshDriver })`; material it only wants navigated (a task's kept reads) → `addWorkspace(id, { access: fn(…), index: 'none' })`.
- Labels, lifetime and provenance of what it hands okcode: okcode knows nothing of sensitivity or retention; a custom embedder learns only the workspace id (§8) and maps it to the host's own label.
- When to sync (cycle open), secrets (`apiKey` functions), recording stats in its own memory.
- Licensing (§5): the host supplies the license — `open({ path, license })`, or on its own `db` when it passes one. The brain's existing `OKDB_LICENSE_FILE` (inherited by its child processes) already covers okcode's store: okdb installs it at every open, with no okcode code.
- Its model-facing tool names/policy (it may use `okcode/tools` renderers or its own).

## 11. Compatibility

okcode 0.1 requires okdb 2.3 (resolved fields incl. batch resolvers, reconcile-based embeddings, async `describeChunk`): the dependency is `@kedem/okdb ^2.3.0`, the published package. That package is okdb's production build, so its free-tier limits are enforced (§5); the few tests that need more than the free tier (three embedder profiles in one workspace, installing a test-signed license) skip there and run against a dev checkout of okdb or under a license.

## 12. Languages beyond JavaScript (Phase F)

JavaScript keeps its own hand-written extractor (`src/analysis/parse.js`, on acorn) — exports, `module.exports`, `node --check`, and already tested. Every other language supported for symbols runs through ONE generic engine, `src/analysis/treesitter.js`: a language is a data row (extensions, a vendored grammar, a tags query), not a block of AST-walking code. First set: TypeScript/TSX (incl. `.mts`/`.cts`), Python, Go, Rust.

- **Query**: each grammar's own upstream `tags.scm` (the query GitHub code navigation uses), plus a small local override committed alongside it (`grammars/<lang>/tags.local.scm`) only where upstream's own query misses something — a plain class, a function declaration, a type alias and an enum for TypeScript; docstrings for Python; a method's receiver type for Go; an impl block's type for Rust. Each override file says what it adds and why.
- **Packaging**: the grammar `.wasm` + its `tags.scm` are vendored into `grammars/<lang>/` (`scripts/vendor-grammars.js`) and committed; the grammar NPM packages (which build native bindings on install) stay devDependencies, needed only to re-vendor. The WASM runtime, `web-tree-sitter`, is an **optionalDependency**, required lazily. Either piece missing degrades to today's behaviour for that language: searchable text, no symbols, no syntax gate — never an error.
- **Sync/async seam**: `extract()`'s registered extractors are synchronous (every call site in `workspace.js` calls `extract()` as a plain function); loading a WASM grammar is not. `workspace.js`'s scan (`fullScan`, `verify`, `ensureTexts`, an own-write ingest) `await`s `treesitter.preload(paths)` for the languages about to be ingested, once per batch, before calling `extract()` synchronously on each file. A file ingested through a path that skips that preload (a library scan outside the index) just gets the "no symbol extractor yet" fallback for that one call.
- **Freshness**: a tree-sitter symbol's `analyzerVersion` is the vendored grammar's version plus the local query's own version; `workspace.js`'s restale check compares it the same way it already compares an extension's version, so bumping either forces a reparse even when a file's own bytes never changed.
- **Syntax gate (F2)**: `validate.js` runs the same grammars' `ERROR`/`MISSING` nodes into diagnostics (`source: 'treesitter-syntax'`) for every tree-sitter language, in the shared diagnostic shape. The existing baseline-vs-candidate rule (§7) is unchanged — it refuses only a *new* one — so every tree-sitter language gets the same guarded edits JavaScript already has, with no toolchain on the target. Absent the runtime or a grammar: no diagnostics, not a false "no errors".

### Resolved edges (F3) and okjs component usage (F4.3)

`imports` (every file's row) now carries, per entry, `names: [{ local, imported }]` — the LOCAL binding a specifier introduces, paired with its pre-alias name in the source module. JS keeps its own acorn extraction (`parse.js`); TypeScript/Python/Go/Rust get it from a NEW per-language query, `grammars/<lang>/imports.scm`, read by one generic function (`treesitter.js#buildImports`) — no per-language JS. A capture is named `<family>.<role>` (`role` = `stmt`/`source`/`name`/`importedName`); `family` becomes the edge's `kind` verbatim (`import`, `reexport`, Python's `modimport` for a whole-module `import a.b`, Rust's `modpath` for a bodyless `mod foo;`) — a language can introduce its own family with no change to the reader. JS also gained a full (non-depth-limited) `CallExpression`/`NewExpression` walk, `references: [{ name, kind: 'call', line }]`, the same shape tree-sitter's `@reference.call` capture already produced — this is what lets `refs()` mark a `call` site in either kind of file. Only call-kind reference sites are persisted (`files.references`, name + line only — never the node); every other `@reference.*` kind (type/class/implementation) is used in-process and dropped before a save.

`src/edges.js` turns those stored rows into a specifier → **workspace file** (and named symbol, when the import names one) — never a filesystem guess: an external package or an import this module cannot resolve stays unresolved. Everything is derived from stored `files`/`imports`/`symbols` rows, with one exception: a handful of small project config files (`tsconfig.json`, `go.mod`, `Cargo.toml`, `pyproject.toml`/`setup.py`) are read directly off the access facade, because they are either not part of the ordinary text index (`go.mod`'s extension is not in `parse.js`'s `TEXT` set) or needed far less often than per-file resolution would otherwise re-read them. Per language:

- **JS/TS**: a relative specifier tries `.js`/`.mjs`/`.cjs`/`.ts`/`.tsx`/`.mts`/`.cts`/`.d.ts` then an index file, package.json-free. A non-relative specifier is tried against the nearest ancestor `tsconfig.json`'s `compilerOptions.baseUrl`/`paths` (JSONC, comments and trailing commas stripped by a string-aware pass — `edges.js#stripJsonc`); nested tsconfigs apply to their own subtree (nearest ancestor wins); `extends` is honoured only when it is a single local relative path, one hop, never a further chain. A re-export (`export … from`/`export *`) is its own `kind: 'reexport'`, followed one level by `importers()` and by `refs()`'s own `imported` label. Unresolved: anything not matching a workspace file (an npm package).
- **Python**: source roots are every directory that is the PARENT of a top-level `__init__.py` chain, every `pyproject.toml`/`setup.py` project directory (plus its `src/`, when present), and the workspace root as a universal fallback. `import a.b.c` resolves the dotted path against each root; `from .pkg import x` resolves the dots against the importing file's own directory (one extra `dirname()` per extra dot) then tries `x` as a symbol inside the module, falling back to `x` as a submodule of it. Unresolved: anything matching no root (an installed package).
- **Go**: `go.mod`'s `module` line gives the module path; an import whose path has that prefix maps to the remaining path as a directory, resolved to every `.go` file in it (not `_test.go`) — Go has no per-symbol import syntax, so a qualified call (`pkg.Helper()`) is not matched by `refs()`'s bare-name `imported` label, even though `importers()` already has the resolved edge. Unresolved: anything outside the module (the standard library, a dependency).
- **Rust**: a crate's module tree is built once from `mod foo;` declarations (the `modpath` family — no body, so a query negation on tree-sitter's `body:` field tells it apart from `mod foo { … }`, which is already resolved where it is written) starting at `src/main.rs`/`src/lib.rs`; a child module lives at `<dir>/<name>.rs` or `<dir>/<name>/mod.rs`. `crate::`/`super::`/`self::` resolve against that tree; the last path segment is tried first as an item inside the module, then as the module itself. Conservative, noted gaps: no `#[path]` overrides, no cross-crate `use app_name::x` (only `crate::`/`super::`/`self::` within one crate), and `super::`/`self::` inside an inline `mod a { … }` resolve to the file that WROTE it, not a synthetic child.
- **okjs (F4.3)**: a `component`/`component-tag` dependency's `from` is the tag text, not a path; it resolves against a workspace-wide tag → defining-file index built from every file's `component`-kind symbol. "Who uses `<setting-group>`" is `importers('setting-group.ok.js')`, a resolved edge, not a grep for the tag string.

`refs()` keeps its universe (the text scan) but now **ranks and labels** each hit, cached per workspace and invalidated by the existing `generation` counter (bumped on every ingest/drop) — same cache lifetime as the rest of the working set:

- `local` — a symbol named exactly this is defined in the SAME file.
- `imported` — the file has a resolved edge binding this name to a file that defines it (following one re-export hop, same as `importers()`).
- `textual` — neither; a name in a comment, a string, or one this module could not resolve.
- `call: true` — a stored call-site reference lands on that exact line, independent of the label above.

Order: `local`, then `imported`, then `textual` (a stable sort — ties keep the scan's own file/line order). Existing fields are unchanged; `label` is new (`confidence` keeps its old name, now carrying the same value, for any caller already matching on it).

`importers(file)` is the reverse: `[{ file, line, kind, names, via }]`, `via` naming the barrel file when the edge was found by following a re-export one hop. Surfaced in `tools.js#codeOutline` (`code_outline`'s existing text output gains a `used by:` line) rather than a new model-facing tool, and as its own CLI command (`okcode importers <file>`) — a terminal caller asking "who uses this" is reasonable even though the model-facing surface reuses `code_outline`.

Known, deliberate gaps (conservative choices, not oversights): a destructured top-level `const { x } = require('./y')` is not recorded as an import edge at all (pre-existing in `parse.js`'s acorn extractor, not introduced here — only `const x = require(...)` with a plain identifier is); a multi-specifier statement resolves its target file once but does not currently track more than one named symbol per edge when the statement is not a simple `import { x } from` (`export { a, b } from './m'` resolves `./m`, but only `refs()`'s own per-name check, not the stored edge, distinguishes `a` from `b`); Python's `import a, b` (comma-separated whole-module imports in one statement) only keeps one of the two source texts.
- **References**: `@reference.call` / `@reference.*` captures come back as each file's `references: [{ name, kind, line }]` — recorded for Phase F3 (resolved edges) to use; nothing new is stored for it yet.
