'use strict';
// Read-only facades built from plain functions or an in-memory map. The filtering, sorting,
// pruning, limit and hashing are native; this file only holds the
// host's functions (answering the native side's calls) and turns their answers into plain data.
const native = require('../native/native');
const { checkPath } = require('./common');
const { methods } = require('./_facade');

const toBuffer = (b) =>
    Buffer.isBuffer(b) ? b : b instanceof Uint8Array ? Buffer.from(b) : Buffer.from(String(b), 'utf8');

function asBufferMap(got) {
    const out = new Map();
    if (!got) return out;
    const entries = got instanceof Map ? got.entries() : Object.entries(got);
    for (const [p, b] of entries) if (b != null) out.set(p, toBuffer(b));
    return out;
}

function fn({ list, read, stat = null, kind = 'fn', root = null } = {}) {
    if (typeof list !== 'function' || typeof read !== 'function') {
        throw new Error('access.fn({ list, read, stat? }): list and read are required');
    }
    const check = (p) => checkPath(p, 'posix');

    async function host(kindName, a) {
        switch (kindName) {
            case 'list': {
                const rows = (await list(a)) || [];
                return {
                    value: rows.map((r) => ({ path: r && r.path, size: Number(r && r.size) || 0, mtime: Number(r && r.mtime) || 0 })),
                };
            }
            case 'read': {
                const got = asBufferMap(await read(a.paths));
                return { value: { paths: [...got.keys()] }, bufs: [...got.values()] };
            }
            case 'stat': {
                let meta = (await stat(a.paths, { hash: !!a.hash })) || new Map();
                meta = meta instanceof Map ? meta : new Map(Object.entries(meta));
                return {
                    value: a.paths.map((p) => {
                        const m = meta.get(p);
                        if (!m || m.missing) return [p, null];
                        return [p, { size: Number(m.size) || 0, mtime: Number(m.mtime) || 0, hash: m.hash || null }];
                    }),
                };
            }
            default:
                throw new Error(`unknown host call ${kindName}`);
        }
    }
    // A value the native side cannot convert must still answer the pending id (else a worker blocks forever).
    const reply = (id, r) => {
        try {
            native.facadeReply(id, null, r.value ?? null, r.bufs ?? null);
        } catch (err) {
            native.facadeReply(id, String((err && err.message) || err), null, null);
        }
    };
    const dispatch = (id, kindName, a) => {
        host(kindName, a).then(
            (r) => reply(id, r),
            (e) => native.facadeReply(id, String((e && e.message) || e), null, null),
        );
    };
    const nf = native.facadeFn({ kind, root, stat: typeof stat === 'function' }, dispatch);
    const m = methods(nf, check);
    return { kind, dialect: 'posix', root, checkPath: check, list: m.list, stat: m.stat, read: m.read };
}

function memory(map) {
    if (!(map instanceof Map)) throw new Error('access.memory(map): a Map<path, { bytes, mtime }> is required');
    const entry = (p) => {
        const e = map.get(p);
        return e == null ? null : Buffer.isBuffer(e) || typeof e === 'string' ? { bytes: e, mtime: 0 } : e;
    };
    return fn({
        kind: 'memory',
        list: async () =>
            [...map.keys()].map((path) => {
                const e = entry(path);
                return { path, size: toBuffer(e.bytes ?? '').length, mtime: e.mtime ?? 0 };
            }),
        read: async (paths) => {
            const out = new Map();
            for (const p of paths) {
                const e = entry(p);
                if (e && e.bytes != null) out.set(p, toBuffer(e.bytes));
            }
            return out;
        },
    });
}

module.exports = { fn, memory };
