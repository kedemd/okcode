'use strict';
// JavaScript access facades → a native access handle.
//
// A plain localFs(root) (tagged by src/access/local-fs.js, none of its methods replaced or wrapped)
// uses the native local facade directly. Anything else is bridged: native worker threads post
// (id, method, args, buffers) here, the facade method runs on this thread and
// the normalised answer goes back through accessReply(id, err, value, buffers).
const native = require('./native');

const TAG = Symbol.for('okcode.native.localFs');
const METHODS = ['list', 'stat', 'read', 'commit', 'lock', 'remove', 'exec', 'filesContaining'];
const cache = new WeakMap();
let lockSeq = 0;
const locks = new Map();

async function run(access, method, a, bufs) {
    switch (method) {
        case 'list': {
            const rows = await access.list({ skip: a.skip, under: a.under ?? null, limit: a.limit || 0 });
            const list = Array.isArray(rows) ? rows : [...(rows || [])];
            return {
                value: {
                    rows: list.map((r) => ({ path: r.path, size: r.size ?? 0, mtime: r.mtime ?? 0 })),
                    truncated: !!(rows && rows.truncated),
                },
            };
        }
        case 'stat': {
            const m = await access.stat(a.paths, { hash: !!a.hash });
            const out = [];
            for (const [p, r] of m || []) out.push([p, r ? { size: r.size, mtime: r.mtime, hash: r.hash ?? null } : null]);
            return { value: out };
        }
        case 'read': {
            const m = await access.read(a.paths);
            const paths = [];
            const bufsOut = [];
            for (const [p, b] of m || []) {
                if (!b) continue;
                paths.push(p);
                bufsOut.push(Buffer.isBuffer(b) ? b : Buffer.from(b));
            }
            return { value: { paths }, bufs: bufsOut };
        }
        case 'commit': {
            const r = await access.commit(a.path, bufs[0], { expectedHash: a.expectedHash ?? null, create: !!a.create });
            return { value: r || {} };
        }
        case 'lock': {
            const r = await access.lock(a.path, a.id);
            if (r && r.ok) {
                const lockId = ++lockSeq;
                locks.set(lockId, r.release);
                return { value: { ok: true, lockId } };
            }
            return { value: { ok: false, reason: (r && r.reason) || 'locked' } };
        }
        case 'release': {
            const rel = locks.get(a.lockId);
            locks.delete(a.lockId);
            if (rel) await rel();
            return { value: null };
        }
        case 'remove':
            await access.remove(a.paths);
            return { value: null };
        case 'checkPath':
            return { value: access.checkPath(a.path) };
        case 'exec':
            return { value: await access.exec(a.command) };
        case 'filesContaining': {
            const s = await access.filesContaining(a.paths, a.text, { ignoreCase: !!a.ignoreCase });
            return { value: s ? [...s] : null };
        }
        default:
            throw new Error(`unknown facade method ${method}`);
    }
}

function bridgeAccess(access) {
    if (!access) return null;
    const hit = cache.get(access);
    if (hit) return hit;
    let handle;
    const tag = access[TAG];
    if (tag && Object.keys(tag.fns).every((m) => access[m] === tag.fns[m])) {
        handle = native.accessLocalFs(tag.root);
    } else {
        const caps = {};
        for (const m of METHODS) caps[m] = typeof access[m] === 'function';
        const call = (id, method, a, bufs) => {
            run(access, method, a || {}, bufs || []).then(
                (r) => id && native.accessReply(id, null, r.value ?? null, r.bufs ?? null),
                (e) => id && native.accessReply(id, String((e && e.message) || e), null, null),
            );
        };
        handle = native.accessJs(
            { kind: access.kind || 'fn', root: typeof access.root === 'string' ? access.root : null, dialect: access.dialect || null, checkPath: typeof access.checkPath === 'function', caps },
            call,
        );
    }
    cache.set(access, handle);
    return handle;
}

module.exports = { bridgeAccess, TAG };
