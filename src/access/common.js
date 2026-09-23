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
// lexical: `..` segments, absolute paths (POSIX or drive-letter/UNC) and NUL are
// refused outright rather than normalised away. Backslash is treated as a
// separator for the `..` test only, so `a\..\..\x` cannot escape on Windows.
// Returns the cleaned path (empty and `.` segments dropped).
function checkPath(p) {
    if (typeof p !== 'string' || p === '') throw new Error(`invalid workspace path: ${JSON.stringify(p)}`);
    if (p.includes('\0')) throw new Error(`invalid workspace path (NUL): ${JSON.stringify(p)}`);
    if (p.startsWith('/') || p.startsWith('\\') || /^[A-Za-z]:/.test(p)) {
        throw new Error(`workspace path must be relative: ${JSON.stringify(p)}`);
    }
    if (p.split(/[\\/]/).includes('..')) throw new Error(`workspace path escapes the root: ${JSON.stringify(p)}`);
    const clean = p
        .split('/')
        .filter((s) => s !== '' && s !== '.')
        .join('/');
    if (!clean) throw new Error(`invalid workspace path: ${JSON.stringify(p)}`);
    return clean;
}

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

module.exports = { DEFAULT_SKIP, sha1, checkPath, mtimeOf, procStartTime, acquireLock, selfIdentity };
