'use strict';
// Pieces shared by every shipped facade: path checking, hashing, the mtime
// number, and the advisory-lock algorithm (which is written once against the
// facade's own primitives, so a lock and a file publish share one proof of
// exclusivity).

const crypto = require('crypto');
const fs = require('fs');
const os = require('os');

// Directories never worth listing. A dependency tree is read for its SURFACE
// (package.json, types) by a separate pass, never walked to workspace depth.
const DEFAULT_SKIP = ['node_modules', '.git', '.idea', 'dist', 'build', 'coverage', '.next', 'vendor'];

const sha1 = (buf) => crypto.createHash('sha1').update(buf).digest('hex').toUpperCase();

// Workspace-relative, `/`-separated, never escaping the root. The check is
// lexical — `..` segments, absolute paths and NUL are refused outright rather
// than normalised away — and it depends on the DIALECT of the filesystem the
// facade reaches, because what "absolute" and "separator" mean does:
//
//   posix    `/` is the only separator. Backslash and `:` are ordinary
//            filename characters, so `d:\test\x.html` is a (strange but valid)
//            RELATIVE name — one such file on disk must be addressable, never
//            a reason to refuse the workspace. Refused: leading `/`, a `..`
//            segment, NUL.
//   windows  Backslash is a separator too (so `a\..\..\x` cannot escape), and
//            drive-letter (`C:`), rooted (`\x`) and UNC (`\\host`) forms are
//            absolute. Refused: those, a `..` segment on either separator, NUL.
//
// Returns the cleaned path (empty and `.` segments dropped).
const DIALECT_RULES = {
    posix: { absolute: (p) => p.startsWith('/'), segments: (p) => p.split('/') },
    windows: {
        absolute: (p) => p.startsWith('/') || p.startsWith('\\') || /^[A-Za-z]:/.test(p),
        segments: (p) => p.split(/[\\/]/),
    },
};

function checkPath(p, dialect = 'posix') {
    const rules = DIALECT_RULES[dialect];
    if (!rules) throw new Error(`unknown path dialect: ${dialect}`);
    if (typeof p !== 'string' || p === '') throw new Error(`invalid workspace path: ${JSON.stringify(p)}`);
    if (p.includes('\0')) throw new Error(`invalid workspace path (NUL): ${JSON.stringify(p)}`);
    if (rules.absolute(p)) throw new Error(`workspace path must be relative: ${JSON.stringify(p)}`);
    if (rules.segments(p).includes('..')) throw new Error(`workspace path escapes the root: ${JSON.stringify(p)}`);
    const clean = p
        .split('/')
        .filter((s) => s !== '' && s !== '.')
        .join('/');
    if (!clean) throw new Error(`invalid workspace path: ${JSON.stringify(p)}`);
    return clean;
}

// The same check bound to one dialect — what a facade exposes as its own
// `checkPath`, so a caller (the workspace scan) can ask "can this facade
// address that name?" before sending it anywhere.
const pathChecker = (dialect) => (p) => checkPath(p, dialect);

// mtime as a Number: whole seconds plus the 9-digit nanosecond fraction,
// formatted as a decimal string then parsed — the same path the bash dialect's
// `%T@` line takes, so the two round identically. Only ever compared for
// equality against an earlier value from the same facade.
function mtimeOf(st) {
    const ns = st.mtimeNs; // BigInt
    const sec = ns / 1_000_000_000n;
    const frac = (ns % 1_000_000_000n).toString().padStart(9, '0');
    return Number(`${sec}.${frac}`);
}

// Start time of a local pid (Linux: /proc/<pid>/stat field 22, clock ticks since
// boot), so a lock's recorded owner can be told apart from a different process
// that later reused the PID. Parsed after the LAST ')' because the comm field
// may itself contain spaces and parentheses. null where the platform has no
// cheap answer (macOS, Windows) — pid reuse is then indistinguishable, a
// documented limit.
function procStartTime(pid) {
    try {
        const s = fs.readFileSync(`/proc/${pid}/stat`, 'utf8');
        const rest = s.slice(s.lastIndexOf(')') + 2).split(' ');
        return rest[19] || null;
    } catch {
        return null;
    }
}

// An advisory lock, published with the SAME atomic fail-if-present primitive
// `commit` uses for `create`.
//
// A lock is NEVER broken on elapsed time. It is broken only when ownership
// metadata proves the recorded owner is dead: the lock's host is the host the
// liveness check ran on, and the recorded PID is not alive there (or is alive
// but its start time no longer matches — a different process reusing the PID).
// A lock recorded on a different host, or whose liveness cannot be proven
// either way, is reported LOCKED rather than guessed at.
//
// What this cannot do, stated rather than hidden: exclude a NON-COOPERATING
// writer. It excludes every writer that goes through this same lock/commit
// path; a process that writes the target directly is invisible to it.
//
// `liveness(pid)` → { host, alive: true|false|null, startTime }, where `host`
// is the hostname of the machine the check ran on.
async function acquireLock({ commit, read, remove, liveness, self }, lockPath, id) {
    const meta = { pid: self.pid, host: self.host, startTime: self.startTime || null, id };
    const payload = Buffer.from(JSON.stringify(meta), 'utf8');
    const release = async () => {
        try {
            await remove([lockPath]);
        } catch {
            /* best effort */
        }
    };

    const first = await commit(lockPath, payload, { create: true });
    if (first.outcome === 'ok') return { ok: true, release };
    if (first.outcome !== 'exists') return { ok: false, reason: first.outcome };

    let existing;
    try {
        const got = await read([lockPath]);
        existing = JSON.parse(got.get(lockPath).toString('utf8'));
    } catch {
        return { ok: false, reason: 'locked' }; // unreadable: cannot prove it is safe to break
    }
    if (!existing || typeof existing !== 'object') return { ok: false, reason: 'locked' };

    let live;
    try {
        live = await liveness(existing.pid);
    } catch {
        return { ok: false, reason: 'locked' };
    }
    const sameHost =
        typeof existing.host === 'string' &&
        typeof live.host === 'string' &&
        existing.host.toLowerCase() === live.host.toLowerCase();
    if (!sameHost) return { ok: false, reason: 'locked' };
    const sameOwner =
        live.alive && (!existing.startTime || !live.startTime || String(live.startTime) === String(existing.startTime));
    if (live.alive === null || sameOwner) return { ok: false, reason: 'locked' };

    // Proven dead (or a different process now holds that PID) — break the
    // stale lock and retry exactly once.
    await release();
    const second = await commit(lockPath, payload, { create: true });
    if (second.outcome === 'ok') return { ok: true, release };
    return { ok: false, reason: second.outcome === 'exists' ? 'locked' : second.outcome };
}

const selfIdentity = () => ({ pid: process.pid, host: os.hostname(), startTime: procStartTime(process.pid) });

module.exports = { DEFAULT_SKIP, sha1, checkPath, pathChecker, mtimeOf, procStartTime, acquireLock, selfIdentity };
