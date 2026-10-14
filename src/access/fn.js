'use strict';
// Read-only facades built from plain functions or an in-memory map
// (docs/ACCESS.md, "The shipped facades"). Neither has `commit`, so a workspace
// over one refuses every edit (OKCODE_READ_ONLY) — the host's material is
// navigated, never changed through okcode.
//
//   fn({ list, read, stat? })   the host's own functions; stat, when absent,
//                               comes from one list() (plus a read for hashes)
//   memory(map)                 Map<path, { bytes, mtime }> — read live, so a
//                               host may add or replace entries between calls
//
// Everything the contract asks of a facade that the host's functions may not
// do themselves is done here: paths are checked (posix), a listing is
// confined to `under`, skip names and the fixed PRUNE_DOT_DIRS list are
// pruned below it, `limit` is honoured, and text comes back as Buffers.

const { checkPath, sha1, DEFAULT_SKIP, PRUNE_DOT_DIRS } = require('./common');

const PRUNE_DOT_DIRS_SET = new Set(PRUNE_DOT_DIRS);

const toBuffer = (b) =>
    Buffer.isBuffer(b) ? b : b instanceof Uint8Array ? Buffer.from(b) : Buffer.from(String(b), 'utf8');

// The host's read() answer — a Map, or a plain object — as Map<path, Buffer>.
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
    const addressable = (p) => {
        try {
            return check(p) === p;
        } catch {
            return false;
        }
    };

    async function listAll(opts) {
        const rows = (await list(opts)) || [];
        return rows
            .filter((r) => r && typeof r.path === 'string' && addressable(r.path))
            .map((r) => ({ path: r.path, size: Number(r.size) || 0, mtime: r.mtime ?? 0 }));
    }

    return {
        kind,
        dialect: 'posix',
        root,
        checkPath: check,
        async list({ skip = DEFAULT_SKIP, under = null, limit = null } = {}) {
            const base = under ? check(under) : null;
            const names = new Set((skip || []).map(String));
            const out = [];
            for (const r of (await listAll({ ...(base ? { under: base } : {}) })).sort((a, b) =>
                a.path < b.path ? -1 : a.path > b.path ? 1 : 0,
            )) {
                if (base && r.path !== base && !r.path.startsWith(`${base}/`)) continue;
                // Skip names and the FIXED PRUNE_DOT_DIRS list apply BELOW
                // `under`, never to its own segments (docs/ACCESS.md
                // list()) — not every dot-directory: `.github/`,
                // `.storybook/` and the like are real source and must
                // survive this, same as every other facade.
                const below = (base && r.path !== base ? r.path.slice(base.length + 1) : base ? '' : r.path).split('/');
                const dirs = below.slice(0, -1);
                if (dirs.some((d) => names.has(d) || PRUNE_DOT_DIRS_SET.has(d))) continue;
                out.push(r);
                if (limit && out.length >= limit) {
                    out.truncated = true;
                    break;
                }
            }
            return out;
        },
        async stat(paths, { hash = false } = {}) {
            const want = paths.map(check);
            const out = new Map();
            let meta;
            if (typeof stat === 'function') meta = (await stat(want, { hash })) || new Map();
            else {
                const all = new Map((await listAll({})).map((r) => [r.path, r]));
                meta = new Map(want.map((p) => [p, all.get(p) || { missing: true }]));
            }
            meta = meta instanceof Map ? meta : new Map(Object.entries(meta));
            const needHash = hash ? want.filter((p) => meta.get(p) && !meta.get(p).missing && !meta.get(p).hash) : [];
            const bodies = needHash.length ? asBufferMap(await read(needHash)) : new Map();
            for (const p of want) {
                const m = meta.get(p);
                if (!m || m.missing) {
                    out.set(p, { missing: true });
                    continue;
                }
                const row = { size: Number(m.size) || 0, mtime: m.mtime ?? 0 };
                if (hash) {
                    const h = m.hash || (bodies.has(p) ? sha1(bodies.get(p)) : null);
                    if (!h) {
                        out.set(p, { missing: true });
                        continue;
                    }
                    row.hash = h;
                }
                out.set(p, row);
            }
            return out;
        },
        async read(paths) {
            return asBufferMap(await read(paths.map(check)));
        },
    };
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
