'use strict';
// The persistence of one workspace's graph in okdb, over the native core.
// wrapStore(h, …) is also what src/okcode.js uses for a store the native okcode opened.
const native = require('./native/native');
const { bridgeAccess } = require('./native/access');
const { rethrow, sync } = require('./native/errors');
const { args, fromJson, toJson } = require('./native/json');
const { handleOf } = require('./native/okdb');

const STORE_HANDLE = Symbol.for('okcode.native.store');
const FILES = 'files';
const SYMBOLS = 'symbols';
const PACKAGES = 'packages';
const STOPWORDS = [
    'a', 'an', 'and', 'are', 'as', 'at', 'be', 'but', 'by', 'can', 'do', 'does',
    'for', 'from', 'get', 'how', 'i', 'if', 'in', 'into', 'is', 'it', 'its',
    'of', 'on', 'or', 'that', 'the', 'their', 'then', 'there', 'these', 'this',
    'to', 'was', 'we', 'what', 'when', 'where', 'which', 'why', 'will', 'with',
    'you', 'your',
]; // prettier-ignore

// The store API: which methods are synchronous (called on this thread) and which async (threadpool).
const ASYNC = ['saveFiles', 'savePackages', 'addProfile', 'profiles', 'settle', 'ask', 'drop'];
const SYNC = ['load', 'search', 'searchContent', 'contentCandidates', 'hasProfiles'];

async function openStore({ db, id, access, profiles = [], fts = 'full', log = () => {} } = {}) {
    if (!db || typeof db.env !== 'function') throw new Error('openStore needs an open okdb instance (db)');
    if (!access || typeof access.read !== 'function') throw new Error('openStore needs the workspace access facade');
    if (typeof id !== 'string' || !id) throw new Error('a workspace id is required');
    const logFn = typeof log === 'function' ? log : () => {};
    const h = await native.openStore(handleOf(db), id, bridgeAccess(access), { profiles: args([profiles])[0], fts }, (line) => {
        try {
            logFn(line);
        } catch {
            /* a logger must not break the store */
        }
    }).catch(rethrow);
    return wrapStore(h, { db, id, ftsMode: fts, log: logFn });
}

// The JS store object over a native StoreHandle (opened by openStore or by the native okcode).
function wrapStore(h, { db, id, ftsMode = h.ftsMode, log = null }) {
    let env;
    try {
        env = db.env(h.envName);
    } catch {
        env = undefined;
    }
    const st = { id, envName: h.envName, env, db, fts: h.fts, contentFts: h.contentFts, ftsMode, FILES, SYMBOLS, PACKAGES, [STORE_HANDLE]: h };
    st.setTextSource = () => {}; // the native workspace binds its own text cache
    for (const m of ASYNC) st[m] = (...a) => native.storeCall(h, m, args(a)).then(fromJson, rethrow);
    for (const m of SYNC) st[m] = (...a) => fromJson(sync(() => native.storeCallSync(h, m, args(a))));
    // Shapes the core cannot express in JSON: Maps, Sets, Float32Array, defaults for absent options.
    st.load = () => {
        const r = sync(() => native.storeCallSync(h, 'load', []));
        // a package without a name sits under `undefined`
        return { files: new Map(r.files), packages: new Map(r.packages.map(([k, p]) => [p && typeof p === 'object' && 'name' in p ? k : undefined, p])) };
    };
    st.contentCandidates = (literal) => {
        const r = sync(() => native.storeCallSync(h, 'contentCandidates', args([literal])));
        return r == null ? null : new Set(r);
    };
    st.settle = ({ profile = null } = {}) => native.storeCall(h, 'settle', args([{ profile }])).then(() => undefined, rethrow);
    // A row that can't be encoded (a cycle) fails alone, not the batch: it is reported in `failed`.
    st.saveFiles = async (saves = [], removals = []) => {
        // A row that cannot be encoded travels as a marker: native fails it alone (it is already logged here).
        const rows = saves.map((s) => {
            try {
                return toJson(s) ?? null;
            } catch (e) {
                // logged here, before the promise can settle; native stays quiet
                const error = String((e && e.message) || e).slice(0, 300);
                if (log) {
                    try {
                        log(`[okcode] ${s && s.rel}: not written to the store — ${error}`);
                    } catch {
                        /* a logger must not break the store */
                    }
                }
                return { rel: s && s.rel, __unencodable: String((e && e.message) || e), __logged: !!log };
            }
        });
        return fromJson(await native.storeCall(h, 'saveFiles', [rows, args([removals])[0]]).catch(rethrow));
    };
    st.savePackages = (list = []) => native.storeCall(h, 'savePackages', args([list])).then(() => undefined, rethrow);
    st.ask = (query, o = {}) => native.storeCall(h, 'ask', args([query, o || {}])).then(fromJson, rethrow);
    return st;
}

const pure = (name) => (...a) => fromJson(sync(() => native.storePure(name, args(a))));
// asQuery hands the vector back as a Float32Array.
const asQuery = (query) => {
    const q = sync(() => native.storePure('asQuery', args([query])));
    return { text: q.text, vector: q.vector ? Float32Array.from(q.vector) : null, identity: q.identity };
};

module.exports = {
    openStore,
    wrapStore,
    asQuery,
    envNameFor: pure('envNameFor'),
    // `perFile = 3` applies to undefined only (null slices to nothing), which JSON cannot tell apart
    quoteLines: (text, query, perFile = 3) => sync(() => native.storePure('quoteLines', args([text, query, perFile]))),
    queryTerms: pure('queryTerms'),
    STOPWORDS,
    FILES,
    SYMBOLS,
    PACKAGES,
};
