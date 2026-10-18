'use strict';
// A workspace over the native core: the api object hosts and tools use.
const native = require('./native/native');
const { bridgeAccess } = require('./native/access');
const { rethrow, sync } = require('./native/errors');
const { args, fromJson } = require('./native/json');

// The workspace api, in the native core's order.
const METHODS = [
    'refresh', 'scanWarnings', 'sync', 'flush', 'structure', 'packages', 'package', 'find', 'ask', 'files', 'symbols',
    'outline', 'mentions', 'grep', 'isLibraryPath', 'glob', 'refs', 'importers', 'read', 'readFile', 'readAt', 'editBatch',
    'edit', 'writeAt', 'writeWholeFile', 'syntaxCheck', 'stats', 'close',
]; // prettier-ignore
const SYNC = new Set(['isLibraryPath', 'scanWarnings']);

const STORE_HANDLE = Symbol.for('okcode.native.store');

async function openWorkspace({ id, access, store = null, options = {} } = {}) {
    if (typeof id !== 'string' || !id) throw new Error('openWorkspace needs an id');
    for (const m of ['list', 'stat', 'read']) {
        if (!access || typeof access[m] !== 'function') throw new Error(`openWorkspace: access.${m}() is required`);
    }
    const sh = store ? store[STORE_HANDLE] : null;
    if (store && !sh) throw new Error('openWorkspace: store must come from the native openStore');
    const h = await native.openWorkspace(id, bridgeAccess(access), sh || null, args([options || {}])[0]).catch(rethrow);
    return wrapWorkspace(h, { id, access, store });
}

// The JS api object over a native WorkspaceHandle (opened by openWorkspace or by the native okcode).
function wrapWorkspace(h, { id, access, store = null }) {
    const index = h.index;
    const api = { id, access, store: index === 'none' ? null : store, index, readOnly: h.readOnly };
    for (const m of METHODS) {
        api[m] = SYNC.has(m)
            ? (...a) => fromJson(sync(() => native.wsCallSync(h, m, args(a))))
            : (...a) => native.wsCall(h, m, args(a)).then(fromJson, rethrow);
    }
    markToolWs(api, () => native.wsToolHandle(h, access && typeof access.root === 'string' ? access.root : null));
    return api;
}

// The native tool layer (lib/tools.js) renders straight on the native workspace when the api
// object's methods are still the ones it was created with: `markToolWs(api, make)` records them
// and how to make the ToolWsHandle (lazily). A host (or okcode.js) that replaces a method
// afterwards (ws.ask = …) makes the snapshot stale, and lib/tools.js then bridges the object.
// Call markToolWs again after an intended replacement whose native equivalent `make` returns.
const TOOL_WS = Symbol.for('okcode.native.toolWs');
function markToolWs(api, make) {
    const methods = {};
    for (const m of METHODS) methods[m] = api[m];
    Object.defineProperty(api, TOOL_WS, { value: { methods, make }, enumerable: false, configurable: true, writable: true });
    return api;
}
// The native ToolWsHandle of a marked, unmodified api object, else null.
function nativeToolWs(ws) {
    const t = ws && ws[TOOL_WS];
    if (!t) return null;
    for (const m of METHODS) if (ws[m] !== t.methods[m]) return null;
    if (!t.handle) t.handle = t.make();
    return t.handle;
}

function compileGitignore(text) {
    if (!text) return () => false;
    return (rel) => native.wsPure('gitignoreMatches', [String(text), String(rel)]);
}
const pure = (name) => (...a) => fromJson(sync(() => native.wsPure(name, args(a))));

module.exports = {
    openWorkspace,
    wrapWorkspace,
    markToolWs,
    nativeToolWs,
    METHODS,
    compileGitignore,
    carveRegions: pure('carveRegions'),
    splitPiece: pure('splitPiece'),
    classifyCommit: pure('classifyCommit'),
    parseRange: pure('parseRange'),
};
