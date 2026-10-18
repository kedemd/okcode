'use strict';
// Internal: the JavaScript face of a native facade — arguments in, the documented facade shapes out
// (Map of rows, Map of Buffers, array with `.truncated`).
const { toJson } = require('../native/json');

// Native errors arrive as plain Error(message) with a native status code; the facade's own errors
// carry no code.
const plain = (e) => {
    const err = new Error(e && e.message !== undefined ? e.message : String(e));
    return err;
};

function methods(nf, checkPath) {
    const call = (method, args, bufs) => nf.call(method, toJson(args) ?? null, bufs).catch((e) => Promise.reject(plain(e)));
    return {
        call,
        async list({ skip, under = null, limit = 0 } = {}) {
            const r = await call('list', { skip, under, limit: Number(limit) });
            if (r.truncated) r.rows.truncated = true;
            return r.rows;
        },
        async stat(paths, { hash = false } = {}) {
            const rows = await call('stat', { paths, hash: !!hash });
            return new Map(rows.map(([p, r]) => [p, r || { missing: true }]));
        },
        async read(paths) {
            const [names, bufs] = await call('read', { paths });
            return new Map(names.map((p, i) => [p, bufs[i]]));
        },
        async commit(p, bytes, { expectedHash = null, create = false } = {}) {
            const buf = Buffer.isBuffer(bytes) ? bytes : Buffer.from(bytes);
            return call('commit', { path: p, expectedHash: expectedHash ? String(expectedHash) : null, create: !!create }, [buf]);
        },
        async lock(lockPath, id) {
            checkPath(lockPath);
            const r = await call('lock', { path: lockPath, id });
            if (!r.ok) return { ok: false, reason: r.reason };
            return {
                ok: true,
                release: async () => {
                    try {
                        await call('release', { lockId: r.lockId });
                    } catch {
                        /* best effort */
                    }
                },
            };
        },
        async remove(paths) {
            await call('remove', { paths });
        },
        exec: (command) => call('exec', { command: String(command) }),
    };
}

module.exports = { methods, plain };
