'use strict';
// The model-facing tools over the native core.
//
// The host's callbacks stay here: the native renderer asks for a workspace / the workspace list /
// a bridged ws method through `host(id, op, payload)` and gets its answer via toolsReply. A ws whose
// api object is native and unmodified
// (lib/workspace.js nativeToolWs) is handed over as a ToolWsHandle and rendered on directly; any
// other object (a fake, a ws with a wrapped method) is registered under a key and bridged.
const native = require('./native/native');
const { args, fromJson } = require('./native/json');
const { nativeToolWs, METHODS } = require('./workspace');

const LOOKUPS = ['code_map', 'code_find', 'code_grep', 'code_glob', 'code_outline', 'code_read', 'code_package', 'code_ask'];
const EDITS = ['code_edit', 'code_write'];
const TOOLS = [...LOOKUPS, ...EDITS];
const isTool = (name) => TOOLS.includes(name);
const isLookup = (name) => LOOKUPS.includes(name);
const isEdit = (name) => EDITS.includes(name);
// A ws result for the native side: an array carrying properties (paged results: total, next,
// truncated, library…) travels as { $arr, $props } like the native workspace's own results.
function enc(v) {
    if (Array.isArray(v)) {
        const extra = Object.keys(v).filter((k) => !/^\d+$/.test(k));
        const items = v.map(enc);
        if (!extra.length) return items;
        const props = {};
        for (const k of extra) props[k] = enc(v[k]);
        return { $arr: items, $props: props };
    }
    if (v && typeof v === 'object' && Object.getPrototypeOf(v) === Object.prototype) {
        const out = {};
        for (const k of Object.keys(v)) out[k] = enc(v[k]);
        return out;
    }
    return v;
}
const pure = (name) => (...a) => fromJson(native.toolsPure(name, args(a)));
const subjectOf = pure('subjectOf');

function createTools({ workspace, workspaces = () => [], grepMaxChars = 12000 } = {}) {
    if (typeof workspace !== 'function') throw new Error('createTools needs workspace: async (hint) => ws | null');
    const known = () => {
        try {
            return [].concat(workspaces() || []).filter(Boolean);
        } catch {
            return [];
        }
    };
    // bridged ws objects by key (released by the native side when its render ends)
    const foreign = new Map();
    let seq = 0;
    const reply = (id, err, value, tw) => native.toolsReply(id, err, value === undefined ? null : value, tw || null);
    async function host(id, op, p) {
        try {
            if (op === 'workspace') {
                const ws = await workspace(p.hint == null ? undefined : p.hint);
                if (!ws) return reply(id, null, null);
                const tw = nativeToolWs(ws);
                if (tw) return reply(id, null, null, tw);
                const key = ++seq;
                foreign.set(key, ws);
                const has = METHODS.filter((m) => typeof ws[m] === 'function');
                const root = ws.access && typeof ws.access.root === 'string' ? ws.access.root : null;
                return reply(id, null, { key, id: ws.id, root, has });
            }
            if (op === 'workspaces') return reply(id, null, args([known()])[0]);
            if (op === 'wsCall') {
                const ws = foreign.get(p.key);
                if (!ws) throw new Error('workspace is gone');
                return reply(id, null, args([enc(await ws[p.method](...p.args.map(fromJson)))])[0]);
            }
            if (op === 'release') {
                foreign.delete(p.key);
                return reply(id, null, null);
            }
            throw new Error(`unknown tools host op ${op}`);
        } catch (e) {
            return reply(id, JSON.stringify({ code: (e && e.code) || null, message: String((e && e.message) || e) }), null);
        }
    }
    const h = native.toolsCreate({ grepMaxChars }, host);
    return {
        TOOLS,
        isTool,
        isLookup,
        isEdit,
        subjectOf,
        // never throws: a failure is a sentence
        render: (name, a = {}) =>
            new Promise((resolve) => {
                let label = '?';
                try {
                    label = String(name);
                } catch {}
                try {
                    native.toolsRender(h, label, args([a && typeof a === 'object' ? a : {}])[0], resolve);
                } catch (err) {
                    resolve(`(${label}) failed: ${String((err && err.message) || err).slice(0, 200)}`);
                }
            }),
        schemas: () => fromJson(native.toolsSchemas(args([known()])[0])),
    };
}

module.exports = {
    createTools,
    TOOLS,
    LOOKUPS,
    EDITS,
    isTool,
    isLookup,
    isEdit,
    subjectOf,
    hitWithin: pure('hitWithin'),
    uniqueAnchors: pure('uniqueAnchors'),
};
