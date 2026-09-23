'use strict';
// The shell facade: every byte in and out of a workspace goes through a shell
// script, in one of two dialects (bash, PowerShell), over any transport the
// host supplies as `run(script, stdin) → Promise<stdout>`. The target needs
// nothing installed — a stock bash (GNU or BSD userland) or PowerShell.
//
// Why base64 and not text. A naive text round-trip through a console codepage
// corrupts files (U+2014 arriving as d7 92 e2 82 ac...). Moving bytes rather
// than text makes that impossible, and the wire payload is [A-Za-z0-9+/=] so
// nothing needs escaping in either dialect.
//
// Why stdin and not the command line. A base64 payload of any real file blows
// past command-line limits (~32k on Windows), and chunking a write is how a
// half-applied edit happens. So the scripts are STATIC per method (they live in
// ./scripts/<dialect>/): root, paths, skip names and payloads all travel on
// stdin, nothing is interpolated into a script, and quoting the script is the
// only quoting a transport ever needs (`shell.quote`).
//
// Framing (request):
//   bash        NUL-terminated fields (paths are arbitrary bytes except NUL;
//               `read -d ''` splits them with no per-field fork), then, for
//               commit/exec, a base64 tail read to end of input.
//   powershell  one base64 (UTF-8) field per line, keeping stdin pure ASCII,
//               then the base64 tail as its own line.
// Every request starts with the root. Responses either reference request paths
// by their INDEX or carry paths only inside base64 (bash list/stat: NUL-separated
// records), so a path never crosses a console codepage as text.

const fs = require('fs');
const path = require('path');
const { spawn } = require('child_process');
const { DEFAULT_SKIP, sha1, checkPath, acquireLock, selfIdentity } = require('./common');

// Load a dialect's scripts once. Full-line comments are stripped so the
// documentation in the script files does not travel over the wire on every call.
function loadScripts(dialect, ext, compose) {
    const dir = path.join(__dirname, 'scripts', dialect);
    const read = (name) =>
        fs
            .readFileSync(path.join(dir, `${name}.${ext}`), 'utf8')
            .split('\n')
            .filter((l) => !/^\s*#/.test(l) && l.trim() !== '')
            .join('\n');
    const out = {};
    for (const [method, parts] of Object.entries(compose)) out[method] = parts.map(read).join('\n') + '\n';
    return out;
}

const lines = (raw) => String(raw).split(/\r?\n/);

// bash stat answer: base64 of NUL-separated records keyed by "./<rel>" (see
// scripts/bash/stat.sh). Returns Map<request index, row>.
function parseStatRecords(raw, clean, hash) {
    const t = Buffer.from(String(raw).replace(/\s+/g, ''), 'base64').toString('utf8').split('\0');
    const meta = new Map();
    const hashes = new Map();
    for (let i = 0; i < t.length;) {
        if (t[i] === 'S' || t[i] === 'H') {
            (t[i] === 'S' ? meta : hashes).set(t[i + 1], t[i + 2]);
            i += 3;
            continue;
        }
        // `sha1sum -z`: "<hash>  <path>" (or "<hash> *<path>" in binary mode)
        const m = /^([0-9a-fA-F]{40}) [ *]([\s\S]*)$/.exec(t[i]);
        if (m) hashes.set(m[2], m[1]);
        i++;
    }
    const rows = new Map();
    clean.forEach((rel, i) => {
        const key = `./${rel}`;
        const sm = meta.get(key);
        if (!sm || (hash && !hashes.has(key))) return rows.set(i, { missing: true });
        const [size, mtime] = sm.split(' ');
        const row = { size: Number(size), mtime: Number(mtime) };
        if (hash) row.hash = hashes.get(key).toUpperCase();
        rows.set(i, row);
    });
    return rows;
}

// PowerShell stat answer: "i size ticks [hash]" or "i -" per request index.
function parseStatLines(raw, clean, hash) {
    const rows = new Map();
    for (const line of lines(raw)) {
        const [i, size, mtime, h] = line.trim().split(/\s+/);
        if (i === '' || i === undefined) continue;
        if (size === '-' || size === undefined) rows.set(Number(i), { missing: true });
        else {
            const row = { size: Number(size), mtime: Number(mtime) };
            if (hash) row.hash = String(h).toUpperCase();
            rows.set(Number(i), row);
        }
    }
    return rows;
}

// Batched bash read: base64(NUL pairs of path,size) "@DATA" base64(bytes).
// Returns Map<request index, Buffer>, or null when the sizes do not add up.
function parseReadBatched(raw, clean) {
    const at = raw.startsWith('@DATA\n') ? 0 : raw.indexOf('\n@DATA\n') + 1;
    const head = Buffer.from(raw.slice(0, at).replace(/\s+/g, ''), 'base64').toString('utf8').split('\0');
    const data = Buffer.from(raw.slice(at + 6).replace(/\s+/g, ''), 'base64');
    const byPath = new Map();
    let off = 0;
    for (let i = 0; i + 1 < head.length; i += 2) {
        const size = Number(head[i + 1]);
        if (!Number.isInteger(size) || size < 0 || off + size > data.length) return null;
        byPath.set(head[i], data.subarray(off, off + size));
        off += size;
    }
    if (off !== data.length) return null;
    const out = new Map();
    clean.forEach((rel, i) => {
        const b = byPath.get(`./${rel}`);
        if (b) out.set(i, Buffer.from(b));
    });
    return out;
}

// Per-file read answer: "@i" then base64 lines; a "!" line voids the entry.
function parseReadEach(raw) {
    const chunks = new Map();
    let cur = null;
    for (const line of lines(raw)) {
        const t = line.trim();
        if (t.startsWith('@')) {
            cur = Number(t.slice(1));
            chunks.set(cur, []);
        } else if (t === '!') {
            chunks.delete(cur); // the read failed part-way
            cur = null;
        } else if (cur !== null && t) chunks.get(cur).push(t);
    }
    const out = new Map();
    for (const [i, parts] of chunks) out.set(i, Buffer.from(parts.join(''), 'base64'));
    return out;
}

const DIALECTS = {
    bash: {
        scripts: loadScripts('bash', 'sh', {
            list: ['_root', '_helpers', 'list'],
            stat: ['_root', '_helpers', 'stat'],
            read: ['_root', '_helpers', 'read'],
            readEach: ['_root', 'read-each'],
            remove: ['_root', 'remove'],
            commit: ['_root', '_helpers', 'commit'],
            liveness: ['_root', 'liveness'],
            exec: ['_root', 'exec'],
        }),
        frame: (fields, tail) => fields.map((f) => `${f}\0`).join('') + (tail ?? ''),
        // sha1sum prints lowercase; the scripts compare against these fields.
        hashCase: (h) => h.toLowerCase(),
        parseStat: parseStatRecords,
        // NOT a login shell, and not an rc-reading one: `-lc` sources the
        // profile first on every call (nvm in a profile made that the WHOLE cost
        // of a fork — ~200 ms vs ~3 ms). None of these scripts needs anything a
        // profile provides. (BASH_ENV is still honoured by bash; nothing here
        // sets it.)
        bin: () => 'bash',
        args: (script) => ['--noprofile', '--norc', '-c', script],
    },
    powershell: {
        scripts: loadScripts('powershell', 'ps1', {
            list: ['_root', 'list'],
            stat: ['_root', 'stat'],
            read: ['_root', 'read'],
            remove: ['_root', 'remove'],
            commit: ['_root', 'commit'],
            liveness: ['_root', 'liveness'],
            exec: ['_root', 'exec'],
        }),
        frame: (fields, tail) =>
            fields.map((f) => `${Buffer.from(String(f), 'utf8').toString('base64')}\n`).join('') +
            (tail !== undefined ? `${tail}\n` : ''),
        hashCase: (h) => h.toUpperCase(),
        parseStat: parseStatLines,
        bin: () => (onPath('pwsh') ? 'pwsh' : 'powershell.exe'),
        args: (script) => ['-NoProfile', '-NonInteractive', '-Command', script],
    },
};

function onPath(bin) {
    const exts = process.platform === 'win32' ? ['.exe', '.cmd', ''] : [''];
    for (const dir of (process.env.PATH || '').split(path.delimiter)) {
        for (const ext of exts) {
            try {
                fs.accessSync(path.join(dir, bin + ext), fs.constants.X_OK);
                return true;
            } catch {}
        }
    }
    return false;
}

// POSIX single-quote escaping, for transports that pass the script through a
// remote shell's command line (ssh joins its arguments into one string).
function quote(s) {
    return `'${String(s).replace(/'/g, `'\\''`)}'`;
}

// A local `run` for tests and local use: spawns bash / pwsh (powershell.exe)
// directly, stdin piped, stdout collected; rejects on a non-zero exit.
function localRunner(dialect = 'bash') {
    const d = DIALECTS[dialect];
    if (!d) throw new Error(`unknown shell dialect: ${dialect}`);
    const bin = d.bin();
    return (script, stdin) =>
        new Promise((resolve, reject) => {
            const p = spawn(bin, d.args(script), { stdio: ['pipe', 'pipe', 'pipe'] });
            const out = [];
            const err = [];
            p.stdout.on('data', (c) => out.push(c));
            p.stderr.on('data', (c) => err.push(c));
            p.on('error', reject);
            p.on('close', (code) => {
                if (code === 0) return resolve(Buffer.concat(out).toString('utf8'));
                const e = new Error(Buffer.concat(err).toString('utf8').trim() || `${bin} exited ${code}`);
                e.code = code;
                reject(e);
            });
            p.stdin.on('error', () => {}); // a script that exits early closes its stdin
            p.stdin.end(stdin ?? '');
        });
}

function shell({ root, run, dialect = 'bash' } = {}) {
    const d = DIALECTS[dialect];
    if (!d) throw new Error(`unknown shell dialect: ${dialect}`);
    if (typeof root !== 'string' || !root) throw new Error('shell({ root }): root is required');
    if (typeof run !== 'function') throw new Error('shell({ run }): run(script, stdin) is required');

    const call = (method, fields, tail) => run(d.scripts[method], d.frame([root, ...fields], tail));

    async function list({ skip = DEFAULT_SKIP } = {}) {
        const raw = await call('list', skip.map(String));
        const buf = Buffer.from(String(raw).replace(/\s+/g, ''), 'base64');
        const parts = buf.toString('utf8').split('\0');
        const out = [];
        for (let i = 0; i + 2 < parts.length; i += 3) {
            out.push({ path: parts[i], size: Number(parts[i + 1]), mtime: Number(parts[i + 2]) });
        }
        return out;
    }

    async function stat(paths, { hash = false } = {}) {
        const clean = paths.map(checkPath);
        const out = new Map();
        if (!paths.length) return out;
        const raw = await call('stat', [hash ? '1' : '0', ...clean]);
        const rows = d.parseStat(raw, clean, hash);
        paths.forEach((p, i) => out.set(p, rows.get(i) || { missing: true }));
        return out;
    }

    async function read(paths) {
        const clean = paths.map(checkPath);
        const out = new Map();
        if (!paths.length) return out;
        const raw = String(await call('read', clean));
        const batched = raw.includes('\n@DATA\n') || raw.startsWith('@DATA\n');
        let got = batched ? parseReadBatched(raw, clean) : parseReadEach(raw);
        // A mis-sized batch (a file changed between find and cat) is never
        // split by guesswork: re-read it one file at a time (a second call,
        // only in that race).
        if (!got) got = parseReadEach(await call('readEach', clean));
        paths.forEach((p, i) => {
            if (got.has(i)) out.set(p, got.get(i));
        });
        return out;
    }

    async function remove(paths) {
        const clean = paths.map(checkPath);
        if (!clean.length) return;
        await call('remove', clean);
    }

    // Publish BYTES to PATH atomically, or refuse — see scripts/*/commit.* for
    // the publish mechanics. `response-lost` is NOT a refusal: the transport
    // failed after sending (or answered something unparseable), so the publish
    // may already have happened; the caller reads the target back to find out.
    async function commit(p, bytes, { expectedHash = null, create = false } = {}) {
        const rel = checkPath(p);
        const buf = Buffer.isBuffer(bytes) ? bytes : Buffer.from(bytes);
        const fields = [
            create ? 'create' : 'replace',
            rel,
            expectedHash ? d.hashCase(String(expectedHash)) : '-',
            d.hashCase(sha1(buf)),
        ];
        let raw;
        try {
            raw = await call('commit', fields, buf.toString('base64'));
        } catch (err) {
            return { outcome: 'response-lost', error: String((err && err.message) || err).slice(0, 500) };
        }
        const line =
            lines(raw)
                .map((l) => l.trim())
                .filter(Boolean)
                .pop() || '';
        const [tag, ...rest] = line.split(/\s+/);
        const arg = rest.join(' ');
        switch (tag) {
            case 'OK':
                return { outcome: 'ok', hash: arg.toUpperCase() };
            case 'STALE':
                return { outcome: 'stale', current: arg.toUpperCase() };
            case 'EXISTS':
            case 'MISSING':
            case 'SYMLINK':
                return { outcome: tag.toLowerCase() };
            case 'NOATOMIC':
                return arg ? { outcome: 'noatomic', error: arg } : { outcome: 'noatomic' };
            case 'CORRUPT':
                // The target saw different bytes than were sent and published
                // nothing — reported as lost so the caller re-reads.
                return { outcome: 'response-lost', error: 'payload corrupted in transit; nothing was written' };
            default:
                return { outcome: 'response-lost', error: `unrecognised commit response: ${line.slice(0, 200)}` };
        }
    }

    // Whether PID is alive ON THE TARGET, plus the target's hostname — fetched
    // in the same script. `alive: null` means "could not be determined", never
    // proof of death.
    async function liveness(pid) {
        let raw;
        try {
            raw = await call('liveness', [String(pid)]);
        } catch {
            return { host: null, alive: null, startTime: null };
        }
        let host = null;
        let alive = null;
        let startTime = null;
        for (const line of lines(raw)) {
            const t = line.trim();
            if (t.startsWith('HOST ')) host = t.slice(5).trim();
            else if (t === 'DEAD') alive = false;
            else if (t.startsWith('ALIVE')) {
                alive = true;
                const st = t.split(/\s+/)[1];
                startTime = st && st !== '-' ? st : null;
            }
        }
        return { host, alive, startTime };
    }

    // The lock file records the CALLER's identity (pid, os.hostname(), start
    // time). Liveness is judged on the TARGET, whose hostname comes back from
    // the same script: a stale lock is broken only when the lock's host IS the
    // target (the local runner, or ssh to this same machine) and the pid is
    // provably dead there. A lock written by a caller on another machine can
    // never be proven dead from the target and so is always reported locked —
    // for a remote workspace a crashed holder's lock needs a manual release.
    async function lock(lockPath, id) {
        checkPath(lockPath);
        return acquireLock({ commit, read, remove, liveness, self: selfIdentity() }, lockPath, id);
    }

    // TRUST BOUNDARY: runs a caller-supplied command in the workspace root on
    // the target (the command travels on stdin and is eval'd there). A
    // capability the host grants, not a default. `out` = stdout, or
    // stdout+stderr on failure.
    async function exec(command) {
        let raw;
        try {
            raw = await call('exec', [], Buffer.from(String(command), 'utf8').toString('base64'));
        } catch (err) {
            return { ok: false, out: String((err && err.message) || err) };
        }
        let rc = null;
        let section = null;
        const acc = { OUT: [], ERR: [] };
        for (const line of lines(raw)) {
            const t = line.trim();
            if (t.startsWith('RC ')) rc = Number(t.slice(3));
            else if (t === '@OUT') section = 'OUT';
            else if (t === '@ERR') section = 'ERR';
            else if (section && t) acc[section].push(t);
        }
        const dec = (a) => Buffer.from(a.join(''), 'base64').toString('utf8');
        const stdout = dec(acc.OUT);
        if (rc === 0) return { ok: true, out: stdout };
        return { ok: false, out: stdout + dec(acc.ERR) };
    }

    return { kind: 'shell', dialect, root, list, stat, read, commit, lock, remove, exec };
}

shell.quote = quote;
shell.localRunner = localRunner;
shell.DIALECTS = DIALECTS;

module.exports = shell;
module.exports.shell = shell;
module.exports.quote = quote;
module.exports.localRunner = localRunner;
