'use strict';
// The local facade: plain Node `fs/promises` against a folder on this machine.
// Same contract as the shell facade (docs/ACCESS.md), no subprocess per call.

const fs = require('fs');
const fsp = require('fs/promises');
const path = require('path');
const crypto = require('crypto');
const os = require('os');
const { exec: cpExec } = require('child_process');
const { DEFAULT_SKIP, sha1, pathChecker, mtimeOf, procStartTime, acquireLock, selfIdentity } = require('./common');

const tmpName = () => `.okcode-tmp-${process.pid}-${crypto.randomBytes(6).toString('hex')}`;

async function unlinkQuiet(p) {
    try {
        await fsp.unlink(p);
    } catch {
        /* already gone */
    }
}

function localFs(root) {
    if (typeof root !== 'string' || !root) throw new Error('localFs(root): root is required');
    const base = path.resolve(root);
    // The names this facade can address follow THIS machine's filesystem: on
    // Linux/macOS a backslash is an ordinary filename character.
    const dialect = process.platform === 'win32' ? 'windows' : 'posix';
    const checkPath = pathChecker(dialect);
    const abs = (rel) => path.join(base, ...checkPath(rel).split('/'));

    async function list({ skip = DEFAULT_SKIP } = {}) {
        const skipSet = new Set(skip);
        const out = [];
        // Same two rules as the shell dialects: dot-directories BELOW the root
        // (and skip names at any depth) are pruned by the walk itself — never
        // descended — and an entry that cannot be read costs itself alone,
        // never the listing. The root may itself be a dot directory. Symlinks
        // are not followed (no cycles, no walking out of the root).
        async function walk(dirAbs, dirRel) {
            let entries;
            try {
                entries = await fsp.readdir(dirAbs, { withFileTypes: true });
            } catch {
                return;
            }
            const subdirs = [];
            await Promise.all(
                entries.map(async (e) => {
                    const rel = dirRel ? `${dirRel}/${e.name}` : e.name;
                    const full = path.join(dirAbs, e.name);
                    if (e.isDirectory()) {
                        if (!skipSet.has(e.name) && !e.name.startsWith('.')) subdirs.push([full, rel]);
                        return;
                    }
                    if (!e.isFile()) return;
                    try {
                        await fsp.access(full, fs.constants.R_OK);
                        const st = await fsp.stat(full, { bigint: true });
                        if (!st.isFile()) return;
                        out.push({ path: rel, size: Number(st.size), mtime: mtimeOf(st) });
                    } catch {
                        /* unreadable or vanished: skipped */
                    }
                }),
            );
            for (const [full, rel] of subdirs) await walk(full, rel);
        }
        await walk(base, '');
        return out;
    }

    async function stat(paths, { hash = false } = {}) {
        const out = new Map();
        await Promise.all(
            paths.map(async (p) => {
                const file = abs(p);
                try {
                    // stat BEFORE hashing: a write landing in between yields a
                    // newer hash against an older mtime, which the caller's next
                    // stat corrects. The reverse order could pair an OLD hash
                    // with a NEW mtime — a wrong answer that would stick.
                    const st = await fsp.stat(file, { bigint: true });
                    if (!st.isFile()) return out.set(p, { missing: true });
                    const row = { size: Number(st.size), mtime: mtimeOf(st) };
                    if (hash) row.hash = sha1(await fsp.readFile(file));
                    out.set(p, row);
                } catch {
                    out.set(p, { missing: true });
                }
            }),
        );
        // Preserve request order in the Map.
        return new Map(paths.map((p) => [p, out.get(p)]));
    }

    async function read(paths) {
        const got = new Map();
        await Promise.all(
            paths.map(async (p) => {
                const file = abs(p);
                try {
                    const st = await fsp.stat(file);
                    if (!st.isFile()) return;
                    got.set(p, await fsp.readFile(file));
                } catch {
                    /* missing or unreadable: absent */
                }
            }),
        );
        const out = new Map();
        for (const p of paths) if (got.has(p)) out.set(p, got.get(p));
        return out;
    }

    // Publish BYTES to PATH atomically, or refuse (outcomes: docs/ACCESS.md).
    // Bytes are never written to the destination directly: a COMPLETED temp
    // file in the target's own directory is published by link(2) for create
    // (atomic fail-if-exists — never a check-then-write) or rename(2) for
    // replace (atomic within one filesystem; refused, not copied, across a
    // device boundary). The temp file is removed on every path.
    async function commit(p, bytes, { expectedHash = null, create = false } = {}) {
        const file = abs(p);
        const buf = Buffer.isBuffer(bytes) ? bytes : Buffer.from(bytes);
        const dir = path.dirname(file);
        const want = sha1(buf);

        let lst = null;
        try {
            lst = await fsp.lstat(file);
        } catch (err) {
            if (err.code !== 'ENOENT' && err.code !== 'ENOTDIR') return { outcome: 'noatomic', error: err.message };
        }
        // Replace/link would act on the link, not on what it points to.
        if (lst && lst.isSymbolicLink()) return { outcome: 'symlink' };
        if (create) {
            if (lst) return { outcome: 'exists' };
            try {
                await fsp.mkdir(dir, { recursive: true });
            } catch (err) {
                return { outcome: 'noatomic', error: err.message };
            }
        } else if (!lst || !lst.isFile()) {
            return { outcome: 'missing' };
        }

        const tmp = path.join(dir, tmpName());
        try {
            try {
                await fsp.writeFile(tmp, buf, { flag: 'wx' });
                // Mode bits survive an edit (the executable bit, notably).
                if (!create) await fsp.chmod(tmp, lst.mode & 0o7777);
            } catch (err) {
                return { outcome: 'noatomic', error: err.message };
            }

            if (create) {
                try {
                    await fsp.link(tmp, file);
                    return { outcome: 'ok', hash: want };
                } catch (err) {
                    if (err.code === 'EEXIST') return { outcome: 'exists' };
                    try {
                        await fsp.lstat(file);
                        return { outcome: 'exists' };
                    } catch {
                        // No hard links on this filesystem: never fall back to a
                        // clobbering rename.
                        return { outcome: 'noatomic', error: err.message };
                    }
                }
            }

            // The destination is re-hashed HERE, immediately before the rename
            // — the narrowest window this primitive can achieve. It does not
            // close the window against a non-cooperating writer landing between
            // this line and the rename; that is a documented limit, not a
            // guarantee (`lock` narrows it among cooperating writers).
            let current;
            let cur;
            try {
                cur = await fsp.lstat(file);
                if (cur.isSymbolicLink()) return { outcome: 'symlink' };
                if (!cur.isFile()) return { outcome: 'missing' };
                current = sha1(await fsp.readFile(file));
            } catch {
                return { outcome: 'missing' };
            }
            if (expectedHash && current !== String(expectedHash).toUpperCase()) {
                return { outcome: 'stale', current };
            }
            const tst = await fsp.stat(tmp);
            if (tst.dev !== cur.dev) return { outcome: 'noatomic', error: 'temp and target on different devices' };
            try {
                await fsp.rename(tmp, file);
            } catch (err) {
                return { outcome: 'noatomic', error: err.message };
            }
            return { outcome: 'ok', hash: want };
        } finally {
            await unlinkQuiet(tmp);
        }
    }

    async function remove(paths) {
        const files = paths.map(abs);
        await Promise.all(files.map(unlinkQuiet));
    }

    async function liveness(pid) {
        const host = os.hostname();
        const n = Number(pid);
        if (!Number.isInteger(n) || n <= 0) return { host, alive: null, startTime: null };
        try {
            process.kill(n, 0);
        } catch (err) {
            if (err.code === 'ESRCH') return { host, alive: false, startTime: null };
            if (err.code !== 'EPERM') return { host, alive: null, startTime: null };
            // EPERM: alive, owned by another user.
        }
        return { host, alive: true, startTime: procStartTime(n) };
    }

    async function lock(lockPath, id) {
        checkPath(lockPath);
        return acquireLock({ commit, read, remove, liveness, self: selfIdentity() }, lockPath, id);
    }

    // A capability, not a default: the host grants it (docs/ACCESS.md).
    function exec(command) {
        return new Promise((resolve) => {
            cpExec(command, { cwd: base, maxBuffer: 256 * 1024 * 1024, encoding: 'utf8' }, (err, stdout, stderr) => {
                if (!err) resolve({ ok: true, out: stdout });
                else resolve({ ok: false, out: `${stdout || ''}${stderr || ''}` || String(err.message) });
            });
        });
    }

    return { kind: 'localFs', dialect, root: base, checkPath, list, stat, read, commit, lock, remove, exec };
}

module.exports = { localFs };
