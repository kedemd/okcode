'use strict';
// Persistence for ONE workspace's code graph, in okdb.
//
// One okdb ENVIRONMENT per workspace (docs/DESIGN.md §5): removing a workspace
// is one removeEnvironment, and nothing mixes with the host's data. The env
// name is derived from the host-given id (see `envNameFor`).
//
// Types inside the env:
//   files     key rel                   metadata only — NO content
//   symbols   key rel::symPath::line    what a symbol is called, where, its prose
//   packages  key name                  a dependency's surface
//
// The text okdb indexes is a RESOLVED FIELD (okdb ≥ 2.3): FTS over
// `files.content`, and the embeddings pipelines over `files.prepared` (the
// same text with its path stamped on — see analysis/chunk.js), are served by
// a batch resolver that reads through the workspace's access facade, one
// `read` per batch. okdb never stores what the resolver returns. The row's
// `hash` is its version: a changed hash rewrites the row, and the change feed
// re-indexes it.
//
// The caller supplies an already-open okdb instance. Never open a second
// instance on the same path in one process — two instances against one path
// in one process was measured to deadlock natively ("writer STALL... not
// cancellable from JS" on both). okdb's multi-process safety is a claim about
// separate OS processes, each with one instance.

const crypto = require('crypto');
const chunk = require('./analysis/chunk');
const identity = require('./identity');

const FILES = 'files';
const SYMBOLS = 'symbols';
const PACKAGES = 'packages';
const FTS_NAME = 'text';
const CONTENT = 'content';
const PREPARED = 'prepared';

// The full-text index over what a symbol is CALLED and what its prose says.
//
// The stopword list is the load-bearing part. A caller asks the way the tool
// invites it to — "where is the budget computed" — and strict `and` mode then
// demands that `where`, `is` and `the` appear in the doc prose too, so the
// question that reads most naturally is the one that matches nothing. Dropping
// grammar words at index and query time leaves {budget, computed}, which is
// what was being asked. The alternative, falling back to `or`, answers every
// question with whatever shares a preposition with it.
const STOPWORDS = [
    'a', 'an', 'and', 'are', 'as', 'at', 'be', 'but', 'by', 'can', 'do', 'does',
    'for', 'from', 'get', 'how', 'i', 'if', 'in', 'into', 'is', 'it', 'its',
    'of', 'on', 'or', 'that', 'the', 'their', 'then', 'there', 'these', 'this',
    'to', 'was', 'we', 'what', 'when', 'where', 'which', 'why', 'will', 'with',
    'you', 'your',
]; // prettier-ignore
const SYMBOL_FTS = { fields: ['name', 'path', 'doc', 'signature'], tokenizer: { stopwords: STOPWORDS } };
// The second eye: full text over the FILE, not the symbol. This is what sees
// a comment inside a function body, a string literal, a local variable, and
// the files that have no symbols at all — markdown, json, html — which the
// symbol index cannot represent even in principle. Same stopwords, so a
// question phrased the same way reaches both.
const CONTENT_FTS = { fields: ['rel', CONTENT], tokenizer: { stopwords: STOPWORDS } };

// The okdb environment for a workspace id.
//
// okdb env names become directory names (`:` → `-`) and prefix engine names
// as `<env>:<engine>` — the env is recovered by splitting on the FIRST `:` —
// so an env name must not contain `:`; a leading `~` marks okdb-internal envs.
// The name is therefore `okcode_` + a slug of [a-z0-9_]. A slug that had to
// change the id (case, punctuation, length) gets a short hash of the exact id
// appended, so two ids can never collide on one env ('App' vs 'app', 'a.b' vs
// 'a_b').
const SLUG_MAX = 40;
function envNameFor(id) {
    if (typeof id !== 'string' || !id) throw new Error('a workspace id is required');
    const s = chunk.slug(id).slice(0, SLUG_MAX);
    const exact = s === id;
    const tag = exact ? '' : `_${crypto.createHash('sha1').update(id).digest('hex').slice(0, 8)}`;
    return `okcode_${s || 'ws'}${tag}`;
}

// Deterministic keys, so a re-scan overwrites rather than accumulating. Keyed
// by PATH, not bare name: `createIndex.find` and a top-level `find` in the
// same file are two different symbols that share a name.
const symKey = (rel, symPath, line) => `${rel}::${symPath}::${line}`;

// The query's own terms decide which lines to quote — stopwords are already
// out of the index, so they must be out of this too or every line with "the"
// in it looks like a match.
function queryTerms(query) {
    return String(query)
        .toLowerCase()
        .split(/[^a-z0-9_$]+/i)
        .filter((t) => t.length > 1 && !STOPWORDS.includes(t));
}

// The lines of one file that best match a content query: the line matching
// the MOST of the query's terms, not the first line matching any of them —
// asking about "the worst failure mode" and being shown the first line that
// says "failure" is a citation that argues against itself. `n` travels with
// the line: the caller groups these by the symbol containing them, and needs
// to know which of two lines in the same symbol is the better citation.
function quoteLines(text, query, perFile = 3) {
    const terms = queryTerms(query);
    const lines = String(text || '').split('\n');
    const scored = [];
    for (let i = 0; i < lines.length; i++) {
        const low = lines[i].toLowerCase();
        let n = 0;
        for (const t of terms) if (low.includes(t)) n++;
        if (n) scored.push({ line: i + 1, n, text: lines[i].trim().slice(0, 200) });
    }
    return scored.sort((a, b) => b.n - a.n || a.line - b.line).slice(0, perFile);
}

// A query as ask() takes it: a string, or { text?, vector?, identity? } — a
// vector the host already embedded (so no embed call is made for it), with
// the text the lexical eyes need. Returns { text, vector, identity } with the
// vector as a Float32Array; throws OKCODE_BAD_QUERY when there is neither.
function asQuery(query) {
    const bad = (why) => {
        const err = new Error(`ask: ${why}`);
        err.code = 'OKCODE_BAD_QUERY';
        return err;
    };
    if (typeof query === 'string') return { text: query, vector: null, identity: null };
    if (!query || typeof query !== 'object' || ArrayBuffer.isView(query) || Array.isArray(query)) {
        throw bad('the query must be a string or { text?, vector? }');
    }
    let vector = null;
    if (query.vector != null) {
        const v = query.vector;
        if (v instanceof Float32Array) vector = v;
        else if (Array.isArray(v) || (ArrayBuffer.isView(v) && !(v instanceof DataView))) vector = Float32Array.from(v);
        else throw bad('vector must be a Float32Array (or an array of numbers)');
        if (!vector.length) throw bad('vector is empty');
    }
    const text = typeof query.text === 'string' && query.text.trim() ? query.text : null;
    if (!text && !vector) throw bad('needs text, a vector, or both');
    return { text, vector, identity: typeof query.identity === 'string' ? query.identity : null };
}

// Registered per PROCESS, not per store: a chunk strategy is a function
// reference, so whichever process runs the indexer has to hold it. Cheap and
// idempotent.
function registerChunker(db) {
    db.embeddings.registerChunkStrategy(chunk.STRATEGY, chunk.headedChunks);
}

async function openStore({ db, id, access, profiles = [], log = () => {} } = {}) {
    if (!db || typeof db.env !== 'function') throw new Error('openStore needs an open okdb instance (db)');
    if (!access || typeof access.read !== 'function') throw new Error('openStore needs the workspace access facade');
    const envName = envNameFor(id);

    // get-or-create: openEnv throws ENV_NOT_FOUND for a name never registered,
    // which is exactly the first run.
    let env;
    try {
        env = await db.openEnv(envName);
    } catch (err) {
        if (err && err.code !== 'ENV_NOT_FOUND') throw err;
        env = await db.createEnvironment(envName);
    }

    await env.ensureType(FILES);
    await env.ensureType(SYMBOLS, { indexes: [['file'], ['name']] });
    await env.ensureType(PACKAGES);

    // ── resolved content ────────────────────────────────────────────────
    // The workspace holds the text it has already read (it parsed it), keyed
    // by rel and hash. A resolver batch asks there first and reads through
    // the facade only for what it lacks — a cold remote scan would otherwise
    // read every file twice more (FTS, then each embedding profile).
    let textSource = null;
    async function textsOf(items) {
        const out = new Array(items.length).fill(null);
        const need = [];
        items.forEach(({ row }, i) => {
            // Opaque rows (binary, oversized, secret) are never read: a
            // `.env` must not become tokens in a full-text index.
            if (!row || row.indexed === false || typeof row.rel !== 'string') return;
            const cached = textSource ? textSource(row.rel, row.hash) : undefined;
            if (typeof cached === 'string') out[i] = cached;
            else need.push(i);
        });
        if (need.length) {
            const got = await access.read(need.map((i) => items[i].row.rel));
            for (const i of need) {
                const buf = got.get(items[i].row.rel);
                out[i] = buf ? buf.toString('utf8') : null; // missing or unreadable: no content
            }
        }
        return out;
    }
    await env.resolveField(FILES, CONTENT, null, { batch: textsOf });
    await env.resolveField(FILES, PREPARED, null, {
        batch: async (items) => {
            const texts = await textsOf(items);
            return items.map(({ row }, i) => {
                const t = chunk.prepareText(row && row.rel, texts[i], { indexed: !!row && row.indexed !== false });
                return t || null;
            });
        },
    });

    // ── full text ───────────────────────────────────────────────────────
    // `ensure` is a no-op when the index exists — including when its CONFIG
    // has changed, so a new field or a new tokenizer would sit in the source
    // doing nothing. Compare what is registered against what is wanted, and
    // rebuild only on a real difference. Compare only what WE set, never the
    // whole config object: okdb fills in tokenizer defaults of its own.
    // Whether THIS process drains derived work (FTS, embeddings). The host
    // decides the roles of its okdb instance; okcode never flips them.
    const drains = () => !db.role || db.role.processors !== false;
    const shape = (c) =>
        [((c && c.fields) || []).join(','), (((c && c.tokenizer) || {}).stopwords || []).join(',')].join('|');
    async function ensureFts(type, config) {
        try {
            const current = (db.fts.list(type, env) || []).find((x) => x && x.name === FTS_NAME);
            if (current && shape(current.config) !== shape(config)) {
                await db.fts.drop(type, FTS_NAME, env).catch(() => {});
            }
            await db.fts.ensure(type, FTS_NAME, config, env);
            // The index builds in the background. Searching before it is ready
            // returns nothing and looks exactly like "no such symbol" — so a
            // process that drains waits for it. One that does not (processors
            // off) must not: nothing here would ever make it ready, and the
            // wait never returns. It searches what a draining process built.
            if (drains()) await db.fts.ready(type, FTS_NAME, env).catch(() => {});
            return true;
        } catch (err) {
            log(`[okcode] full-text index on ${type} unavailable: ${err.message}`);
            try {
                return !!db.fts.has(type, FTS_NAME, env);
            } catch {
                return false;
            }
        }
    }
    const fts = await ensureFts(SYMBOLS, SYMBOL_FTS);
    const contentFts = await ensureFts(FILES, CONTENT_FTS);

    // ── embeddings profiles ─────────────────────────────────────────────
    registerChunker(db);
    // profile name -> { name, model, parts, dims, identity, pipeline, scoped, embedderName, error }
    const profileState = new Map();

    const modelOf = (cfg) => (cfg && (cfg.model || cfg.type)) || null;

    // The dimension a previous run already committed to, read off the
    // pipeline record it left behind. The name encodes the identity (and the
    // dims readably) precisely so this is answerable without asking the
    // model — which matters most to a process that must NOT ask (no engines
    // role).
    async function dimsFromRecords(parts) {
        try {
            const names = ((await env.pipelines.listRecords()) || []).map(({ key, value }) =>
                String((value && value.name) || key || ''),
            );
            const hit = identity.findPipeline(names, parts);
            return hit ? hit.dims : null;
        } catch {
            return null; // no pipelines yet
        }
    }

    async function ensureProfile(profile) {
        const { name, embedder } = profile || {};
        if (!name) throw new Error('an embedding profile needs a name');
        if (!embedder || typeof embedder !== 'object') throw new Error(`profile "${name}" needs an embedder config`);
        const model = modelOf(embedder);
        // The vector space this profile embeds into, minus the dims (below).
        // okcode's profiles carry it (the provider, not a derived factory
        // type); a bare store profile falls back to its embedder config.
        const parts = profile.identity || identity.partsOf(embedder);
        const st = {
            name,
            model,
            parts,
            dims: null,
            identity: null,
            pipeline: null,
            scoped: null,
            embedderName: null,
            error: null,
        };
        profileState.set(name, st);
        try {
            let dims = profile.dims || embedder.dims || null;
            if (!dims && db.embeddings.resolveModelDims)
                dims = db.embeddings.resolveModelDims(parts.type, parts.model || embedder.model) || null;
            if (!dims) dims = await dimsFromRecords(parts);
            let embedderRef = null;
            if (!dims) {
                // Dimensionality is the model's to state, not ours to assume.
                // Start the embedder on its own (probing it) and hand the
                // same engine to the pipeline. Named after the space too: a
                // url change must probe the NEW endpoint, not reuse an
                // engine still pointed at the old one.
                const space = identity.shortHash(JSON.stringify([parts.type, parts.endpoint, parts.model]));
                const embName = `${envName}:emb_${chunk.slug(name)}_${space}`;
                const engine =
                    db.engines.getEngine?.('embedder', embName) ||
                    (await db.embeddings.createEmbedder(embName, embedder, {}, envName));
                const health = engine && engine.api && (await engine.api.health?.());
                dims = (engine && engine.api && engine.api.dims) || (health && health.dims) || null;
                if (!dims) throw new Error(`could not learn the dimensions of ${model} — pass dims`);
                embedderRef = { name: embName };
            }
            const pipeline = identity.pipelineName(parts, dims);
            const scoped = `${envName}:${pipeline}`;
            Object.assign(st, { dims, pipeline, scoped, identity: identity.identityOf(parts, dims) });
            const existing = await env.pipelines.getRecord(pipeline);
            if (!existing) {
                await db.embeddings.createPipeline(pipeline, {
                    ...(profile.pipeline || {}),
                    source_type: FILES,
                    source_env: envName,
                    field: PREPARED,
                    chunk: { strategy: chunk.STRATEGY, size: chunk.CHUNK },
                    dims,
                    embedder: embedderRef || { ...embedder, dims },
                });
            }
            const rec = await env.pipelines.getRecord(pipeline);
            st.embedderName = (rec?.engines || []).find((e) => e.role === 'embedder')?.name || null;
            // The embedder is a lifecycle-skipped member of the pipeline
            // record, so bringing a pipeline back up may start the indexer
            // but not the thing it embeds with. Start it explicitly wherever
            // engines run; a process without engines only searches (okdb
            // embeds a query locally from the persisted embedder record).
            if (db.role?.engines !== false && st.embedderName) {
                const eng = db.engines.getEngine?.('embedder', st.embedderName);
                if (eng && !eng.isRunning && typeof eng.start === 'function') await eng.start().catch(() => {});
            }
        } catch (err) {
            st.error = err.message;
            log(`[okcode] embedding profile ${name} unavailable: ${err.message}`);
        }
        return st;
    }
    for (const p of profiles) await ensureProfile(p);

    function profileFor(name) {
        if (!profileState.size) return null;
        if (name == null) return [...profileState.values()].find((p) => !p.error) || null;
        return profileState.get(name) || null;
    }

    return {
        id,
        envName,
        env,
        db,
        fts,
        contentFts,
        FILES,
        SYMBOLS,
        PACKAGES,

        // Where resolvers look first for text the workspace already holds:
        // fn(rel, hash) → string | undefined.
        setTextSource(fn) {
            textSource = typeof fn === 'function' ? fn : null;
        },

        // Everything known about the workspace, rehydrated into the shape the
        // index works with in memory. Symbols arrive grouped by file.
        load() {
            const files = new Map();
            for (const { key, value } of env.getRange(FILES)) {
                if (!value) continue;
                files.set(key, {
                    ...value,
                    path: key,
                    rel: key,
                    symbols: [],
                    imports: value.imports || [],
                    exports: value.exports || [],
                });
            }
            for (const { value } of env.getRange(SYMBOLS)) {
                const f = value && files.get(value.file);
                if (f) f.symbols.push(value);
            }
            for (const f of files.values()) f.symbols.sort((a, b) => a.lineStart - b.lineStart);
            const packages = new Map();
            for (const { value } of env.getRange(PACKAGES)) if (value) packages.set(value.name, value);
            return { files, packages };
        },

        // Write-through for a batch of files, in ONE transaction — measured
        // 77x faster than a put per row, which matters because a cold build
        // of a real project is hundreds of files at once.
        //
        // okdb's transaction() returns a BUILDER: operations are staged on it
        // synchronously and committed once. It does NOT take a callback.
        async saveFiles(saves = [], removals = []) {
            if (!saves.length && !removals.length) return;
            const txn = env.transaction();
            // One indexed lookup per touched file, never a scan of every
            // symbol per file.
            const staleSymbols = (rel) => env.query(SYMBOLS, { file: rel }, { index: ['file'], prefix: [rel] });
            for (const rel of removals) {
                txn.remove(FILES, rel);
                for (const { key } of staleSymbols(rel)) txn.remove(SYMBOLS, key);
            }
            for (const file of saves) {
                // Stale symbol rows go first, so a symbol deleted from the
                // source does not linger in the graph.
                for (const { key } of staleSymbols(file.rel)) txn.remove(SYMBOLS, key);
                txn.put(FILES, file.rel, {
                    rel: file.rel,
                    hash: file.hash,
                    size: file.size,
                    mtime: file.mtime,
                    lang: file.lang,
                    lines: file.lines,
                    parsed: !!file.parsed,
                    // false = opaque (binary, oversized, secret): hashed, never
                    // read — and the resolver refuses to read it too.
                    indexed: file.indexed !== false,
                    reason: file.reason || null,
                    imports: file.imports || [],
                    exports: file.exports || [],
                    // Which version of an extension's analyser produced this
                    // file's SYMBOLS — null for anything not extension-owned.
                    // Symbols persist and drive resolve/find/editBatch the
                    // instant a process starts, so a stale row is caught the
                    // way a stale hash is: the scan compares this against the
                    // extension's CURRENT version and re-ingests on mismatch.
                    analyzerVersion: file.analyzerVersion || null,
                    // Whether the text round-trips byte-for-byte as UTF-8. A
                    // strict write refuses to compose an edit onto a lossy
                    // decode; persisting it keeps that refusal after a restart.
                    lossless: file.lossless !== false,
                });
                for (const s of file.symbols || []) {
                    txn.put(SYMBOLS, symKey(file.rel, s.path || s.name, s.lineStart), {
                        file: file.rel,
                        name: s.name,
                        kind: s.kind,
                        lineStart: s.lineStart,
                        lineEnd: s.lineEnd,
                        // Character offsets, so a symbol rehydrated after a
                        // restart reads and edits exactly the same region a
                        // symbol from a fresh scan does.
                        start: Number.isInteger(s.start) ? s.start : null,
                        end: Number.isInteger(s.end) ? s.end : null,
                        path: s.path || s.name,
                        parent: s.parent || null,
                        signature: s.signature || null,
                        doc: s.doc || '',
                        exported: !!s.exported,
                    });
                }
            }
            await txn.commit();
            // FTS indexing is asynchronous — a search issued straight after a
            // write can miss it. Waiting for BOTH indexes here is what makes
            // find() read its own writes — where this process drains them.
            if (drains()) {
                if (fts) await db.fts.flush(SYMBOLS, env).catch(() => {});
                if (contentFts) await db.fts.flush(FILES, env).catch(() => {});
            }
        },

        // The dependency surface. Separate from files and symbols because it
        // has a different lifetime: source changes on every edit, a
        // dependency set changes on an install.
        async savePackages(list = []) {
            if (!list.length) return;
            const txn = env.transaction();
            for (const p of list) txn.put(PACKAGES, p.name, p);
            await txn.commit();
        },

        // Full-text over name + doc + signature. null when FTS is unavailable,
        // so the caller uses its own substring path.
        //
        // The MODE is the caller's, deliberately: `and` requires every term,
        // and a loose `or` pass is needed too — but deciding that per index
        // was wrong: a symbol index that fell back to `or` on its own
        // returned three unrelated symbols that OUTRANKED an exact content
        // match. The fallback belongs to whoever can see both answers.
        search(query, limit = 20, mode = 'and') {
            if (!fts) return null;
            try {
                const hits = env.ftsQuery(SYMBOLS, FTS_NAME, String(query), {}, { limit, mode });
                // Carry the RELEVANCE out with the row: it is how a loose
                // query ranks its best match first.
                return (hits || []).filter((h) => h && h.value).map((h) => ({ ...h.value, relevance: h.score || 0 }));
            } catch {
                return null;
            }
        },

        // Full text over file CONTENT, at file granularity: [{ file, hash,
        // lang, relevance }]. Quoting the matching lines needs the text,
        // which the store does not hold — the workspace does that
        // (`quoteLines`), against text it has verified.
        searchContent(query, limit = 10, { mode = 'and' } = {}) {
            if (!contentFts) return null;
            try {
                return [...env.ftsQuery(FILES, FTS_NAME, String(query), {}, { limit, mode })]
                    .filter((h) => h && h.value)
                    .map((h) => ({ file: h.key, hash: h.value.hash, lang: h.value.lang, relevance: h.score || 0 }));
            } catch {
                return null;
            }
        },

        // ── embeddings ──────────────────────────────────────────────────
        hasProfiles() {
            return [...profileState.values()].some((p) => !p.error);
        },

        async addProfile(profile) {
            return ensureProfile(profile);
        },

        // Per profile: name, model, pipeline, dims, and the indexer's own
        // progress (live where this process runs it, durable otherwise).
        async profiles() {
            const out = [];
            for (const p of profileState.values()) {
                let status = null;
                if (p.scoped) {
                    try {
                        const live = db.embeddings.indexer(p.scoped);
                        status = live ? await live.stats() : (db.embeddings._durableIndexerStats?.(p.scoped) ?? null);
                    } catch (err) {
                        status = { error: err.message };
                    }
                }
                out.push({
                    name: p.name,
                    model: p.model,
                    type: p.parts.type || null,
                    endpoint: p.parts.endpoint || null,
                    pipeline: p.pipeline,
                    dims: p.dims,
                    identity: p.identity,
                    error: p.error,
                    status,
                });
            }
            return out;
        },

        // Wait for a profile's indexer to drain what it has been handed. For a
        // test, a CLI run or a deliberate rebuild — never a read path.
        async settle({ profile = null } = {}) {
            const list = profile ? [profileFor(profile)].filter(Boolean) : [...profileState.values()];
            for (const p of list) {
                const idx = p.scoped && db.embeddings.indexer(p.scoped);
                if (idx) await idx.flush();
            }
        },

        // Semantic search in one profile's pipeline. Returns FILE locations:
        // [{ file, hash, score, chunkHash, start, end }] with start/end as
        // offsets into the file's text as it was when embedded (the chunk
        // manifest's offsets, minus the path stamp). Turning that into a
        // symbol is the workspace's job, against text it has verified. With
        // `text: true`, each hit also carries the chunk's text, re-derived
        // through the resolver and returned only if the file still holds it.
        //
        // `query` is a string (embedded here, by the profile's embedder) or
        // { vector, text?, identity? } — a vector the host already embedded,
        // searched as-is with NO embed call. It must have the profile's dims
        // (OKCODE_DIMS_MISMATCH otherwise — a wrong-length vector is a wrong
        // space, never "close enough"), and when the host says which space
        // it came from (`identity`) that must be this profile's
        // (OKCODE_IDENTITY_MISMATCH).
        async ask(query, { profile = null, limit = 8, text = false } = {}) {
            const q = asQuery(query);
            const p = profileFor(profile);
            if (!p) {
                const err = new Error(
                    profile == null
                        ? 'no embedding profile is configured for this workspace — ask() is unavailable'
                        : `no embedding profile named "${profile}"`,
                );
                err.code = 'OKCODE_NO_EMBEDDINGS';
                throw err;
            }
            if (p.error) {
                const err = new Error(`embedding profile "${p.name}" is unavailable: ${p.error}`);
                err.code = 'OKCODE_NO_EMBEDDINGS';
                throw err;
            }
            if (q.vector) {
                if (q.vector.length !== p.dims) {
                    const err = new Error(
                        `embedding profile "${p.name}" holds ${p.dims}-dim vectors; the query vector has ${q.vector.length}`,
                    );
                    err.code = 'OKCODE_DIMS_MISMATCH';
                    throw err;
                }
                if (q.identity && q.identity !== p.identity) {
                    const err = new Error(
                        `embedding profile "${p.name}" is ${p.identity}; the query vector is from ${q.identity}`,
                    );
                    err.code = 'OKCODE_IDENTITY_MISMATCH';
                    throw err;
                }
            }
            const api = db.embeddings.search(p.scoped);
            const raw = (await api.search(q.vector || q.text, { limit: limit * 3 })) || [];
            // Best chunk per file.
            const best = new Map();
            for (const r of raw) {
                const [file, chunkHash] = String(r.key || '').split('\t');
                if (!file) continue;
                const prior = best.get(file);
                if (prior && prior.score >= r.score) continue;
                best.set(file, { file, chunkHash: chunkHash || null, score: r.score });
            }
            const out = [];
            for (const hit of [...best.values()].sort((a, b) => b.score - a.score).slice(0, limit)) {
                const row = env.get(FILES, hit.file);
                if (!row) continue; // the file left the graph since it was embedded
                let meta = null;
                try {
                    meta = await db.embeddings.describeChunk(p.scoped, hit.file, hit.chunkHash, text ? row : undefined);
                } catch {
                    meta = null;
                }
                // No chunk record: the vector is orphaned (the file was
                // re-chunked since) and its text no longer exists. Drop it.
                if (!meta) continue;
                const off = chunk.markLength(hit.file);
                out.push({
                    file: hit.file,
                    hash: row.hash,
                    score: hit.score,
                    chunkHash: hit.chunkHash,
                    start: typeof meta.start === 'number' ? Math.max(0, meta.start - off) : null,
                    end: typeof meta.end === 'number' ? Math.max(0, meta.end - off) : null,
                    ...(text && typeof meta.text === 'string' ? { text: meta.text } : {}),
                });
            }
            return out;
        },

        // Remove the workspace env entirely — rows, full-text indexes,
        // pipelines and their vectors. The store is unusable afterwards.
        async drop() {
            return db.removeEnvironment(envName);
        },
    };
}

module.exports = {
    openStore,
    asQuery,
    envNameFor,
    quoteLines,
    queryTerms,
    STOPWORDS,
    FILES,
    SYMBOLS,
    PACKAGES,
};
