'use strict';
// okcode's public API over the native core. Exports: { open, access, analysis }.
//
// What stays here is what cannot cross as JSON: the host's functions. A profile's `embed` /
// `apiKey` function becomes an okdb embedder factory registered on the (shim) db by
// src/embedders.js#preRegister before the native side translates the profile; the logger is
// normalised to its `warn`; ws/store handles are wrapped into the JS objects src/workspace.js and
// src/store.js build.
const path = require('path');
// open({ path }): okcode's own instance through its @kedem/okdb ^3 dependency (okdb's JS lib owns and pumps it)
const OKDB = require('@kedem/okdb');
const native = require('./native/native');
const { bridgeAccess } = require('./native/access');
const { rethrow, sync } = require('./native/errors');
const { args, fromJson } = require('./native/json');
const { wrapWorkspace, markToolWs } = require('./workspace');
const { wrapStore } = require('./store');
const { handleOf } = require('./native/okdb');

const emb = require('./embedders');

// Accepts a logger object, a bare function, or nothing.
function normalizeLog(log) {
    const noop = () => {};
    if (typeof log === 'function') return { info: log, warn: log, error: log, debug: noop };
    const o = log && typeof log === 'object' ? log : {};
    const pick = (k) => (typeof o[k] === 'function' ? o[k].bind(o) : noop);
    return { info: pick('info'), warn: pick('warn'), error: pick('error'), debug: pick('debug') };
}

const fnKeys = (cfg) => (cfg && typeof cfg === 'object' ? Object.keys(cfg).filter((k) => typeof cfg[k] === 'function') : []);
const hostProfile = (name, cfg) => ({ name, cfg: args([cfg])[0], fnKeys: fnKeys(cfg) });

async function open({
    path: storePath = null,
    db = null,
    role = null,
    license = null,
    embedders: hostProfiles = {},
    active = null,
    workspace: workspaceDefaults = {},
    log,
} = {}) {
    const L = normalizeLog(log);
    if (!db && !storePath) throw new Error('okcode.open needs a store path or an open okdb instance (db)');
    if (db && storePath) throw new Error('okcode.open: pass either path or db, not both');
    const ownsDb = !db;
    if (ownsDb) {
        const r = role && typeof role === 'object' ? role : {};
        const flags = {};
        for (const k of ['processors', 'engines', 'compaction']) if (k in r) flags[k] = r[k];
        db = new OKDB(storePath, flags);
    } else if (typeof db.env !== 'function' || typeof db.openEnv !== 'function') {
        throw new Error('okcode.open: db must be an open okdb instance');
    }
    // before open(): okdb restarts the persisted embedder engines inside it
    emb.preRegister(db, hostProfiles);
    if (ownsDb) await db.open();
    // okcode's core talks to this very instance through okdb's C bridge (throws OKCODE_OKDB_V3_REQUIRED for a v2 db)
    const dbh = handleOf(db);
    const warn = (line) => {
        try {
            L.warn(line);
        } catch {
            /* a logger must not break okcode */
        }
    };
    const options = {
        ownsDb,
        license: license == null ? null : args([license])[0],
        embedders: Object.entries(hostProfiles || {}).map(([n, c]) => hostProfile(n, c)),
        active,
        workspace: args([workspaceDefaults || {}])[0],
    };
    const h = await native.okcodeOpen(dbh, options, warn).catch(async (e) => {
        if (ownsDb) await db.close().catch(() => {});
        rethrow(e);
    });

    const call = (m, a = []) => native.okcodeCall(h, m, args(a)).then(fromJson, rethrow);
    const callSync = (m, a = []) => fromJson(sync(() => native.okcodeCallSync(h, m, args(a))));
    // id -> the JS ws object (the same object for as long as it stays added)
    const wsObjects = new Map();

    async function addWorkspace(id, { access, options: wsOptions = {}, index = 'full', fts } = {}) {
        callSync('checkId', [id]); // an invalid id is refused before anything else
        if (!access || typeof access.read !== 'function') throw new Error(`addWorkspace("${id}") needs an access facade`);
        const r = await native
            .okcodeAddWorkspace(h, id, bridgeAccess(access), {
                options: args([wsOptions || {}])[0],
                index,
                fts: fts ?? null,
                accessKind: access.kind || null,
                accessRoot: typeof access.root === 'string' ? access.root : null,
            })
            .catch((e) => {
                resync(id, access); // the native side may have opened the new ws before failing
                return rethrow(e);
            });
        return build(id, access, r);
    }

    // Make the JS entry for id mirror the native one (after a failure part-way through a reopen).
    function resync(id, access) {
        const r = native.okcodeWorkspace(h, id);
        if (r) build(id, access, r);
        else wsObjects.delete(id);
    }

    // The JS object over native { ws, store } handles; ws.ask follows the ACTIVE profile.
    function build(id, access, r) {
        const store = r.store ? wrapStore(r.store, { db, id }) : null;
        const ws = wrapWorkspace(r.ws, { id, access, store });
        if (store) {
            // ask() without a profile follows the ACTIVE profile. Tied to THIS native workspace:
            // a stale ws object asks its own (closed) workspace.
            ws.ask = (query, o = {}) => {
                const [q, op] = args([query, o]);
                return native.okcodeWsAsk(h, r.ws, r.store, q, op).then(fromJson, rethrow);
            };
            const root = typeof access.root === 'string' ? access.root : null;
            markToolWs(ws, () => native.okcodeToolWs(h, r.ws, r.store, root));
        }
        wsObjects.set(id, ws);
        return ws;
    }

    const api = {
        db,
        meta: db.env('okcode'),
        addWorkspace,
        workspace: (id) => wsObjects.get(id) || null,
        workspaces: () => callSync('workspaces'),
        removeWorkspace: async (id) => {
            const out = await call('removeWorkspace', [id]);
            wsObjects.delete(id);
            return out;
        },
        sync: (id = null, o) => call('sync', [id, o]),
        status: (id = null) => call('status', [id]),
        reset: async (id, o = {}) => {
            let out;
            try {
                out = await call('reset', [id, o]);
            } catch (e) {
                const prior = wsObjects.get(id);
                if (prior) resync(id, prior.access);
                throw e;
            }
            // reset({ scope: 'all' }) reopened the workspace: workspace(id) is the new object
            if (out && out.scope === 'all' && out.rescanned) {
                const prior = wsObjects.get(id);
                const r = prior && native.okcodeWorkspace(h, id);
                if (r) build(id, prior.access, r);
            }
            return out;
        },
        addEmbedder: (name, cfg) => {
            // an existing, usable profile refuses (natively) before anything is re-registered
            if (!callSync('hasEmbedder', [name])) emb.preRegister(db, { [name]: cfg });
            return native.okcodeAddEmbedder(h, hostProfile(name, cfg)).then(fromJson, rethrow);
        },
        useEmbedder: (name) => callSync('useEmbedder', [name]),
        removeEmbedder: (name) => call('removeEmbedder', [name]),
        removeOrphaned: (id = null) => call('removeOrphaned', [id]),
        compare: (id, query, names = null, o = {}) => call('compare', [id, query, names, o]),
        close: async () => {
            await call('close');
            if (ownsDb) await db.close();
            wsObjects.clear();
        },
        get active() {
            return callSync('active');
        },
        embedders: () => callSync('embedders'),
    };
    return api;
}

module.exports = { open, access: require('./access'), analysis: require('./analysis') };
