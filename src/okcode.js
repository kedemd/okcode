'use strict';
// The host-facing surface (docs/DESIGN.md §9, §10): open okcode on an okdb
// store, register workspaces by the host's id, configure embedder profiles,
// and manage what is indexed — status, sync, reset.
//
// WHAT IS DURABLE AND WHAT IS CODE. The access facade is code (it may hold a
// transport), so the host re-adds its workspaces after every open. What
// okcode keeps durably, in its own okdb env `okcode`, is only what a
// DIFFERENT process needs to answer questions about the index without the
// host's code:
//
//   workspaces  key id     { id, env, added, lastSync, lastScan, access: { kind, root } }
//   embedders   key name   the profile minus functions and secrets (src/embedders.js)
//   settings    key active { name }
//
// ROLES. okdb runs derived work (FTS drains, embedding) only where the
// instance has those roles. Everything here acts on durable state — records,
// cursors, rebuild requests — so status, reset and removal work from a
// passive process (a CLI next to a running service); the process holding the
// roles does the work. okcode never assumes the caller embeds.
//
// ONE INSTANCE PER PATH PER PROCESS. `open({ path })` opens okcode's own
// okdb; a host that already holds an okdb on that path must pass `db`
// instead (two instances on one path in one process deadlock natively).

const OKDB = require('@kedem/okdb');
const { openWorkspace } = require('./workspace');
const { openStore, envNameFor, FILES, SYMBOLS } = require('./store');
const emb = require('./embedders');
const identity = require('./identity');

const META_ENV = 'okcode';
const T_WS = 'workspaces';
const T_EMB = 'embedders';
const T_SET = 'settings';
const FTS_NAME = 'text'; // store.js names both of its full-text indexes this

function codedError(message, code) {
    const err = new Error(message);
    err.code = code;
    return err;
}

// Accepts a logger object, a bare function, or nothing.
function normalizeLog(log) {
    const noop = () => {};
    if (typeof log === 'function') return { info: log, warn: log, error: log, debug: noop };
    const o = log && typeof log === 'object' ? log : {};
    const pick = (k) => (typeof o[k] === 'function' ? o[k].bind(o) : noop);
    return { info: pick('info'), warn: pick('warn'), error: pick('error'), debug: pick('debug') };
}

async function openOrCreateEnv(db, name) {
    try {
        return await db.openEnv(name);
    } catch (err) {
        if (!err || err.code !== 'ENV_NOT_FOUND') throw err;
        return db.createEnvironment(name);
    }
}

async function open({
    path = null,
    db = null,
    role = null,
    embedders: hostProfiles = {},
    active = null,
    workspace: workspaceDefaults = {},
    log,
} = {}) {
    const L = normalizeLog(log);
    if (!db && !path) throw new Error('okcode.open needs a store path or an open okdb instance (db)');
    if (db && path) throw new Error('okcode.open: pass either path or db, not both');
    const ownsDb = !db;
    if (ownsDb) {
        const r = role && typeof role === 'object' ? role : {};
        const flags = {};
        for (const k of ['processors', 'engines', 'compaction']) if (k in r) flags[k] = r[k];
        db = new OKDB(path, flags);
        await db.open();
    } else if (typeof db.env !== 'function' || typeof db.openEnv !== 'function') {
        throw new Error('okcode.open: db must be an open okdb instance');
    }

    let meta;
    try {
        meta = await openOrCreateEnv(db, META_ENV);
        for (const t of [T_WS, T_EMB, T_SET]) await meta.ensureType(t);
    } catch (err) {
        if (ownsDb) await db.close().catch(() => {});
        throw err;
    }

    // Writes that a synchronous call (useEmbedder) starts; close() awaits them.
    const pendingWrites = new Set();
    const track = (p) => {
        const q = Promise.resolve(p).catch((err) => L.warn(`[okcode] persist failed: ${err.message}`));
        pendingWrites.add(q);
        q.finally(() => pendingWrites.delete(q));
        return q;
    };

    // ── profiles ────────────────────────────────────────────────────────
    // name -> { name, record, profile (store profile | null), needsConfig, reason }
    const profiles = new Map();

    const persistProfile = (rec) => meta.put(T_EMB, rec.name, rec);

    for (const [name, cfg] of Object.entries(hostProfiles || {})) {
        const t = emb.fromHost(db, name, cfg);
        profiles.set(name, { name, record: t.record, profile: t.profile, needsConfig: false, reason: null });
        await persistProfile(t.record);
    }
    for (const { key, value } of Array.from(meta.getRange(T_EMB))) {
        if (profiles.has(key) || !value) continue;
        const t = emb.fromRecord(db, value);
        if (!t) continue;
        profiles.set(key, {
            name: key,
            record: t.record,
            profile: t.profile,
            needsConfig: !!t.needsConfig,
            reason: t.reason || null,
        });
        if (t.needsConfig) L.warn(`[okcode] embedder "${key}" needs config: ${t.reason}`);
    }

    const usable = () => [...profiles.values()].filter((p) => !p.needsConfig && p.profile);
    const usableProfiles = () => usable().map((p) => p.profile);

    // The active profile: explicit > persisted > first usable.
    let activeName = null;
    if (active != null) {
        if (!profiles.has(active)) {
            if (ownsDb) await db.close().catch(() => {});
            throw codedError(`no embedder profile named "${active}"`, 'OKCODE_UNKNOWN_EMBEDDER');
        }
        activeName = active;
        await meta.put(T_SET, 'active', { name: activeName });
    } else {
        const stored = meta.get(T_SET, 'active');
        const storedName = stored && stored.name;
        if (storedName && profiles.has(storedName) && !profiles.get(storedName).needsConfig) activeName = storedName;
        else activeName = (usable()[0] && usable()[0].name) || null;
    }

    // ── workspaces ──────────────────────────────────────────────────────
    // id -> { ws, store, access, options }
    const opened = new Map();

    const regGet = (id) => meta.get(T_WS, id) || null;
    const regList = () =>
        Array.from(meta.getRange(T_WS))
            .map(({ value }) => value)
            .filter(Boolean);
    const regPatch = async (id, patch) => {
        const cur = regGet(id);
        if (!cur) return null;
        const next = { ...cur, ...patch };
        await meta.put(T_WS, id, next);
        return next;
    };

    // The workspace env WITHOUT a store (for a workspace not open in this
    // process): null when it does not exist.
    async function envOf(id) {
        const o = opened.get(id);
        if (o) return o.store.env;
        try {
            return await db.openEnv(envNameFor(id));
        } catch (err) {
            if (err && err.code === 'ENV_NOT_FOUND') return null;
            throw err;
        }
    }

    // Drop a workspace's env — removeEnvironment takes its pipelines, engines
    // and vectors with it.
    async function dropEnv(id, store = null) {
        const env = store ? store.env : await envOf(id);
        if (!env) return false;
        if (store) await store.drop();
        else {
            try {
                await db.removeEnvironment(envNameFor(id));
            } catch (err) {
                if (!err || err.code !== 'ENV_NOT_FOUND') throw err;
            }
        }
        return true;
    }

    function assertKnown(id) {
        if (!regGet(id) && !opened.has(id)) throw codedError(`no workspace "${id}"`, 'OKCODE_UNKNOWN_WORKSPACE');
    }

    function requireOpen(id, what) {
        const o = opened.get(id);
        if (!o) {
            throw codedError(
                `workspace "${id}" is not open in this process — ${what} needs its access facade (addWorkspace first)`,
                'OKCODE_WORKSPACE_NOT_OPEN',
            );
        }
        return o;
    }

    // The profile name an ask() resolves to: an explicit one must exist and
    // be usable; none means the active profile.
    function resolveProfile(name) {
        const n = name == null ? activeName : name;
        if (n == null)
            throw codedError('no embedder profile is configured — ask() is unavailable', 'OKCODE_NO_EMBEDDINGS');
        const p = profiles.get(n);
        if (!p) throw codedError(`no embedding profile named "${n}"`, 'OKCODE_NO_EMBEDDINGS');
        if (p.needsConfig)
            throw codedError(`embedding profile "${n}" needs config: ${p.reason}`, 'OKCODE_NO_EMBEDDINGS');
        return n;
    }

    // What a store learned about a profile (its dims) goes into the record, so
    // a process that never ran the probe can still name the pipeline.
    async function learnDims(store) {
        for (const sp of await store.profiles()) {
            const p = profiles.get(sp.name);
            if (!p || !sp.dims || p.record.dims) continue;
            p.record = { ...p.record, dims: sp.dims };
            if (p.profile) p.profile = { ...p.profile, dims: sp.dims };
            await persistProfile(p.record);
        }
    }

    async function openOne(id, access, options) {
        const store = await openStore({ db, id, access, profiles: usableProfiles(), log: (m) => L.warn(m) });
        const ws = await openWorkspace({
            id,
            access,
            store,
            options: { ...(workspaceDefaults || {}), ...(options || {}) },
        });
        // ask() without a profile follows the ACTIVE profile (useEmbedder),
        // not whichever profile the store happened to be given first.
        const innerAsk = ws.ask;
        ws.ask = async (query, opts = {}) => {
            if (!store.hasProfiles()) return innerAsk(query, opts); // its own "unavailable" error
            return innerAsk(query, { ...opts, profile: resolveProfile(opts.profile) });
        };
        opened.set(id, { ws, store, access, options });
        return { ws, store };
    }

    function syncResult(id, r, ms) {
        return {
            id,
            scanned: r.scanned || 0,
            changed: r.reparsed || 0,
            removed: r.removed || 0,
            ms,
        };
    }

    async function addWorkspace(id, { access, options = {} } = {}) {
        envNameFor(id); // validates the id
        if (!access || typeof access.read !== 'function')
            throw new Error(`addWorkspace("${id}") needs an access facade`);
        const prior = opened.get(id);
        if (prior) {
            await prior.ws.close().catch(() => {});
            opened.delete(id);
        }
        const now = Date.now();
        const existing = regGet(id);
        await meta.put(T_WS, id, {
            ...(existing || {}),
            id,
            env: envNameFor(id),
            added: (existing && existing.added) || now,
            lastSync: (existing && existing.lastSync) || null,
            access: { kind: access.kind || null, root: typeof access.root === 'string' ? access.root : null },
        });
        const { ws, store } = await openOne(id, access, options);
        // The open walk: cold → reads and parses everything; warm → one list
        // and a hash only for what moved since the rows were written.
        const t0 = Date.now();
        const r = await ws.refresh();
        await regPatch(id, { lastSync: Date.now(), lastScan: syncResult(id, r, Date.now() - t0) });
        await learnDims(store);
        return ws;
    }

    function workspace(id) {
        const o = opened.get(id);
        return o ? o.ws : null;
    }

    function workspaces() {
        const out = [];
        const seen = new Set();
        for (const r of regList()) {
            seen.add(r.id);
            out.push({ ...r, open: opened.has(r.id) });
        }
        for (const id of opened.keys()) if (!seen.has(id)) out.push({ id, env: envNameFor(id), open: true });
        return out.sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
    }

    async function removeWorkspace(id) {
        const o = opened.get(id);
        const known = !!(o || regGet(id));
        if (o) {
            await o.ws.close().catch(() => {});
            opened.delete(id);
        }
        await dropEnv(id, o ? o.store : null);
        if (regGet(id)) await meta.remove(T_WS, id);
        return { id, removed: known };
    }

    // ── management ──────────────────────────────────────────────────────

    async function sync(id = null, { force = false } = {}) {
        if (id && typeof id === 'object') return sync(null, id);
        const ids = id == null ? [...opened.keys()] : [id];
        const out = [];
        for (const wid of ids) {
            const o = requireOpen(wid, 'sync');
            const t0 = Date.now();
            const r = await o.ws.sync({ force });
            const res = syncResult(wid, r, Date.now() - t0);
            await regPatch(wid, { lastSync: Date.now(), lastScan: res });
            await learnDims(o.store);
            out.push(res);
        }
        return out;
    }

    // The names of every pipeline in a workspace env.
    async function pipelineNames(env) {
        try {
            return ((await env.pipelines.listRecords()) || []).map(({ key, value }) =>
                String((value && value.name) || key || ''),
            );
        } catch {
            return [];
        }
    }

    // One profile's pipeline in one workspace, without a store: named from
    // its identity (type, endpoint, model) and (learned) dims, else found
    // among the env's records by re-checking each candidate's identity hash.
    async function pipelineFor(env, p, names = null) {
        const parts = (p.profile && p.profile.identity) || emb.partsOfRecord(p.record);
        if (!parts || !parts.type) return null;
        return identity.findPipeline(names || (await pipelineNames(env)), parts, p.record.dims || null);
    }

    // okcode's pipelines in a workspace env that no known profile addresses —
    // left behind when a profile's identity changed (a new url, model or
    // dims) or by an older naming scheme. They still hold vectors, and their
    // indexers still embed every change against the OLD embedder, so they are
    // reported (status) and dropped on request (removeOrphaned), never
    // served.
    async function orphansOf(env) {
        const names = await pipelineNames(env);
        const claimed = new Set();
        for (const p of profiles.values()) {
            const f = await pipelineFor(env, p, names);
            if (f) claimed.add(f.pipeline);
        }
        return names.filter((n) => identity.isOurs(n) && !claimed.has(n));
    }

    async function indexerStats(scoped) {
        try {
            const live = db.embeddings.indexer(scoped);
            if (live) return await live.stats();
            return db.embeddings._durableIndexerStats ? db.embeddings._durableIndexerStats(scoped) : null;
        } catch (err) {
            return { error: err.message };
        }
    }

    function ftsState(type, env) {
        try {
            const x = (db.fts.list(type, env) || []).find((i) => i && i.name === FTS_NAME);
            if (!x) return { status: 'absent' };
            return { status: x.status || null, lag: x.lag == null ? null : x.lag, error: x.error || null };
        } catch (err) {
            return { status: 'unavailable', error: err.message };
        }
    }

    function embedderEntry(p, { pipeline = null, dims = null, stats = null, error = null } = {}) {
        const counts = (stats && stats.doc_counts) || {};
        const space = emb.describe(p.record, dims || null);
        const e = {
            name: p.name,
            pipeline,
            type: space.type,
            endpoint: space.endpoint,
            model: space.model,
            dims: space.dims,
            identity: space.identity,
            active: p.name === activeName,
            done: counts.done || 0,
            pending: counts.pending || 0,
            failed: counts.failed || 0,
            vectors: stats && typeof stats.vector_count === 'number' ? stats.vector_count : null,
        };
        if (p.needsConfig) {
            e.state = 'needs-config';
            e.error = p.reason;
        } else if (error || (stats && stats.error)) {
            e.state = 'error';
            e.error = error || stats.error;
        } else if (!pipeline) {
            e.state = 'absent';
        } else {
            e.state = e.pending > 0 ? 'building' : e.failed > 0 ? 'failed' : 'ready';
        }
        return e;
    }

    async function workspaceStatus(id) {
        const reg = regGet(id);
        const o = opened.get(id);
        const env = await envOf(id);
        const out = {
            id,
            env: envNameFor(id),
            open: !!o,
            lastSync: (reg && reg.lastSync) || null,
            files: 0,
            symbols: 0,
            fts: {},
            embedders: [],
            orphaned: [],
        };
        if (!env) {
            out.missing = true;
            return out;
        }
        const count = (t) => {
            try {
                return env.hasType(t) ? env.getCount(t) : 0;
            } catch {
                return 0;
            }
        };
        out.files = count(FILES);
        out.symbols = count(SYMBOLS);
        out.fts = { symbols: ftsState(SYMBOLS, env), files: ftsState(FILES, env) };

        const storeProfiles = o ? new Map((await o.store.profiles()).map((sp) => [sp.name, sp])) : new Map();
        for (const p of profiles.values()) {
            const sp = storeProfiles.get(p.name);
            if (sp && sp.pipeline) {
                out.embedders.push(
                    embedderEntry(p, {
                        pipeline: sp.pipeline,
                        dims: sp.dims,
                        stats: sp.status,
                        error: sp.error,
                    }),
                );
                continue;
            }
            if (sp && sp.error) {
                out.embedders.push(embedderEntry(p, { error: sp.error }));
                continue;
            }
            const found = await pipelineFor(env, p);
            if (!found) {
                out.embedders.push(embedderEntry(p));
                continue;
            }
            const stats = await indexerStats(`${env.name}:${found.pipeline}`);
            out.embedders.push(embedderEntry(p, { pipeline: found.pipeline, dims: found.dims, stats }));
        }
        for (const pipeline of await orphansOf(env)) {
            const stats = await indexerStats(`${env.name}:${pipeline}`);
            const counts = (stats && stats.doc_counts) || {};
            out.orphaned.push({
                pipeline,
                done: counts.done || 0,
                vectors: stats && typeof stats.vector_count === 'number' ? stats.vector_count : null,
            });
        }
        return out;
    }

    // What the host sees about a profile: its vector space and whether this
    // process can use it. No functions, no secrets.
    function profileEntry(p) {
        return {
            name: p.name,
            ...emb.describe(p.record),
            active: p.name === activeName,
            state: p.needsConfig ? 'needs-config' : 'configured',
            ...(p.needsConfig ? { error: p.reason } : {}),
        };
    }

    async function status(id = null) {
        const ids = id == null ? workspaces().map((w) => w.id) : (assertKnown(id), [id]);
        const list = [];
        for (const wid of ids) list.push(await workspaceStatus(wid));
        return {
            workspaces: list,
            active: activeName,
            embedders: [...profiles.values()].map(profileEntry),
            role: db.role ? { processors: db.role.processors !== false, engines: db.role.engines !== false } : null,
        };
    }

    async function reset(id, { scope = 'vectors', embedder = null } = {}) {
        assertKnown(id);
        if (scope === 'vectors') {
            const env = await envOf(id);
            if (!env) return { id, scope, rebuilt: [] };
            const rebuilt = [];
            for (const p of profiles.values()) {
                if (embedder != null && p.name !== embedder) continue;
                if (p.needsConfig) continue;
                const found = await pipelineFor(env, p);
                if (!found || rebuilt.some((r) => r.pipeline === found.pipeline)) continue;
                // Live indexer here → rebuild(); otherwise the durable variant
                // (drop vectors + status, reset the cursor) and whoever runs the
                // indexer re-embeds from clock 0.
                const result = await env.pipelines.rebuild(found.pipeline);
                rebuilt.push({ name: p.name, pipeline: found.pipeline, result });
            }
            return { id, scope, rebuilt };
        }
        if (scope === 'fts') {
            // The content index reads a RESOLVED field, served only where the
            // workspace's access facade is: rebuilding it anywhere else would
            // index nothing and call it ready.
            const o = requireOpen(id, "reset({ scope: 'fts' })");
            const env = o.store.env;
            const drains = !db.role || db.role.processors !== false;
            const out = [];
            for (const type of [SYMBOLS, FILES]) {
                if (!db.fts.has(type, FTS_NAME, env)) continue;
                await db.fts.reset(type, FTS_NAME, true, env);
                if (drains) await db.fts.ready(type, FTS_NAME, env).catch(() => {});
                out.push(type);
            }
            return { id, scope, reset: out };
        }
        if (scope === 'all') {
            const o = opened.get(id);
            if (!o) {
                // No facade here: drop the env; the next addWorkspace rescans.
                await dropEnv(id);
                await regPatch(id, { lastSync: null, lastScan: null });
                return { id, scope, dropped: true, rescanned: false };
            }
            await o.ws.close().catch(() => {});
            opened.delete(id);
            await dropEnv(id, o.store);
            const { ws, store } = await openOne(id, o.access, o.options);
            const t0 = Date.now();
            const r = await ws.refresh();
            const res = syncResult(id, r, Date.now() - t0);
            await regPatch(id, { lastSync: Date.now(), lastScan: res });
            await learnDims(store);
            return { id, scope, dropped: true, rescanned: true, ...res };
        }
        throw new Error(`reset: unknown scope "${scope}" (vectors | fts | all)`);
    }

    async function addEmbedder(name, cfg) {
        const prior = profiles.get(name);
        if (prior && !prior.needsConfig) {
            throw codedError(`embedder "${name}" already exists — removeEmbedder it first`, 'OKCODE_EMBEDDER_EXISTS');
        }
        const t = emb.fromHost(db, name, cfg);
        // A needs-config profile being re-supplied keeps what it learned.
        const record =
            prior && prior.record && !t.record.dims && prior.record.dims
                ? { ...t.record, dims: prior.record.dims }
                : t.record;
        const entry = {
            name,
            record,
            profile: record.dims ? { ...t.profile, dims: record.dims } : t.profile,
            needsConfig: false,
            reason: null,
        };
        profiles.set(name, entry);
        await persistProfile(record);
        if (activeName == null) {
            activeName = name;
            await meta.put(T_SET, 'active', { name });
        }
        const results = [];
        for (const [id, o] of opened) {
            const st = await o.store.addProfile(entry.profile);
            results.push({ id, pipeline: st.pipeline, dims: st.dims, error: st.error || null });
        }
        for (const o of opened.values()) await learnDims(o.store);
        return { name, workspaces: results };
    }

    function useEmbedder(name) {
        const p = profiles.get(name);
        if (!p) throw codedError(`no embedder profile named "${name}"`, 'OKCODE_UNKNOWN_EMBEDDER');
        if (p.needsConfig) throw codedError(`embedder "${name}" needs config: ${p.reason}`, 'OKCODE_NEEDS_CONFIG');
        activeName = name;
        track(meta.put(T_SET, 'active', { name }));
        return name;
    }

    async function removeEmbedder(name) {
        const p = profiles.get(name);
        if (!p) throw codedError(`no embedder profile named "${name}"`, 'OKCODE_UNKNOWN_EMBEDDER');
        const removed = [];
        const ids = new Set([...regList().map((r) => r.id), ...opened.keys()]);
        for (const id of ids) {
            const env = await envOf(id);
            if (!env) continue;
            const found = await pipelineFor(env, p);
            if (!found) continue;
            // Another profile with the same identity shares the pipeline.
            let shared = false;
            for (const q of profiles.values()) {
                if (q === p) continue;
                const f = await pipelineFor(env, q);
                if (f && f.pipeline === found.pipeline) shared = true;
            }
            if (shared) continue;
            await dropPipeline(env, found.pipeline);
            removed.push({ id, pipeline: found.pipeline });
        }
        profiles.delete(name);
        await meta.remove(T_EMB, name);
        if (activeName === name) {
            activeName = (usable()[0] && usable()[0].name) || null;
            if (activeName) await meta.put(T_SET, 'active', { name: activeName });
            else await meta.remove(T_SET, 'active');
        }
        return { name, removed };
    }

    // Remove a pipeline: okdb drops its engines, vectors and doc status.
    async function dropPipeline(env, pipeline) {
        await env.pipelines.remove(pipeline);
    }

    // Drop the orphaned pipelines (status().workspaces[].orphaned) of one
    // workspace, or of every known one. Works from any process.
    async function removeOrphaned(id = null) {
        const ids = id == null ? workspaces().map((w) => w.id) : (assertKnown(id), [id]);
        const removed = [];
        for (const wid of ids) {
            const env = await envOf(wid);
            if (!env) continue;
            for (const pipeline of await orphansOf(env)) {
                await dropPipeline(env, pipeline);
                removed.push({ id: wid, pipeline });
            }
        }
        return { removed };
    }

    // `query` as for ask(): a string, or { vector, text?, identity? } — one
    // vector can only be compared across profiles of the same dims (each
    // profile refuses a mismatch).
    async function compare(id, query, names = null, { limit = 8, text = false } = {}) {
        const o = requireOpen(id, 'compare');
        const list = names == null ? usable().map((p) => p.name) : Array.isArray(names) ? names : [names];
        const out = {};
        for (const n of list) {
            resolveProfile(n);
            out[n] = await o.store.ask(query, { profile: n, limit, text });
        }
        return out;
    }

    async function close() {
        await Promise.all([...pendingWrites]);
        for (const o of opened.values()) await o.ws.close().catch(() => {});
        opened.clear();
        if (ownsDb) await db.close();
    }

    return {
        db,
        meta,
        addWorkspace,
        workspace,
        workspaces,
        removeWorkspace,
        sync,
        status,
        reset,
        addEmbedder,
        useEmbedder,
        removeEmbedder,
        removeOrphaned,
        compare,
        close,
        // The active profile's name (what ask() uses without { profile }).
        get active() {
            return activeName;
        },
        // [{ name, type, endpoint, model, dims, identity, active, state }] —
        // no functions, no secrets. `identity` (src/identity.js) is the
        // vector space: JSON [type, endpoint, model, dims], null until the
        // dims are known. A host holding a query vector passes it to ask()
        // only for a profile whose identity equals its own.
        embedders() {
            return [...profiles.values()].map(profileEntry);
        },
    };
}

module.exports = { open, access: require('./access'), analysis: require('./analysis') };
