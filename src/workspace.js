'use strict';
// One workspace's code graph: locate and edit code by NAME, without reading
// whole files to find things, and without ever corrupting or silently
// overwriting a file.
//
// Everything it knows about the files comes through the ACCESS FACADE the host
// supplies (docs/ACCESS.md) — list, stat(+hash), read, atomic commit; optional
// lock and exec. Paths are workspace-relative everywhere: the facade owns the
// root. Identity is the host-given `id`, never a path (two machines can have
// the same path).
//
// STORAGE — optional (`store: null` runs purely in memory). With a store
// (src/store.js), the graph — file rows, symbols, packages — survives
// restarts and carries full-text search; this in-memory Map is then a
// WORKING SET, a cache of the store. It may also hold file TEXT as a cache for
// parsing and slicing, rebuilt from the facade on demand, never persisted:
// okdb indexes file text through a resolved field and never stores it.
//
// FRESHNESS (docs/DESIGN.md §6). The tree is walked only at open and on an
// explicit sync; every verb that quotes file text re-stats exactly the files
// it is about to quote (verify-on-read), and every index-wide verb re-checks
// only its own result set and re-runs once if anything moved.

const crypto = require('crypto');
const pathPosix = require('path').posix;
const { extract, langOf, isTextual, isSecret } = require('./analysis/parse');
const { extensionFor } = require('./analysis/extensions');
const { scanPackages, readPackages } = require('./analysis/packages');
const { validateSourceAsync, compareDiagnostics } = require('./analysis/validate');
const { paletteFrom } = require('./analysis/convention-color');
const textUtil = require('./analysis/text');
const { chunkCode } = require('./analysis/chunk');
const { quoteLines, asQuery } = require('./store');

const DEFAULTS = {
    // A whole-file read is capped by LINES, not bytes: a truncated read must
    // still end on a line boundary, and every range it reports must be one the
    // caller can ask for verbatim. 600 lines covers most source files outright.
    fileReadLines: 600,
    // How much of a file to quote AROUND a hit. A search result is a location
    // plus enough of its surroundings to judge it; one line is a location and
    // nothing else, which is what sent a live task to the shell to see the CSS
    // rule that a hit had already correctly located.
    windowLines: 12,
    windowMaxChars: 1200,
    // The largest piece an outline will offer. A "piece" exists to be judged
    // cheaply and then read in full if it survives, so its size is really the
    // TIER-1 BUDGET of whoever is judging: a 158-line template region put in
    // front of a judge is the whole-file read the outline was built to avoid,
    // only smaller.
    outlineMaxLines: 80,
    // Reads during a scan are batched: one facade call per batch, bounded by
    // count and by bytes so a remote transport is never handed one enormous
    // payload.
    readBatchFiles: 200,
    readBatchBytes: 8 * 1024 * 1024,
    // Persist ingests from verify-on-read and own writes in the background
    // (one transaction per burst). `false` = only on flush()/sync().
    autoFlush: true,
    // Directory names the facade's list() prunes at any depth (null = the
    // facade's default).
    skip: null,
};
// A leftover sliver of a carved region — the `};` between a template and a
// style block — is not a piece anyone wants offered. An outline is a menu, not
// a partition: everything stays reachable by explicit range regardless.
const MIN_PIECE_LINES = 3;

const sha1 = (buf) => crypto.createHash('sha1').update(buf).digest('hex').toUpperCase();

// okcode's own transient files never become part of the graph.
const OWN_TRANSIENT = /(^|\/)\.okcode-tmp-|\.okcode-lock$/;

// A workspace's own .gitignore is the single source of truth for "not project
// source" — reusing it beats maintaining a parallel exclusion list, and it is
// what caught a real 8,763-file blind spot: a session-state directory (one
// JSON file per key) sitting at a workspace root with no leading dot, so the
// dot-directory filter never saw it.
//
// Deliberately a pragmatic SUBSET of gitignore syntax — no negation (!), no
// **, no character classes — covering what workspaces actually use in
// practice: bare basenames, directory-only patterns (trailing /), and simple
// * wildcards. A pattern this can't express is simply not excluded, never
// wrongly excluded; good enough without a parsing dependency.
function compileGitignore(text) {
    if (!text) return () => false;
    const patterns = String(text)
        .split(/\r?\n/)
        .map((l) => l.trim())
        .filter((l) => l && !l.startsWith('#') && !l.startsWith('!'));
    const toRegexPart = (seg) => seg.replace(/[.+^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '[^/]*');
    const matchers = patterns.map((pat) => {
        const body = pat.endsWith('/') ? pat.slice(0, -1) : pat;
        const anchored = body.includes('/'); // any slash (leading or internal) anchors to the root
        const stripped = body.replace(/^\//, '');
        return anchored
            ? new RegExp(`^${toRegexPart(stripped)}(?:/|$)`)
            : new RegExp(`(?:^|/)${toRegexPart(stripped)}(?:/|$)`);
    });
    return (relPath) => matchers.some((re) => re.test(relPath));
}

// REGIONS NEST, AND A NESTED MENU IS A TRAP.
//
// The okjs analyser reports a component as three regions:
// component-definition 13-455, template 231-388, style 390-454 — and the first
// one CONTAINS the other two. Offered as-is, a judge reads the same markup
// twice and, worse, keeping the envelope keeps all 443 lines, which is the
// whole-file read the piecewise outline exists to prevent.
//
// So a container is carved down to what it alone covers. component-definition
// becomes 13-230 (the script and setup, the part nothing else claims); the
// one-line slivers at 389 and 455 fall below MIN_PIECE_LINES and are dropped.
// Sibling regions are untouched. The result is a flat menu of disjoint pieces.
function carveRegions(regions) {
    const sorted = [...regions].sort((a, b) => a.lineStart - b.lineStart || b.lineEnd - a.lineEnd);
    const out = [];
    for (const r of sorted) {
        const span = r.lineEnd - r.lineStart;
        // STRICTLY inside: two regions reported over the identical range must
        // not erase each other, which is what a non-strict test would do.
        const inner = sorted.filter(
            (o) => o !== r && o.lineStart >= r.lineStart && o.lineEnd <= r.lineEnd && o.lineEnd - o.lineStart < span,
        );
        if (!inner.length) {
            out.push(r);
            continue;
        }
        const gaps = [];
        let at = r.lineStart;
        for (const o of inner) {
            if (o.lineStart > at) gaps.push([at, o.lineStart - 1]);
            at = Math.max(at, o.lineEnd + 1);
        }
        if (at <= r.lineEnd) gaps.push([at, r.lineEnd]);
        for (const [a, b] of gaps) {
            if (b - a + 1 >= MIN_PIECE_LINES) out.push({ ...r, lineStart: a, lineEnd: b });
        }
    }
    return out.sort((a, b) => a.lineStart - b.lineStart);
}

// Where a piece MAY be cut, in 1-based lines relative to the slice. The
// chunker already knows the seams — declarations where the language parser
// finds them, paragraphs where it does not — so this borrows them rather than
// inventing a second opinion about structure.
function cutLines(text) {
    try {
        const chunks = chunkCode(text, null) || [];
        return chunks.map((c) => text.slice(0, c.start).split('\n').length);
    } catch {
        return [1];
    }
}

// Split anything over `max` lines, preferring real seams and falling back to a
// flat cut. The fallback is not a nicety: a 158-line template with no seam the
// chunker recognises would otherwise stay 158 lines, and the bound has to hold
// whether or not the structure cooperated.
function splitPiece(a, b, text, max) {
    const total = b - a + 1;
    if (total <= max) return [[a, b]];
    const edges = [...new Set([1, ...cutLines(text).filter((c) => c > 1 && c <= total), total + 1])].sort(
        (x, y) => x - y,
    );
    const runs = [];
    let start = edges[0];
    for (let i = 1; i < edges.length; i++) {
        if (edges[i] - start >= max || i === edges.length - 1) {
            runs.push([start, edges[i] - 1]);
            start = edges[i];
        }
    }
    const out = [];
    for (const [s, e] of runs) {
        for (let n = s; n <= e; n += max) out.push([a + n - 1, a + Math.min(e, n + max - 1) - 1]);
    }
    return out;
}

// The label a piece is judged on, which is the only thing tier 0 ever sees.
//
// Two jobs. Siblings cut from one region are NUMBERED, because "template html"
// three times over describes all of them equally and none of them usefully.
// And every piece gets its FIRST NON-BLANK LINE, which is what a fragment is
// about far more often than not — a selector, a tag, a heading, a declaration.
function labelPieces(pieces, lines) {
    const seen = new Map();
    for (const p of pieces) seen.set(p.name, (seen.get(p.name) || 0) + 1);
    const n = new Map();
    return pieces.map((p) => {
        const head = (lines.slice(p.lineStart - 1, p.lineEnd).find((l) => l.trim()) || '').trim().slice(0, 120);
        const doc = [p.doc, head].filter(Boolean).join(' — ') || null;
        if ((seen.get(p.name) || 0) < 2) return { ...p, doc };
        const i = (n.get(p.name) || 0) + 1;
        n.set(p.name, i);
        return { ...p, name: `${p.name} §${i}`, doc };
    });
}

// What actually happened at and after the commit point, from: the commit
// primitive's own report (which may be entirely ABSENT — response-lost), and
// an INDEPENDENT readback of the target that is always attempted. By the time
// control reaches here the commit call has returned or failed, so a lost
// RESPONSE is never confused with an in-flight commit — which is what makes
// `not_committed` provable rather than assumed.
//
// Two different kinds of "wrong" are deliberately NOT the same outcome:
//   - the facade told us 'ok' (it HAS a report) but the readback shows
//     different bytes than the candidate — a genuine, POSITIVELY CONFIRMED
//     divergence: `diverged_after_commit`.
//   - the response was lost entirely and the readback shows bytes that are
//     neither the candidate nor the original — there is no report to trust at
//     all, so this is not even confirmed to be OUR commit that produced it:
//     `commit_outcome_unknown`, a strictly less certain claim than "diverged".
function classifyCommit({ commit, candidateHash, beforeHash, readbackHash, readAvailable }) {
    const responseLost = commit.outcome !== 'ok';
    if (!readAvailable) return responseLost ? 'commit_outcome_unknown' : 'committed_unverified';
    if (readbackHash === candidateHash) {
        return !responseLost && commit.hash === candidateHash ? 'ok' : 'committed_verified_with_warning';
    }
    if (readbackHash === beforeHash) return 'not_committed';
    return responseLost ? 'commit_outcome_unknown' : 'diverged_after_commit';
}

// ONE parser for the "where" half of every address, used by read(),
// fileForTarget() and resolveTargetInSnapshot() alike.
//
// A range may be written with EITHER separator. `file:from-to` is the
// canonical form and what every reader hands back; `file#from-to` is what a
// model writes when it has just seen `file#symbol` in the same vocabulary. The
// two can never collide, because a symbol name cannot be digits and a dash —
// so accepting both costs nothing and removes a dead end that cost a whole
// task: a correct file with correct lines and the wrong separator resolved as
// a SYMBOL lookup, missed, and read as "this file is not the target".
//
// A bare Windows drive letter is not a separator: the head must be longer than
// one character.
const RANGE_TAIL = /^(\d+)(?:-(\d+))?$/;
function parseRange(query) {
    const raw = String(query || '');
    const m = /^(.*?)[:#](\d+)(?:-(\d+))?$/.exec(raw);
    if (!m || m[1].length <= 1) return null;
    return { file: m[1], from: Number(m[2]), to: m[3] ? Number(m[3]) : Number(m[2]) };
}

async function openWorkspace({ id, access, store = null, options = {} } = {}) {
    if (typeof id !== 'string' || !id) throw new Error('openWorkspace needs an id');
    for (const m of ['list', 'stat', 'read', 'commit']) {
        if (!access || typeof access[m] !== 'function') throw new Error(`openWorkspace: access.${m}() is required`);
    }
    const opts = { ...DEFAULTS, ...(options || {}) };

    // rel -> { path, rel, size, mtime, hash, lang, lines, symbols, imports,
    //          exports, parsed, indexed, reason, lossless, analyzerVersion,
    //          content? (cache only) }
    // `path` and `rel` are the same workspace-relative string; both are kept
    // because callers of the brain's index address records by either.
    let files = new Map();
    // name -> package surface. A different lifetime from source: this changes
    // on an install, not on an edit, so it is never part of the file scan.
    let packages = new Map();
    if (store) {
        const loaded = store.load();
        files = loaded.files;
        packages = loaded.packages;
        // The store's resolvers ask here first: text this process already
        // read (and hashed) is served without another facade round trip.
        store.setTextSource((rel, hash) => {
            const f = files.get(rel);
            return f && f.hash === hash && typeof f.content === 'string' ? f.content : undefined;
        });
    }
    let lastScanAt = 0;
    // Bumped on every mutation of `files` — an ingest OR a removal — from ANY
    // path (a tree walk, a targeted verify, an own write). palette()'s cache
    // key needs a counter that moves on every real change.
    let generation = 0;
    // rel -> 'save' | 'remove' (last op wins); flushed as one transaction.
    const pending = new Map();
    const pendingPackages = [];

    // ── ingest ──────────────────────────────────────────────────────────

    // One row, with its bytes already in hand (or null for an opaque file).
    // The hash is taken from the bytes that were PARSED whenever there are
    // any, so a record's text, symbols and hash always describe the same
    // version of the file — never a hash from one stat and text from a later
    // read.
    function ingestOne(row, buf) {
        // `lossless` is what a strict write refuses on: a decode that does not
        // round-trip byte-for-byte is not text this index can safely compose
        // an edit onto.
        let text = null;
        let lossless = true;
        if (buf) {
            const d = textUtil.decode(buf);
            text = d.text;
            lossless = d.lossless;
        }
        const parsed = extract(row.path, text);
        const record = {
            ...row,
            ...parsed,
            path: row.path,
            rel: row.path,
            hash: buf ? sha1(buf) : row.hash,
            lossless,
        };
        // Symbols and diagnostics describe the text; the text itself is kept
        // as a cache so slicing, quoting and the next verify need no read.
        if (text !== null && parsed.indexed !== false) record.content = text;
        files.set(row.path, record);
        generation++;
        if (store) pending.set(row.path, 'save');
        return record;
    }

    function drop(rel) {
        if (!files.has(rel)) return false;
        files.delete(rel);
        generation++;
        if (store) pending.set(rel, 'remove');
        return true;
    }

    // Read many files through the facade in bounded batches.
    async function readMany(rels, sizeOf = () => 0) {
        const out = new Map();
        let batch = [];
        let bytes = 0;
        const go = async () => {
            if (!batch.length) return;
            const got = await access.read(batch);
            for (const [p, buf] of got) out.set(p, buf);
            batch = [];
            bytes = 0;
        };
        for (const rel of rels) {
            batch.push(rel);
            bytes += sizeOf(rel) || 0;
            if (batch.length >= opts.readBatchFiles || bytes >= opts.readBatchBytes) await go();
        }
        await go();
        return out;
    }

    // Make sure these records carry their text. A text read that finds the
    // file's hash has MOVED is not attached as a cache — it is new
    // information, and it is ingested (re-parsed) instead, so a record's text
    // can never disagree with its symbols. A file that has vanished is
    // dropped.
    async function ensureTexts(list) {
        const need = list.filter((f) => f && typeof f.content !== 'string' && f.indexed !== false && !isSecret(f.path));
        if (!need.length) return;
        const got = await readMany(
            need.map((f) => f.path),
            (p) => (files.get(p) || {}).size,
        );
        for (const f of need) {
            const buf = got.get(f.path);
            if (!buf) {
                drop(f.path);
                continue;
            }
            const h = sha1(buf);
            const cur = files.get(f.path);
            if (cur && cur.hash === h) {
                const d = textUtil.decode(buf);
                cur.content = d.text;
                cur.lossless = d.lossless;
            } else {
                // mtime unknown for these bytes: null makes the next verify
                // re-check rather than trust a pairing it never observed.
                ingestOne({ path: f.path, size: buf.length, mtime: null }, buf);
                scheduleFlush();
            }
        }
    }

    // Freshness for the files about to be SERVED, checked against the facade
    // at the moment of the read rather than trusted from the last tree scan.
    //
    // STAT-FIRST, read only what moved. A file whose size AND mtime still match
    // the row this index holds (and whose text is already cached) is served
    // from that row with no hash at all. A textual file that moved is simply
    // READ — its hash is taken from the bytes, one round trip instead of a
    // remote hash and then a read; an opaque one is re-hashed where it lives.
    // Only a hash that actually moved re-parses and re-records the file. This
    // is the VERIFY-ON-READ primitive: every verb that quotes file text calls
    // it for exactly the files about to be quoted.
    //
    // Re-recording rather than merely re-reading is the point: letting the read
    // land in the index means the next asker, the vector pipeline and the next
    // tree walk all get it too, instead of it evaporating into one observation.
    //
    // `strict` is the write-path variant of the same check. A best-effort read
    // that cannot reach the files must not be fatal — the index still holds a
    // coherent answer. A WRITE is the opposite: a transport hiccup during
    // verification must never let an edit proceed against a cached offset it
    // could not actually confirm, so `strict` THROWS instead of falling back —
    // the caller (editBatch) turns that into a refusal that has touched nothing.
    //
    // NOTE on the safety property this shortcut trades on: trusting an
    // unchanged (size, mtime) pair means a forged edit that lands within the
    // same mtime tick and preserves the exact byte count would go undetected
    // HERE. It cannot produce an unsafe COMMIT, though — the facade's commit
    // independently re-hashes the destination immediately before publishing and
    // refuses (`stale`) on any mismatch against the caller's `expectedHash`.
    async function verify(paths, { strict = false } = {}) {
        const want = [...new Set(paths)].filter(Boolean);
        if (!want.length) return { checked: 0, changed: [], gone: [] };
        const fail = (err, partial) => {
            if (strict) throw new Error(`could not verify against the files: ${err.message}`);
            return { ...partial, unverified: true };
        };
        let lite;
        try {
            lite = await access.stat(want);
        } catch (err) {
            return fail(err, { checked: 0, changed: [], gone: [] });
        }
        const gone = [];
        const changed = [];
        const needHash = [];
        const needRead = [];
        for (const p of want) {
            const now = lite.get(p);
            const prior = files.get(p);
            if (!now || now.missing) {
                if (drop(p)) gone.push(p);
                continue;
            }
            const textual = isTextual(p, now.size);
            if (
                prior &&
                prior.size === now.size &&
                prior.mtime === now.mtime &&
                (textual ? typeof prior.content === 'string' : prior.indexed === false)
            ) {
                continue;
            }
            (textual ? needRead : needHash).push({ p, now });
        }
        if (needHash.length) {
            let hashed;
            try {
                hashed = await access.stat(
                    needHash.map((x) => x.p),
                    { hash: true },
                );
            } catch (err) {
                return fail(err, { checked: want.length, changed, gone });
            }
            for (const { p } of needHash) {
                const now = hashed.get(p);
                const prior = files.get(p);
                if (!now || now.missing) {
                    if (drop(p)) gone.push(p);
                    continue;
                }
                if (prior && prior.hash === now.hash && prior.indexed === false) {
                    prior.size = now.size;
                    prior.mtime = now.mtime;
                    continue;
                }
                ingestOne({ path: p, size: now.size, mtime: now.mtime, hash: now.hash }, null);
                changed.push(p);
            }
        }
        if (needRead.length) {
            let bufs;
            try {
                bufs = await readMany(
                    needRead.map((x) => x.p),
                    (p) => (lite.get(p) || {}).size,
                );
            } catch (err) {
                return fail(err, { checked: want.length, changed, gone });
            }
            for (const { p, now } of needRead) {
                const buf = bufs.get(p);
                const prior = files.get(p);
                if (!buf) {
                    // Vanished between the stat and the read, or unreadable —
                    // either way there is nothing this index may quote.
                    if (drop(p)) gone.push(p);
                    continue;
                }
                const h = sha1(buf);
                if (prior && prior.hash === h && prior.indexed !== false) {
                    const d = textUtil.decode(buf);
                    prior.content = d.text;
                    prior.lossless = d.lossless;
                    prior.size = now.size;
                    prior.mtime = now.mtime;
                    continue;
                }
                ingestOne({ path: p, size: now.size, mtime: now.mtime }, buf);
                changed.push(p);
            }
        }
        if (changed.length || gone.length) scheduleFlush();
        return { checked: want.length, changed, gone };
    }

    // The RESULT-SET CHECK for index-wide verbs (find/refs/mentions): stat-check
    // only the files a query is about to return — bounded by the hit count,
    // never the tree — and report whether any of them moved. A caller that gets
    // `changed: true` back re-runs its own query once against the now-corrected
    // map; it never loops further than that, and it never triggers a tree walk.
    //
    // STAT-FIRST, and nothing more for a file that did not move: these verbs
    // quote rows (names, line ranges, hashes), not text, so a file whose
    // (size, mtime) still matches its row needs no read — the same trust
    // verify() extends to a cached file, without demanding the text be
    // cached first (on a warm store right after an open it never is). Only
    // what moved or vanished goes through verify(), which reads and
    // re-ingests it.
    async function resultSetCheck(paths) {
        const want = [...new Set(paths)].filter(Boolean);
        if (!want.length) return { changed: false };
        const moved = await movedOf(want);
        if (!moved) return { changed: false, unverified: true };
        if (!moved.length) return { changed: false };
        const r = await verify(moved);
        return { changed: r.changed.length > 0 || r.gone.length > 0 };
    }

    // The paths among `want` whose (size, mtime) no longer match their row
    // (or that vanished) — one metadata stat, no hashing, no read. null when
    // the facade could not be reached.
    async function movedOf(want) {
        let lite;
        try {
            lite = await access.stat(want);
        } catch {
            return null;
        }
        return want.filter((p) => {
            const now = lite.get(p);
            const prior = files.get(p);
            return !now || now.missing || !prior || prior.size !== now.size || prior.mtime !== now.mtime;
        });
    }

    // Mark listing rows ({ file }) that moved or vanished since their row was
    // written — one metadata stat, nothing read or re-ingested.
    async function flagMoved(rows) {
        if (!rows.length) return rows;
        let lite;
        try {
            lite = await access.stat(rows.map((r) => r.file));
        } catch {
            for (const r of rows) r.unverified = true;
            return rows;
        }
        for (const r of rows) {
            const now = lite.get(r.file);
            const prior = files.get(r.file);
            if (!now || now.missing) r.gone = true;
            else if (!prior || prior.size !== now.size || prior.mtime !== now.mtime) r.moved = true;
        }
        return rows;
    }

    // The OPEN/sync primitive: ONE tree-wide list (path+size+mtime, no
    // hashing), then hash ONLY the rows whose (size, mtime) differ from the row
    // this index already holds. A repeat walk of an untouched tree costs one
    // list call and zero hashes. A file this index has never seen is READ
    // straight away (it has to be parsed anyway, and its hash comes from the
    // bytes), so a cold open does not pay a remote hash AND a read per file.
    //
    // This is the only path that discovers files ADDED or REMOVED outside this
    // process — a lookup verb never runs it. It runs at OPEN (this workspace's
    // first use) and on an explicit sync.
    //
    // `rehash` ignores the (size, mtime) shortcut and re-hashes everything
    // (re-parsing only what moved); `rewrite` re-reads and re-records every
    // file even when nothing moved — the rebuild primitive, deliberately
    // explicit, the one operation here that costs real time.
    async function fullScan({ rewrite = false, rehash = false } = {}) {
        const [listed, gi] = await Promise.all([
            access.list(opts.skip ? { skip: opts.skip } : {}),
            access.read(['.gitignore']).catch(() => new Map()),
        ]);
        const gitignored = compileGitignore(gi.get('.gitignore') ? gi.get('.gitignore').toString('utf8') : '');
        // Dot-directories BELOW the root are working state, not project (a
        // workspace carried 13.4 MB of leftover LMDB in .smoke-data and
        // friends). The shipped facades already prune them; a custom facade
        // may not. .gitignore is the second filter, for a working-state
        // directory with no leading dot.
        const rows = listed.filter(
            (r) => !/(^|\/)\.[^/]+\//.test(r.path) && !OWN_TRANSIENT.test(r.path) && !gitignored(r.path),
        );
        const seen = new Set();
        const needHash = [];
        const toRead = [];
        const untouched = [];
        for (const row of rows) {
            seen.add(row.path);
            const prior = files.get(row.path);
            if (!rewrite && !rehash && prior && prior.size === row.size && prior.mtime === row.mtime) {
                untouched.push({ row, prior });
                continue;
            }
            if (!prior || rewrite) {
                if (isTextual(row.path, row.size)) toRead.push(row);
                else needHash.push(row);
                continue;
            }
            needHash.push(row);
        }
        let hashed = new Map();
        if (needHash.length) {
            hashed = await access.stat(
                needHash.map((r) => r.path),
                { hash: true },
            );
        }
        const opaque = [];
        for (const row of needHash) {
            const h = hashed.get(row.path);
            if (!h || h.missing) {
                seen.delete(row.path); // vanished between the walk and the hash
                continue;
            }
            const prior = files.get(row.path);
            const wantsText = isTextual(row.path, h.size);
            const unchanged = !rewrite && prior && prior.hash === h.hash;
            // A THIRD way the rules can move under a file that never did: an
            // extension's analyser changes what it extracts (an okjs upgrade
            // that starts producing symbols it used to miss), and a content
            // hash that has not moved is no evidence the SYMBOLS are still
            // right. `analyzerVersion` is stamped by extract() on every
            // extension-analysed file; a mismatch against the extension's
            // current version forces the same re-ingest a hash change would.
            const ext = extensionFor(row.path);
            const restale =
                unchanged &&
                (wantsText !== (prior.indexed !== false) || (ext && prior.analyzerVersion !== ext.version));
            if (unchanged && !restale) {
                prior.size = h.size;
                prior.mtime = h.mtime;
                continue;
            }
            if (wantsText) toRead.push({ ...row, size: h.size, mtime: h.mtime });
            else opaque.push({ path: row.path, size: h.size, mtime: h.mtime, hash: h.hash });
        }
        // Rows whose (size, mtime) did not move still need the SAME
        // analyzerVersion re-stale check — a file that never changed can still
        // go stale when the RULES change under it. The stored hash is trusted
        // without recomputing it, which is exactly the shortcut this whole
        // primitive exists to take.
        for (const { row, prior } of untouched) {
            const wantsText = isTextual(row.path, row.size);
            const ext = extensionFor(row.path);
            const restale = wantsText !== (prior.indexed !== false) || (ext && prior.analyzerVersion !== ext.version);
            if (!restale) continue;
            if (wantsText) toRead.push(row);
            else opaque.push({ ...row, hash: prior.hash });
        }
        // Only textual files are READ; the rest are recorded from the scan row
        // alone. That keeps a workspace's images, archives and bundles present
        // in the graph (they have a path, a size and a hash) without dragging
        // their bytes through the facade.
        for (const row of opaque) ingestOne(row, null);
        const sizes = new Map(toRead.map((r) => [r.path, r.size]));
        const bodies = await readMany(
            toRead.map((r) => r.path),
            (p) => sizes.get(p),
        );
        for (const row of toRead) {
            const buf = bodies.get(row.path);
            if (!buf) {
                seen.delete(row.path); // vanished or unreadable since the walk
                continue;
            }
            ingestOne({ path: row.path, size: row.size, mtime: row.mtime }, buf);
        }
        let removed = 0;
        for (const key of [...files.keys()]) {
            if (seen.has(key)) continue;
            if (drop(key)) removed++;
        }
        lastScanAt = Date.now();
        return { scanned: rows.length, reparsed: opaque.length + toRead.length, removed, pendingWrites: pending.size };
    }

    // Whether this workspace has run its open-time tree walk. Plain refresh()
    // (what every lookup verb calls) does that walk exactly once — on first
    // use — and is a no-op every call after. New/deleted files show up only
    // through an explicit sync or through an OWN write.
    let opened = false;
    let scanning = null;
    async function refresh({ force = false, rewrite = false, sync = false, rehash = false } = {}) {
        const explicit = force || sync || rewrite || rehash;
        if (scanning) {
            await scanning.catch(() => {});
            if (!explicit) return { scanned: files.size, reparsed: 0, cached: true };
        }
        if (!explicit && opened) return { scanned: files.size, reparsed: 0, cached: true };
        opened = true;
        scanning = fullScan({ rewrite, rehash });
        let result;
        try {
            result = await scanning;
        } catch (err) {
            opened = false; // the next use tries again
            throw err;
        } finally {
            scanning = null;
        }
        // An open or a sync is an explicit sync point, so its results are
        // persisted before it returns — full-text search reads its own scan.
        if (store) await flush();
        return result;
    }
    const ensureOpen = () => refresh();

    // ── persistence ─────────────────────────────────────────────────────
    // Persistence is OUT of the read path. A scan that finds nothing changed
    // must cost no writes at all, and a read that happened to re-record a
    // file must not wait on a durable commit before it can answer: those
    // writes are flushed in the background, one transaction per burst.
    let flushChain = Promise.resolve();
    let flushTimer = null;
    async function doFlush() {
        if (!store) return { written: 0 };
        if (pendingPackages.length) {
            const batch = pendingPackages.splice(0, pendingPackages.length);
            try {
                await store.savePackages(batch);
            } catch {
                /* a cache, not a crash */
            }
        }
        if (!pending.size) return { written: 0 };
        const saves = [];
        const removals = [];
        for (const [rel, op] of pending) {
            if (op === 'remove') removals.push(rel);
            else if (files.has(rel)) saves.push(files.get(rel));
        }
        pending.clear();
        try {
            await store.saveFiles(saves, removals);
        } catch {
            /* the store is a cache; a failed write is a cold start, not a crash */
        }
        return { written: saves.length + removals.length };
    }
    function flush() {
        if (flushTimer) {
            clearImmediate(flushTimer);
            flushTimer = null;
        }
        const next = flushChain.then(doFlush, doFlush);
        flushChain = next.catch(() => {});
        return next;
    }
    function scheduleFlush() {
        if (!store || !opts.autoFlush || flushTimer) return;
        flushTimer = setImmediate(() => {
            flushTimer = null;
            flush();
        });
    }

    // ── lookups (sync helpers over the current working set) ─────────────
    const list = () => [...files.values()];
    const byRel = (rel) => files.get(rel) || null;

    // The INNERMOST symbol whose range contains a line. Every line inside a
    // factory is inside the factory too, and "in createIndex" for 400 lines is
    // an answer that is already useless.
    function ownerOf(file, line) {
        let best = null;
        for (const s of file.symbols || []) {
            if (line < s.lineStart || line > s.lineEnd) continue;
            if (!best || s.lineEnd - s.lineStart < best.lineEnd - best.lineStart) best = s;
        }
        return best;
    }

    // The text AROUND a located line, which is what a hit is for. Centred on
    // the line, clipped to the enclosing symbol when there is one — a hit
    // inside a function should not bleed into the one above it — and to a
    // character budget, because a preview that runs to a thousand lines is a
    // file read wearing a disguise. The caller gets `file`, a line range and
    // `at` alongside it, so widening it into a real read is one call.
    function windowAt(file, line, owner = null, want = opts.windowLines) {
        const src = typeof file.content === 'string' ? file.content : '';
        if (!src) return null;
        const lines = src.split('\n');
        const half = Math.floor(want / 2);
        let lo = Math.max(1, line - half);
        let hi = Math.min(lines.length, line + half);
        if (owner) {
            lo = Math.max(lo, owner.lineStart);
            hi = Math.min(hi, owner.lineEnd);
        }
        let text = lines.slice(lo - 1, hi).join('\n');
        // Trim from the END, never the middle: the located line is the one
        // thing the caller is guaranteed to want, so it is the last to go.
        while (text.length > opts.windowMaxChars && hi > line) {
            hi -= 1;
            text = lines.slice(lo - 1, hi).join('\n');
        }
        while (text.length > opts.windowMaxChars && lo < line) {
            lo += 1;
            text = lines.slice(lo - 1, hi).join('\n');
        }
        return { lineStart: lo, lineEnd: hi, text: text.slice(0, opts.windowMaxChars) };
    }

    // Resolve a path-ish string to one file. A caller quoting a search result
    // may have a bare basename, and one quoting a host observation may have an
    // absolute path under the facade's root — all name the same file.
    function fileOf(hint) {
        if (!hint) return null;
        let want = String(hint).replace(/\\/g, '/').replace(/^\.\//, '');
        const root = typeof access.root === 'string' ? access.root.replace(/\\/g, '/').replace(/\/$/, '') : null;
        if (root && want.toLowerCase().startsWith(`${root.toLowerCase()}/`)) want = want.slice(root.length + 1);
        const all = list();
        const exact = all.find((f) => f.rel === want) || all.find((f) => f.rel.toLowerCase() === want.toLowerCase());
        if (exact) return exact;
        // Suffix, not substring: `store.js` must find `src/store.js` without
        // `index.js` also matching `src/index.js` AND `test/index.js`. Ties are
        // refused rather than guessed at.
        const tail = all.filter((f) => f.rel.endsWith(`/${want}`));
        return tail.length === 1 ? tail[0] : null;
    }

    // Where does this TEXT appear, for a caller who asked for it as a symbol
    // and was wrong? Deliberately cheap and literal — this exists to turn a
    // dead end into a next step, not to compete with find().
    async function mentionsOf(text, limit = 3) {
        const needle = String(text).toLowerCase();
        if (needle.length < 2) return [];
        // Withheld files stay withheld: an address is a lead even without
        // the bytes, so `.env` is never reported as containing "PASSWORD".
        const candidates = list().filter((f) => f.indexed !== false && !isSecret(f.path));
        await ensureTexts(candidates);
        const out = [];
        for (const f of list()) {
            if (typeof f.content !== 'string' || f.indexed === false || isSecret(f.path)) continue;
            const at = f.content.toLowerCase().indexOf(needle);
            if (at < 0) continue;
            out.push({ rel: f.rel, line: f.content.slice(0, at).split('\n').length });
            if (out.length >= limit) break;
        }
        return out;
    }

    // Resolve a name to exactly one symbol, or report the ambiguity honestly.
    // Accepts "name", "parent.name" and "file#name" / "file#parent.name".
    //
    // A nested symbol answers to both its bare name and its path, so a caller
    // that knows only `saveFiles` still lands on it, and one that knows
    // `openStore.saveFiles` can say so when the bare name is ambiguous. The
    // path is tried FIRST: an exact path match is a deliberate address and
    // must not be drowned by same-named siblings elsewhere.
    async function resolve(query) {
        const [fileHint, nameHint] = query.includes('#') ? query.split('#') : [null, query];
        const inScope = [];
        for (const f of list()) {
            if (fileHint && !f.rel.includes(fileHint)) continue;
            for (const s of f.symbols || []) inScope.push({ file: f, symbol: s });
        }
        const byPath = inScope.filter((h) => (h.symbol.path || h.symbol.name) === nameHint);
        const hits = byPath.length ? byPath : inScope.filter((h) => h.symbol.name === nameHint);
        if (!hits.length) {
            // A miss is where the caller gets stranded, so it has to say what
            // to do next: a FILE addressed as a symbol, and text that exists
            // but is not a symbol (a CSS class, a template property), each
            // used to answer "no symbol named X" and send the caller to the
            // shell one action later.
            const asFile = !fileHint && fileOf(nameHint);
            if (asFile)
                return { ok: false, isFile: true, file: asFile, reason: `"${nameHint}" is a file, not a symbol` };
            // A PATH THAT DOES NOT EXIST MUST NOT BE CALLED A MISSING SYMBOL.
            // A caller that cannot tell "this file is not here" from "this
            // name is not a symbol" keeps rephrasing — 32 searches in one live
            // task, several for files that were never going to be found.
            // Naming the miss as a FILE miss, and offering the real files that
            // share its basename, ends that loop.
            if (!fileHint && /[\\/]/.test(nameHint) && /\.[a-z0-9]{1,6}$/i.test(nameHint)) {
                const base = nameHint.replace(/\\/g, '/').split('/').pop();
                const near = list()
                    .filter((f) => f.rel.split('/').pop() === base)
                    .map((f) => f.rel)
                    .slice(0, 4);
                // Nearest by SHARED NAME TOKEN, not by substring (every
                // dotfile's stem is the empty string, and everything
                // contains that).
                const tokens = (s) =>
                    s
                        .split('.')[0]
                        .split(/[^a-z0-9]+/i)
                        .filter((t) => t.length >= 3)
                        .map((t) => t.toLowerCase());
                const wantTokens = new Set(tokens(base));
                const kin =
                    near.length || !wantTokens.size
                        ? []
                        : list()
                              .filter((f) => tokens(f.rel.split('/').pop()).some((t) => wantTokens.has(t)))
                              .map((f) => f.rel)
                              .slice(0, 4);
                return {
                    ok: false,
                    noSuchFile: true,
                    reason:
                        `there is no file "${nameHint}" in this workspace` +
                        (near.length
                            ? ` — but ${near.join(', ')} ${near.length === 1 ? 'has' : 'have'} that name`
                            : '') +
                        (kin.length ? ` — the nearest existing files are ${kin.join(', ')}` : '') +
                        (!near.length && !kin.length
                            ? ' — nothing with a similar name exists either; use structure() to see what is actually here'
                            : ''),
                    candidates: near.length ? near : kin,
                };
            }
            const mentions = await mentionsOf(nameHint, 3);
            return {
                ok: false,
                reason:
                    `no symbol named "${nameHint}"${fileHint ? ` in ${fileHint}` : ''}` +
                    (mentions.length
                        ? ` — but the text appears in ${mentions.map((m) => `${m.rel}:${m.line}`).join(', ')}; read the file, or use find() for what contains it`
                        : ' — nothing in the workspace mentions it either; check the spelling or use ask()'),
                mentions,
            };
        }
        if (hits.length > 1) {
            const where = new Set(hits.map((h) => h.file.rel)).size > 1 ? 'files' : 'places';
            return {
                ok: false,
                ambiguous: true,
                reason: `"${nameHint}" is defined in ${hits.length} ${where} — qualify it as <file>#<path>`,
                candidates: hits.map((h) => `${h.file.rel}#${h.symbol.path || h.symbol.name}`),
            };
        }
        return { ok: true, ...hits[0] };
    }

    // The text of a verified record: the cache when it is held, the facade
    // when it is not (a record whose text somehow is not cached still has to
    // answer — returning nothing is not an option a caller asking for a body
    // can use).
    async function textOfFresh(rel) {
        const f = files.get(rel);
        if (!f) return null;
        if (typeof f.content !== 'string') await ensureTexts([f]);
        const cur = files.get(rel);
        return cur && typeof cur.content === 'string' ? cur.content : '';
    }

    // The exact bytes a symbol addresses, by the offsets the parser recorded —
    // not by its line range. Falls back to the line range for a symbol from an
    // extractor that has none.
    function symbolText(src, symbol) {
        if (Number.isInteger(symbol.start) && Number.isInteger(symbol.end)) return src.slice(symbol.start, symbol.end);
        return src
            .split('\n')
            .slice(symbol.lineStart - 1, symbol.lineEnd)
            .join('\n');
    }

    // Verified against the files first, then served from the index. The order
    // is the whole rule: the cache is authoritative only once it has been
    // shown to agree with reality.
    async function sliceSymbol(file, symbol) {
        await verify([file.path]);
        const fresh = files.get(file.path) || file;
        const src = (await textOfFresh(fresh.path)) ?? '';
        // verify() may just have re-ingested this file with NEW symbol
        // offsets. If this exact symbol still exists in the fresh record,
        // slice what it says NOW rather than the symbol resolved a moment ago.
        const current =
            (fresh.symbols || []).find((s) => (s.path || s.name) === (symbol.path || symbol.name)) || symbol;
        return { text: symbolText(src, current), src, file: files.get(file.path) || fresh, symbol: current };
    }

    // The dialect-specific envelope (regions/diagnostics/coverage/the raw
    // analysis) for an extension-owned file. Computed once per process and
    // cached on the in-memory record — NEVER persisted, unlike symbols. A
    // record rehydrated from the store never ran extract() in THIS process, so
    // this is the backfill: compute once, on first ask.
    async function analysisOf(file) {
        if (file.okAnalysis) return file.okAnalysis;
        const ext = extensionFor(file.path);
        if (!ext) return null;
        await verify([file.path]);
        const fresh = files.get(file.path) || file;
        if (fresh.okAnalysis) return fresh.okAnalysis;
        const src = await textOfFresh(fresh.path);
        // A checker that throws here must lose only the envelope, never the
        // caller — outline() still has symbols/chunks to fall back to.
        let result;
        try {
            result = ext.analyze({ path: fresh.rel, source: src });
        } catch {
            return null;
        }
        if (result.okAnalysis) {
            fresh.okAnalysis = result.okAnalysis;
            fresh.diagnostics = result.diagnostics;
            fresh.coverage = result.coverage;
            fresh.regions = result.regions;
        }
        return fresh.okAnalysis || null;
    }

    // ── editBatch internals ─────────────────────────────────────────────
    // Everything below composes an edit against ONE immutable snapshot of ONE
    // file, taken after strict verification — never against the live,
    // possibly-stale map. A target is resolved against the snapshot's own
    // symbols/content, never against whatever a pre-verification lookup
    // returned.

    // A bare or path-qualified symbol name, resolved WITHIN one file's
    // snapshot only. Ambiguity is refused, never guessed.
    function resolveSymbolInSnapshot(snapshot, nameHint) {
        const syms = snapshot.symbols || [];
        const byPath = syms.filter((s) => (s.path || s.name) === nameHint);
        const hits = byPath.length ? byPath : syms.filter((s) => s.name === nameHint);
        if (!hits.length) return { ok: false, reason: `no symbol named "${nameHint}" in ${snapshot.rel}` };
        if (hits.length > 1) {
            return {
                ok: false,
                ambiguous: true,
                reason: `"${nameHint}" is defined in ${hits.length} places in ${snapshot.rel} — qualify it as <parent>.<name>`,
                candidates: hits.map((s) => s.path || s.name),
            };
        }
        return { ok: true, symbol: hits[0] };
    }

    // A target ("name", "parent.name", "file#name", or "path:from-to"),
    // resolved to OFFSETS in the snapshot's own content.
    function resolveTargetInSnapshot(snapshot, target) {
        const raw = String(target || '');
        const range = parseRange(raw);
        if (range) {
            const { from, to } = range;
            const total = snapshot.content.split('\n').length;
            // The START must be inside the file — refused, not clamped: a
            // range past the end is a caller mistake worth surfacing, not
            // silently correcting into a different edit. The END clamps down.
            if (from < 1 || from > total) {
                return { ok: false, reason: `${snapshot.rel} has ${total} lines — ${from}-${to} is outside it` };
            }
            const end = Math.min(to, total);
            const off = textUtil.lineRangeToOffsets(snapshot.content, from, end);
            return {
                ok: true,
                start: off.start,
                end: off.end,
                lineStart: from,
                lineEnd: end,
                name: null,
                kind: 'range',
            };
        }
        const nameHint = raw.includes('#') ? raw.split('#')[1] : raw;
        const r = resolveSymbolInSnapshot(snapshot, nameHint);
        if (!r.ok) return r;
        const s = r.symbol;
        if (!Number.isInteger(s.start) || !Number.isInteger(s.end)) {
            return {
                ok: false,
                reason: `"${nameHint}" has no offset-addressable region — read it and edit it as a range`,
            };
        }
        return {
            ok: true,
            start: s.start,
            end: s.end,
            lineStart: s.lineStart,
            lineEnd: s.lineEnd,
            name: s.path || s.name,
            kind: s.kind,
        };
    }

    // Which FILE a target names — ONLY to learn which file. No offset this
    // returns is trusted; every offset used to compose an edit comes from
    // resolveTargetInSnapshot() against a strictly-verified snapshot.
    async function fileForTarget(target) {
        const raw = String(target || '');
        const range = parseRange(raw);
        if (range) return fileOf(range.file);
        const r = await resolve(raw);
        if (r.ok || r.isFile) return r.file;
        return null;
    }

    // WHY a target did not resolve, in the caller's own terms — never blaming
    // the file when the file resolves perfectly and only the suffix is wrong,
    // which is the one case where blaming the file sends the caller somewhere
    // destructive.
    async function explainTarget(target) {
        const raw = String(target || '');
        const cut = Math.max(raw.lastIndexOf(':'), raw.lastIndexOf('#'));
        if (cut > 1) {
            const head = raw.slice(0, cut);
            const tail = raw.slice(cut + 1);
            const f = fileOf(head);
            if (f) {
                return RANGE_TAIL.test(tail)
                    ? `${f.rel} IS indexed — "${raw}" failed on the separator, not the file; address a line range as "${f.rel}:${tail}"`
                    : `${f.rel} IS indexed, but has no symbol "${tail}" — address a line range as "${f.rel}:<from>-<to>", or read ${f.rel} to find the symbol's real name`;
            }
        }
        const r = await resolve(raw);
        // A noSuchFile reason is already a complete sentence about the file.
        if (r && r.noSuchFile && r.reason) return r.reason;
        if (r && r.reason) return `no indexed file matching "${raw}" — ${r.reason}`;
        return `no indexed file matching "${raw}"`;
    }

    // THE PROJECT'S OWN COLOURS, read from the project. "Style colour through
    // variables, not literals" is a fact ABOUT this repository, so the palette
    // is derived on demand from the files that declare custom properties,
    // cached against the scan generation. Scoped to STYLE-BEARING files: a
    // variable declared in a fixture or a vendored dependency is not this
    // project's convention.
    let paletteCache = null;
    async function palette() {
        const stamp = `${files.size}:${generation}`;
        if (paletteCache && paletteCache.stamp === stamp) return paletteCache.list;
        const styled = list().filter(
            (f) =>
                f.indexed !== false &&
                !isSecret(f.path) &&
                /\.(html|css|ok\.js|ok\.mjs)$/i.test(f.rel) &&
                !/(^|\/)(node_modules|vendor|dist|build)\//.test(f.rel),
        );
        // Text is only cached for files something has touched. An empty
        // palette makes the rule inert by design, so read what is missing —
        // found the honest way, when the gate accepted the exact edit it
        // exists to refuse.
        await ensureTexts(styled);
        const sources = [];
        for (const f of styled) {
            const cur = files.get(f.path);
            const src = cur && typeof cur.content === 'string' ? cur.content : null;
            if (src && src.includes('--')) sources.push(src);
        }
        const out = paletteFrom(sources);
        paletteCache = { stamp: `${files.size}:${generation}`, list: out };
        return out;
    }

    // .mjs/.cjs decide it outright. A bare .js is decided by the NEAREST
    // ancestor package.json's "type" — strictly re-verified, never served from
    // whatever the index last cached, because the manifest controls what
    // counts as valid source for the file being edited. When the nearest
    // manifest declares no "type" (or none exists), Node's own `--check` runs
    // syntax detection rather than defaulting to CommonJS — which is what
    // 'ambiguous' means here, resolved by validate trying both goals.
    //
    // Only manifests INSIDE the workspace are consulted: the facade cannot
    // (and must not) reach above its root.
    async function resolveModuleKind(rel) {
        const ext = pathPosix.extname(rel).toLowerCase();
        if (ext === '.mjs') return 'module';
        if (ext === '.cjs') return 'commonjs';
        const candidates = [];
        let dir = pathPosix.dirname(rel);
        for (let i = 0; i < 64; i++) {
            candidates.push(dir === '.' || dir === '' ? 'package.json' : `${dir}/package.json`);
            if (dir === '.' || dir === '' || dir === '/') break;
            dir = pathPosix.dirname(dir);
        }
        let found;
        try {
            found = await access.stat(candidates);
        } catch {
            return 'ambiguous';
        }
        const pkgPath = candidates.find((c) => found.get(c) && !found.get(c).missing);
        if (!pkgPath) return 'ambiguous'; // no manifest at all, up to the workspace root
        try {
            await verify([pkgPath], { strict: true });
        } catch {
            return 'ambiguous';
        }
        const rec = files.get(pkgPath);
        const src = rec && typeof rec.content === 'string' ? rec.content : null;
        if (src != null) {
            try {
                const pkg = JSON.parse(src);
                if (pkg && pkg.type === 'module') return 'module';
                if (pkg && pkg.type === 'commonjs') return 'commonjs';
            } catch {
                /* an unparsable manifest cannot control the decision */
            }
        }
        return 'ambiguous'; // the nearest manifest controls, and declares no explicit type
    }

    // The commit point shared by editBatch and writeWholeFile: optional lock,
    // then the facade's atomic compare-and-swap (or exclusive create). A
    // commit that THROWS (a custom facade, a transport) is a lost response,
    // not a refusal — the readback decides what happened.
    async function commitGuarded(rel, bytes, commitOpts, operationId) {
        let lock = null;
        if (typeof access.lock === 'function') {
            try {
                lock = await access.lock(`${rel}.okcode-lock`, operationId);
            } catch (err) {
                lock = { ok: false, reason: err.message };
            }
            if (!lock || !lock.ok) return { locked: true, reason: (lock && lock.reason) || 'locked' };
        }
        try {
            return await access.commit(rel, bytes, commitOpts);
        } catch (err) {
            return { outcome: 'response-lost', error: String((err && err.message) || err).slice(0, 500) };
        } finally {
            if (lock && typeof lock.release === 'function') await Promise.resolve(lock.release()).catch(() => {});
        }
    }

    // An INDEPENDENT readback, always attempted after a commit.
    async function readBack(rel) {
        try {
            const got = await access.read([rel]);
            return got.get(rel) || null;
        } catch {
            return null;
        }
    }

    // Re-sync from the files regardless of outcome — the index must reflect
    // reality, never the candidate it merely attempted. This is an OWN WRITE:
    // the bytes just published are already IN HAND (the readback), so the row
    // is updated directly from them — no re-read, and no tree walk. One stat
    // gets the AUTHORITATIVE size/mtime so the row compares correctly against
    // the very next verify() with no needless re-read — the cost lands here,
    // once, on the WRITE. Without a readback there is nothing trustworthy to
    // ingest — the row is dropped so the next verify() re-establishes it.
    async function ingestOwnWrite(rel, readback) {
        if (readback) {
            let liteAfter = null;
            try {
                liteAfter = (await access.stat([rel])).get(rel);
            } catch {
                liteAfter = null;
            }
            const ok = liteAfter && !liteAfter.missing;
            ingestOne(
                { path: rel, size: ok ? liteAfter.size : readback.length, mtime: ok ? liteAfter.mtime : null },
                readback,
            );
        } else {
            drop(rel);
        }
        scheduleFlush();
    }

    const api = {
        id,
        access,
        store,
        refresh,
        // The host-triggered sync point (DESIGN §6): rescan now. `force`
        // re-hashes everything instead of trusting (size, mtime).
        sync({ force = false } = {}) {
            return refresh({ sync: true, rehash: force });
        },
        flush,

        // ── structure ───────────────────────────────────────────────────
        async structure({ dir = null } = {}) {
            await ensureOpen();
            const prefix = dir ? String(dir).replace(/\\/g, '/') : null;
            const all = list().filter((f) => !prefix || f.rel.startsWith(prefix));
            const byDir = new Map();
            for (const f of all) {
                const d = pathPosix.dirname(f.rel);
                const e = byDir.get(d) || { dir: d, files: 0, lines: 0, symbols: 0 };
                e.files++;
                e.lines += f.lines || 0;
                e.symbols += (f.symbols || []).length;
                byDir.set(d, e);
            }
            const unparsed = all
                .filter((f) => !f.parsed && f.lang === 'javascript')
                .map((f) => ({ file: f.rel, reason: f.reason }));
            // The biggest single symbols — the sort of thing a file listing
            // hides (one 973-line function holding half a file). Top-level
            // only: a nested symbol is contained by its parent, so listing
            // both says the same lines twice.
            const largest = all
                .flatMap((f) =>
                    (f.symbols || [])
                        .filter((s) => !s.parent)
                        .map((s) => ({ name: s.name, file: f.rel, lines: s.lineEnd - s.lineStart + 1 })),
                )
                .sort((a, b) => b.lines - a.lines)
                .slice(0, 12);
            // RESULT-SET CHECK, bounded to the dozen files actually NAMED
            // here — never the whole tree the aggregate counts were drawn
            // from, which stay a coarse snapshot between explicit syncs.
            await resultSetCheck(largest.map((s) => s.file));
            return {
                id,
                files: all.length,
                lines: all.reduce((a, f) => a + (f.lines || 0), 0),
                symbols: all.reduce((a, f) => a + (f.symbols || []).length, 0),
                dirs: [...byDir.values()].sort((a, b) => b.symbols - a.symbols),
                unparsed,
                largest,
            };
        },

        // ── packages ────────────────────────────────────────────────────
        // What this project depends on, and what each of those exposes. Read
        // lazily and kept until asked to refresh: a dependency set changes on
        // an install, and re-learning it on every question would be a cost
        // with no event behind it.
        async packages({ refresh: again = false } = {}) {
            if (!again && packages.size) return [...packages.values()];
            const { packages: found, reason } = await scanPackages(access);
            if (reason && !found.size) return { error: reason };
            packages = found;
            if (store) {
                pendingPackages.push(...found.values());
                scheduleFlush();
            }
            return [...found.values()];
        },

        // One dependency, by name — including one that is merely installed
        // rather than declared. "Is it in our package.json" and "can I call
        // it" are different questions, and only the second is being asked.
        async package(name) {
            const known = packages.get(name);
            if (known) return known;
            const found = await readPackages(access, [String(name)]);
            const pkg = found.get(String(name));
            if (!pkg) return { error: `no package "${name}" under node_modules` };
            packages.set(pkg.name, pkg);
            if (store) {
                pendingPackages.push(pkg);
                scheduleFlush();
            }
            return pkg;
        },

        // ── find ────────────────────────────────────────────────────────
        // Name match first (exact, then prefix/substring), then the doc prose,
        // which in a well-commented codebase answers "where is X handled"
        // better than a name search does; then file content, attributed to
        // the symbol containing each matched line.
        async find(query, { kind = null, limit = 20 } = {}, _retried = false) {
            const q = String(query).toLowerCase();
            // ONE scoring rule for every hit, whatever index produced it. An
            // exact name match is an exact name match whether full-text
            // surfaced it or the substring scan did (scoring FTS hits flat
            // once ranked the real `execute` THIRD behind two symbols that
            // merely mention it). Scored on the bare name AND the path.
            const nameScore = (name, symPath) => {
                let best = 0;
                for (const candidate of [name, symPath]) {
                    if (!candidate) continue;
                    const n = String(candidate).toLowerCase();
                    const score = n === q ? 100 : n.startsWith(q) ? 70 : n.includes(q) ? 50 : 0;
                    if (score > best) best = score;
                }
                return best;
            };
            // A symbol found by both indexes is ONE hit: the better score
            // wins, and `alsoVia` records the other eyes. A later, weaker
            // channel must not overwrite `via` — the label is how a caller
            // judges how much to trust a hit.
            const out = new Map();
            const offer = (hit) => {
                const k = `${hit.file}:${hit.path || hit.name}`;
                const prior = out.get(k);
                if (!prior) {
                    out.set(k, hit);
                    return;
                }
                const strong = hit.score > prior.score ? hit : prior;
                const weak = strong === hit ? prior : hit;
                const also = new Set([...(strong.alsoVia || []), ...(weak.alsoVia || [])]);
                if (weak.via && weak.via !== strong.via) also.add(weak.via);
                out.set(k, {
                    ...strong,
                    ...(also.size ? { alsoVia: [...also] } : {}),
                    // A quote is worth keeping even when the name match won: it
                    // is the only channel that can say WHICH line matched.
                    quote: strong.quote || weak.quote || null,
                });
            };
            const symbolHit = (f, s, extra) => ({
                name: s.name,
                kind: s.kind,
                file: f.rel,
                path: s.path || s.name,
                parent: s.parent || null,
                lines: `${s.lineStart}-${s.lineEnd}`,
                span: s.lineEnd - s.lineStart + 1,
                signature: s.signature || null,
                doc: (s.doc || '').slice(0, 160) || null,
                exported: !!s.exported,
                at: f.hash,
                ...extra,
            });

            // Open BEFORE querying the store, so a full-text hit is checked
            // against the graph as it is now: a symbol whose file has since
            // been deleted lives on in the store until the next flush.
            await ensureOpen();
            // Both full-text indexes, strictly, then both loosely — and only
            // if strict found nothing ANYWHERE.
            let symbolHits = store ? store.search(query, limit, 'and') : null;
            let contentHits = store ? store.searchContent(query, Math.max(6, limit), { mode: 'and' }) : null;
            // A LOOSE pass returns "shares some words with the question" — a
            // weaker claim than any strict match and weaker than a semantic
            // one, so it is scored below both.
            let loose = false;
            if (
                store &&
                String(query).trim().split(/\s+/).length > 1 &&
                !(symbolHits || []).length &&
                !(contentHits || []).length
            ) {
                loose = true;
                symbolHits = store.search(query, limit, 'or');
                contentHits = store.searchContent(query, Math.max(6, limit), { mode: 'or' });
            }
            const PROSE = loose ? 18 : 40;
            const CONTENT = loose ? 16 : 35;
            // The ladder, top to bottom: exact name 100, name prefix 70, name
            // substring 50, PROSE via full-text 40, prose via substring 25,
            // content 35. No prose match outranks a name match: searching
            // "budget" wants DEFAULT_BUDGET before three symbols that mention
            // budgets in passing.
            for (const h of symbolHits || []) {
                if (kind && h.kind !== kind) continue;
                const f = byRel(h.file);
                if (!f) continue;
                offer(
                    symbolHit(f, h, {
                        score: nameScore(h.name, h.path) || PROSE,
                        via: loose ? 'fts~' : 'fts',
                        relevance: h.relevance || 0,
                    }),
                );
            }
            // Substring matching still runs and merges: FTS misses partial
            // identifiers, and the two are complementary rather than redundant.
            for (const f of list()) {
                for (const s of f.symbols || []) {
                    if (kind && s.kind !== kind) continue;
                    const score = nameScore(s.name, s.path) || ((s.doc || '').toLowerCase().includes(q) ? 25 : 0);
                    if (!score) continue;
                    offer(symbolHit(f, s, { score }));
                }
            }
            // The content eye. A hit on the FILE is not an answer — "it is
            // somewhere in dashboard.js" is what the caller already suspected
            // — so every matched line is attributed to the symbol containing
            // it. These are the only hits that quote FILE TEXT, so these are
            // the files verified before a single window is cut.
            if (contentHits && contentHits.length) await verify(contentHits.map((h) => h.file));
            for (const hit of contentHits || []) {
                const f = byRel(hit.file);
                if (!f || typeof f.content !== 'string') continue;
                const lines = quoteLines(f.content, query, 3);
                // Lines group by the symbol containing them, best citation
                // wins — otherwise the survivor is whichever line came FIRST.
                const best = new Map();
                for (const ln of lines.length ? lines : [{ line: 1, text: '', n: 0 }]) {
                    const owner = ownerOf(f, ln.line);
                    const key = owner ? owner.path || owner.name : f.rel;
                    const prior = best.get(key);
                    if (!prior || (ln.n || 0) > (prior.ln.n || 0)) best.set(key, { owner, ln });
                }
                for (const { owner, ln } of best.values()) {
                    // `quote` stays a one-line label; `window` is what a
                    // caller reads, with its own line range.
                    const win = windowAt(f, ln.line, owner);
                    const common = {
                        score: CONTENT,
                        via: loose ? 'content~' : 'content',
                        relevance: hit.relevance,
                        quote: `${f.rel}:${ln.line}  ${ln.text}`,
                        line: ln.line,
                        window: win,
                    };
                    offer(
                        owner
                            ? symbolHit(f, owner, common)
                            : {
                                  name: f.rel,
                                  kind: f.lang,
                                  file: f.rel,
                                  path: f.rel,
                                  parent: null,
                                  // A file-level hit spans the window it shows,
                                  // not the single line it matched.
                                  lines: win ? `${win.lineStart}-${win.lineEnd}` : `${ln.line}-${ln.line}`,
                                  span: win ? win.lineEnd - win.lineStart + 1 : 1,
                                  signature: null,
                                  doc: null,
                                  exported: false,
                                  at: f.hash,
                                  ...common,
                              },
                    );
                }
            }
            // Tier first (how the query matched), then full-text relevance
            // inside the tier, then the smaller symbol.
            const results = [...out.values()]
                .sort((a, b) => b.score - a.score || (b.relevance || 0) - (a.relevance || 0) || a.span - b.span)
                .slice(0, limit);
            // RESULT-SET CHECK: this additionally catches a file a symbol hit
            // still points at but that has since been deleted (or whose
            // symbols moved), and re-runs once.
            if (!_retried && results.length) {
                if ((await resultSetCheck(results.map((h) => h.file))).changed) {
                    return api.find(query, { kind, limit }, true);
                }
            }
            return results;
        },

        // ── ask ─────────────────────────────────────────────────────────
        // find() with the third eye attached: semantic search through one
        // embedding profile. A separate verb because a vector query has to
        // embed the question first, and the two lexical eyes must not wait
        // on a model for callers who only wanted a name. Unavailable (throws
        // OKCODE_NO_EMBEDDINGS) when no profile is configured.
        //
        // `query` is a string, or { text?, vector, identity? } when the host
        // has already embedded it (one embed shared across several indexes):
        // the vector is searched as-is — NO embed call is made for it — and
        // must match the profile's dims (OKCODE_DIMS_MISMATCH) and, when
        // given, its identity (OKCODE_IDENTITY_MISMATCH). The lexical eyes
        // need words: with a vector and no text the answer is semantic only;
        // with both, the three eyes fuse exactly as for a string.
        async ask(query, { limit = 12, profile = null } = {}) {
            if (!store || !store.hasProfiles()) {
                const err = new Error(`workspace "${id}" has no embedding profile — ask() is unavailable`);
                err.code = 'OKCODE_NO_EMBEDDINGS';
                throw err;
            }
            const q = asQuery(query);
            // Ask for a FULL set, not half of one: the query is embedded once
            // either way, and a smaller ask only discards answers — a phrase
            // missed by both lexical eyes was found by the vector eye at rank
            // 6, then dropped because only four were requested. Semantic
            // first, so a mismatched vector refuses before any lexical work.
            const semantic = await store.ask(q.vector ? q : q.text, { profile, limit: Math.max(8, limit) });
            if (!q.text) await ensureOpen();
            const lexical = q.text ? await api.find(q.text, { limit }) : [];
            const seen = new Set(lexical.map((h) => `${h.file}:${h.path || h.name}`));
            // A vector hit's chunk was located at EMBEDDING time. It is a
            // reliable locator and an unreliable quote: the file may have
            // moved under it since. So the files that matched are verified
            // here, once, and every line number and every character of text
            // below comes from the verified record — the chunk only says WHERE.
            await verify(semantic.map((s) => s.file));
            const out = [...lexical];
            for (const s of semantic) {
                const f = byRel(s.file);
                if (!f || typeof f.content !== 'string') continue;
                const off = typeof s.start === 'number' ? Math.min(s.start, f.content.length) : 0;
                // Skip leading blank lines so the hit names where the chunk's
                // code starts, not the gap before it.
                const lead = /^\s*/.exec(f.content.slice(off, off + 400))[0];
                const at = off + lead.slice(0, lead.lastIndexOf('\n') + 1).length;
                const line = f.content.slice(0, at).split('\n').length;
                const owner = ownerOf(f, line);
                const win = windowAt(f, line, owner);
                const key = `${f.rel}:${owner ? owner.path || owner.name : f.rel}`;
                if (seen.has(key)) continue;
                seen.add(key);
                out.push({
                    name: owner ? owner.name : f.rel,
                    kind: owner ? owner.kind : f.lang,
                    file: f.rel,
                    path: owner ? owner.path || owner.name : f.rel,
                    parent: owner ? owner.parent || null : null,
                    lines: owner
                        ? `${owner.lineStart}-${owner.lineEnd}`
                        : win
                          ? `${win.lineStart}-${win.lineEnd}`
                          : `${line}-${line}`,
                    span: owner ? owner.lineEnd - owner.lineStart + 1 : win ? win.lineEnd - win.lineStart + 1 : 1,
                    signature: owner ? owner.signature || null : null,
                    doc: owner ? (owner.doc || '').slice(0, 160) || null : null,
                    exported: !!(owner && owner.exported),
                    at: f.hash,
                    score: 20 + Math.round(20 * (s.score || 0)),
                    via: 'vector',
                    relevance: s.score || 0,
                    // The one-line label, from the CURRENT file rather than the
                    // stored chunk — the chunk located it, the file says it.
                    quote: `${f.rel}:${line}  ${(f.content.split('\n')[line - 1] || '').trim()}`.slice(0, 200),
                    line,
                    window: win,
                });
            }
            // Lexical certainty first, then semantic proximity. A name match is
            // knowledge; a vector hit is a good guess, and mixing the two
            // rankings would let a guess outrank the thing itself.
            return out.sort((a, b) => b.score - a.score || (b.relevance || 0) - (a.relevance || 0)).slice(0, limit);
        },

        // ── cheap listings (rows only) ──────────────────────────────────
        // What the index already knows about files and symbols, served from
        // the rows — NO file content is read, ever, and by default no facade
        // call is made at all. This is the listing a host builds menus from
        // (it lists by metadata and never opens bodies to list); it is as
        // fresh as the last scan (`asOf`), which on a warm store is the
        // open-time walk. `stat: true` adds ONE metadata stat for the listed
        // files and flags each that moved (`moved: true`) or vanished
        // (`gone: true`) since — still no read, no hash, no re-ingest; a
        // moved file is re-indexed by the next sync or verifying verb.
        //
        // `hash` is the row's hash as of that scan: a locator, not an `at`.
        // read()/outline() verify and hand out the `at` an edit needs.

        // [{ file, size, lines, lang, symbols, indexed, hash }] under `dir`.
        async files({ dir = null, lang = null, limit = 0, stat = false } = {}) {
            await ensureOpen();
            // A directory, not a string prefix: `lib` lists lib/, not libs/.
            const d = String(dir || '')
                .replace(/\\/g, '/')
                .replace(/^\.(\/|$)/, '')
                .replace(/^\/+|\/+$/g, '');
            const prefix = d ? `${d}/` : null;
            let all = list()
                .filter((f) => !prefix || f.rel.startsWith(prefix))
                .filter((f) => !lang || f.lang === lang)
                .sort((a, b) => (a.rel < b.rel ? -1 : a.rel > b.rel ? 1 : 0));
            const total = all.length;
            if (limit > 0) all = all.slice(0, limit);
            const rows = all.map((f) => ({
                file: f.rel,
                size: f.size == null ? null : f.size,
                lines: f.lines == null ? null : f.lines,
                lang: f.lang || null,
                symbols: (f.symbols || []).length,
                indexed: f.indexed !== false,
                hash: f.hash || null,
            }));
            if (stat) await flagMoved(rows);
            return { id, dir: prefix, total, truncated: rows.length < total, asOf: lastScanAt || null, files: rows };
        },

        // One file's SYMBOL TABLE from the stored rows: name, path, kind,
        // parent, lines, signature, exported, doc. The cheap sibling of
        // outline(): no text is read, so no regions (an extension's
        // template/style pieces) and no chunk fallback for a file without
        // symbols — outline() carves those from the text.
        async symbols(filePath, { limit = 0, kind = null, stat = false } = {}) {
            await ensureOpen();
            const f = fileOf(filePath);
            if (!f) {
                // Not explainTarget(): its miss path greps the workspace text.
                const base = String(filePath || '')
                    .replace(/\\/g, '/')
                    .split('/')
                    .pop();
                const near = list()
                    .filter((x) => base && x.rel.split('/').pop() === base)
                    .map((x) => x.rel)
                    .slice(0, 4);
                return {
                    ok: false,
                    reason: `no indexed file matching "${filePath}"${near.length ? ` — did you mean ${near.join(', ')}?` : ''}`,
                    ...(near.length ? { candidates: near } : {}),
                };
            }
            let syms = (f.symbols || []).filter((s) => !kind || s.kind === kind);
            const total = syms.length;
            if (limit > 0) syms = syms.slice(0, limit);
            const out = {
                ok: true,
                file: f.rel,
                size: f.size == null ? null : f.size,
                lines: f.lines == null ? null : f.lines,
                lang: f.lang || null,
                indexed: f.indexed !== false,
                hash: f.hash || null,
                asOf: lastScanAt || null,
                total,
                truncated: syms.length < total,
                symbols: syms.map((s) => ({
                    name: s.name,
                    path: s.path || s.name,
                    kind: s.kind,
                    parent: s.parent || null,
                    lineStart: s.lineStart,
                    lineEnd: s.lineEnd,
                    span: s.lineEnd - s.lineStart + 1,
                    signature: s.signature || null,
                    exported: !!s.exported,
                    doc: s.doc ? String(s.doc).slice(0, 200) : null,
                })),
            };
            if (stat) {
                const [row] = await flagMoved([{ file: f.rel, size: f.size }]);
                if (row.moved) out.moved = true;
                if (row.gone) out.gone = true;
                if (row.unverified) out.unverified = true;
            }
            return out;
        },

        // ── outline ─────────────────────────────────────────────────────
        // THE SYMBOL TABLE OF ONE FILE — what it contains, without its
        // contents. This is how a resource gets inspected in PIECES rather
        // than judged whole: asking "is this 455-line file relevant?" is close
        // to a coin flip; asking it of twenty-odd named pieces is a filter,
        // and only the survivors cost a body read. Verified first, and the
        // `at` comes back with it.
        async outline(filePath, { limit = 80 } = {}) {
            await ensureOpen();
            const f = fileOf(filePath);
            if (!f) return { ok: false, reason: await explainTarget(filePath) };
            if (f.indexed === false) {
                return { ok: false, reason: `${f.rel} is not indexed as text (${f.reason || 'binary or withheld'})` };
            }
            // A plain file with symbols is answered from its rows, which need
            // no text: stat-first (as for find's results), reading only if
            // it moved. Regions and chunks are carved from the text, so
            // those files verify (and read) as before.
            if (!extensionFor(f.path) && (f.symbols || []).length) await resultSetCheck([f.path]);
            else await verify([f.path]);
            let fresh = files.get(f.path);
            if (!fresh) return { ok: false, reason: `${f.rel} no longer exists` };
            // Moved and lost every symbol: the rich paths below need text.
            if (!(fresh.symbols || []).length && typeof fresh.content !== 'string') {
                await verify([f.path]);
                fresh = files.get(f.path);
                if (!fresh) return { ok: false, reason: `${f.rel} no longer exists` };
            }
            const syms = fresh.symbols || [];
            const max = opts.outlineMaxLines;
            const piecesOut = (by, flat, mapKind) => ({
                ok: true,
                file: fresh.rel,
                lines: fresh.lines,
                at: fresh.hash,
                by,
                total: flat.length,
                truncated: flat.length > limit,
                symbols: flat.slice(0, limit).map((p) => ({
                    name: p.name,
                    path: `${fresh.rel}:${p.lineStart}-${p.lineEnd}`,
                    kind: mapKind(p),
                    parent: null,
                    lineStart: p.lineStart,
                    lineEnd: p.lineEnd,
                    span: Math.max(1, p.lineEnd - p.lineStart + 1),
                    signature: null,
                    doc: p.doc,
                })),
            });
            // An EXTENSION's structural regions, when there are any, are
            // checked BEFORE symbol count: an okjs file carries exactly one
            // coarse symbol (the component tag), which would otherwise hide
            // the template/style breakdown a caller wants to read or edit by
            // piece. `source: 'script'` regions (one import statement) are
            // reference-sized, not a "piece". Each piece addresses through the
            // same `file:from-to` range syntax, so read()/editBatch() need
            // nothing new to reach one.
            const analysis = extensionFor(fresh.path) ? await analysisOf(fresh) : null;
            const regions = analysis
                ? (analysis.regions || [])
                      .filter((r) => r.source === 'definition' || r.source === 'block')
                      .map((r) => ({ region: r, span: r.contentRange || r.valueRange || r.range }))
                      .filter(({ span }) => span && span.end > span.start)
                      .map(({ region, span }) => ({
                          name: region.role || region.kind,
                          kind: region.kind,
                          lineStart: span.loc.start.line,
                          lineEnd: Math.max(span.loc.start.line, span.loc.end.line),
                          doc: [region.role, region.format || region.language].filter(Boolean).join(' ') || null,
                      }))
                : [];
            if (regions.length) {
                const all = String((await textOfFresh(fresh.path)) || '').split('\n');
                const cut = [];
                for (const p of carveRegions(regions)) {
                    const body = all.slice(p.lineStart - 1, p.lineEnd).join('\n');
                    for (const [a, b] of splitPiece(p.lineStart, p.lineEnd, body, max)) {
                        cut.push({ ...p, lineStart: a, lineEnd: b });
                    }
                }
                return piecesOut('region', labelPieces(cut, all), (p) => p.kind);
            }
            // NOT EVERY FILE HAS SYMBOLS, AND THE IMPORTANT ONES OFTEN DO NOT
            // (an okjs component is one big template literal). So the pieces
            // fall back to CHUNKS, which the chunker cuts at real seams. `by`
            // says which path answered.
            if (!syms.length) {
                const src = String((await textOfFresh(fresh.path)) || '');
                const before = (n) => src.slice(0, n).split('\n').length;
                const all = src.split('\n');
                // A chunk is not automatically small: an unbounded chunk in
                // front of a judge is the same whole-file read as an unbounded
                // region.
                const cut = [];
                for (const c of chunkCode(src, fresh.rel)) {
                    const lineStart = before(c.start);
                    const lineEnd = Math.max(
                        lineStart,
                        before(c.end) - (String(src[c.end - 1] || '') === '\n' ? 1 : 0),
                    );
                    for (const [a, b] of splitPiece(lineStart, lineEnd, String(c.text || ''), max)) {
                        cut.push({ name: 'chunk', kind: 'chunk', lineStart: a, lineEnd: b, doc: null });
                    }
                }
                return piecesOut('chunk', labelPieces(cut, all), () => 'chunk');
            }
            return {
                ok: true,
                file: fresh.rel,
                lines: fresh.lines,
                at: fresh.hash,
                by: 'symbol',
                total: syms.length,
                truncated: syms.length > limit,
                symbols: syms.slice(0, limit).map((s) => ({
                    name: s.name,
                    path: s.path || s.name,
                    kind: s.kind,
                    parent: s.parent || null,
                    lineStart: s.lineStart,
                    lineEnd: s.lineEnd,
                    span: s.lineEnd - s.lineStart + 1,
                    signature: s.signature || null,
                    doc: s.doc ? String(s.doc).slice(0, 200) : null,
                })),
            };
        },

        // Is this EXACT text anywhere in the workspace? The cheapest question,
        // and the one a "did my edit actually land" check needs: a CSS class
        // or an attribute value is not a symbol. RESULT-SET CHECK: only the
        // files about to back a hit are stat-checked; a change re-runs once.
        async mentions(text, { limit = 5 } = {}, _retried = false) {
            await ensureOpen();
            const hits = await mentionsOf(text, limit);
            if (!_retried && hits.length && (await resultSetCheck(hits.map((h) => h.rel))).changed) {
                return api.mentions(text, { limit }, true);
            }
            return hits;
        },

        // ── refs ────────────────────────────────────────────────────────
        // "Where is this USED?" — the question that actually costs cycles.
        // The `textual` confidence tier: match the text, then attribute every
        // hit to the symbol whose line range contains it. Attribution is what
        // makes it more than grep — a hit reads as "inside assembleContext",
        // a place you can then read or edit by name. Labelled `textual`: a
        // name in a comment or a string counts here.
        async refs(query, { limit = 40, includeSelf = false } = {}, _retried = false) {
            await ensureOpen();
            // isSecret as well as the flag: a store written under older rules
            // can still hold `indexed: true` for a file that must not be read.
            await ensureTexts(list().filter((f) => f.indexed !== false && !isSecret(f.path)));
            const q = String(query);
            const out = [];
            outer: for (const f of list()) {
                if (f.indexed === false || isSecret(f.path) || typeof f.content !== 'string') continue;
                const lines = f.content.split('\n');
                for (let i = 0; i < lines.length; i++) {
                    if (!lines[i].includes(q)) continue;
                    const ln = i + 1;
                    const owner = ownerOf(f, ln);
                    // The declaration itself is not a use of itself.
                    if (!includeSelf && owner && owner.name === q && owner.lineStart === ln) continue;
                    out.push({
                        file: f.rel,
                        line: ln,
                        symbol: owner ? owner.path || owner.name : '(top level)',
                        kind: owner ? owner.kind : null,
                        text: lines[i].trim().slice(0, 160),
                        confidence: 'textual',
                        at: f.hash,
                    });
                    if (out.length >= limit) break outer;
                }
            }
            if (!_retried && out.length && (await resultSetCheck(out.map((h) => h.file))).changed) {
                return api.refs(query, { limit, includeSelf }, true);
            }
            return out;
        },

        // ── read ────────────────────────────────────────────────────────
        // One verb for "show me that", whether "that" is a symbol, a file, or
        // a range of a file:
        //   read('budgetOf')                        a symbol
        //   read('store.js#openStore.save')         a symbol, qualified
        //   read('public/dashboard.html')           a whole file
        //   read('public/dashboard.html:269-339')   a range
        async read(query, { from = null, to = null } = {}) {
            await ensureOpen();
            // A trailing :N-M (or #N-M) is a range, but a bare `store.js#name`
            // is not, and neither is a Windows drive letter.
            const range = parseRange(query);
            if (range) {
                from = from || range.from;
                to = to || range.to;
                query = range.file;
            }
            const r = await resolve(query);
            // Not a symbol, but a file the index knows — serve it rather than
            // sending the caller to the shell for bytes we can reach.
            if (!r.ok && !r.isFile && (from || to)) {
                const guess = fileOf(query);
                if (guess) return api.readAt(guess.rel, from, to || from);
            }
            if (!r.ok && r.isFile) {
                return from || to ? api.readAt(r.file.rel, from || 1, to || from) : api.readFile(r.file.rel);
            }
            if (!r.ok) return r;
            if (from || to) return api.readAt(r.file.rel, from || r.symbol.lineStart, to || r.symbol.lineEnd);
            const { text, file, symbol } = await sliceSymbol(r.file, r.symbol);
            return {
                ok: true,
                name: symbol.name,
                kind: symbol.kind,
                file: file.rel,
                path: symbol.path || symbol.name,
                parent: symbol.parent || null,
                lineStart: symbol.lineStart,
                lineEnd: symbol.lineEnd,
                at: file.hash,
                body: text,
            };
        },

        // A whole file, verified and capped by LINES rather than by a byte
        // count, so a truncated read still ends on a line boundary and still
        // says exactly how to ask for the rest. A silent clip in the middle of
        // a token is the shape of truncation that makes a caller edit against
        // half a statement.
        async readFile(filePath, { maxLines = opts.fileReadLines } = {}) {
            await ensureOpen();
            const f = fileOf(filePath);
            if (!f) return { ok: false, reason: await explainTarget(filePath) };
            if (f.indexed === false) {
                return { ok: false, reason: `${f.rel} is not indexed as text (${f.reason || 'binary or withheld'})` };
            }
            await verify([f.path]);
            const fresh = files.get(f.path);
            if (!fresh) return { ok: false, reason: `${f.rel} no longer exists` };
            const src = (await textOfFresh(fresh.path)) ?? '';
            const cur = files.get(f.path) || fresh;
            const lines = src.split('\n');
            const shown = Math.min(lines.length, maxLines);
            return {
                ok: true,
                file: cur.rel,
                lineStart: 1,
                lineEnd: shown,
                lines: lines.length,
                at: cur.hash,
                body: lines.slice(0, shown).join('\n'),
                ...(shown < lines.length
                    ? {
                          truncated: true,
                          more: `${cur.rel}:${shown + 1}-${lines.length}`,
                          note: `showing lines 1-${shown} of ${lines.length}; read "${cur.rel}:${shown + 1}-${Math.min(lines.length, shown + maxLines)}" for the next stretch`,
                      }
                    : {}),
            };
        },

        async readAt(filePath, lineStart, lineEnd, at = null) {
            await ensureOpen();
            const f = fileOf(filePath);
            if (!f) return { ok: false, reason: await explainTarget(filePath) };
            // Verify BEFORE judging the caller's `at`, so a range replayed
            // against a file that changed a millisecond ago is caught, and one
            // replayed against a file that did not is not falsely refused.
            await verify([f.path]);
            const fresh = files.get(f.path);
            if (!fresh) return { ok: false, reason: `${f.rel} no longer exists` };
            const src = (await textOfFresh(fresh.path)) ?? '';
            const cur = files.get(f.path) || fresh;
            if (at && at !== cur.hash) {
                return {
                    ok: false,
                    stale: true,
                    reason: 'the file changed since that range was resolved',
                    at: cur.hash,
                };
            }
            const lines = src.split('\n');
            const total = lines.length;
            const start = Math.max(1, Number(lineStart) || 1);
            const end = Math.min(total, Math.max(start, Number(lineEnd) || start));
            return {
                ok: true,
                file: cur.rel,
                lineStart: start,
                lineEnd: end,
                lines: total,
                at: cur.hash,
                body: lines.slice(start - 1, end).join('\n'),
            };
        },

        // ── editBatch ───────────────────────────────────────────────────
        // The ONLY writer. One immutable snapshot of ONE file, taken after
        // strict verification; every target resolved against that exact
        // snapshot; validated as a CANDIDATE before the target is touched;
        // published atomically or refused; read back independently; reported
        // with a receipt honest about what is actually known.
        //
        // `at` is MANDATORY — the snapshot token from the read that supplied
        // the edited body. A stale or missing token is refused with the
        // CURRENT token, never silently rebased and never fuzzy-matched onto a
        // new occurrence. find/ask locate; only a precise read establishes the
        // snapshot an edit may target.
        async editBatch({ at = null, edits = [] } = {}) {
            const todo = Array.isArray(edits) ? edits.filter(Boolean) : [];
            if (!todo.length) return { ok: false, reason: 'editBatch needs at least one edit' };
            if (!at) {
                return {
                    ok: false,
                    reason: 'at is required — read the file or symbol first and pass back its token; find/ask locate, they do not authorize an edit',
                };
            }
            await ensureOpen();
            const first = await fileForTarget(todo[0].target);
            if (!first) return { ok: false, reason: await explainTarget(todo[0].target) };

            // Strict verification BEFORE anything about this file is trusted.
            try {
                await verify([first.path], { strict: true });
            } catch (err) {
                return {
                    ok: false,
                    reason: `could not verify ${first.rel} — refusing rather than trusting a cached offset: ${err.message}`,
                };
            }
            const snapshot = files.get(first.path);
            if (!snapshot) return { ok: false, reason: `${first.rel} no longer exists` };
            if (at !== snapshot.hash) {
                return {
                    ok: false,
                    stale: true,
                    reason: 'the file changed since that token was issued',
                    at: snapshot.hash,
                };
            }
            if (typeof snapshot.content !== 'string') {
                return { ok: false, reason: `${snapshot.rel} is not indexed as text — it cannot be edited here` };
            }
            if (snapshot.lossless === false) {
                return {
                    ok: false,
                    reason: `${snapshot.rel} could not be decoded losslessly as UTF-8 — refusing to compose an edit on top of an uncertain decode`,
                };
            }

            // Every target resolved against the SAME snapshot; the whole batch
            // refused on the first target that names a different file, is
            // ambiguous, or does not resolve at all.
            const resolved = [];
            for (const e of todo) {
                const f = await fileForTarget(e.target);
                if (!f || f.path !== snapshot.path) {
                    return {
                        ok: false,
                        crossFile: true,
                        reason: `all edits in one batch must target the same file — "${e.target}" resolves to ${f ? f.rel : 'an unknown file'}, not ${snapshot.rel}`,
                    };
                }
                const r = resolveTargetInSnapshot(snapshot, e.target);
                if (!r.ok) return r;
                resolved.push({
                    target: e.target,
                    name: r.name,
                    kind: r.kind,
                    start: r.start,
                    end: r.end,
                    lineStart: r.lineStart,
                    lineEnd: r.lineEnd,
                    body: e.body,
                });
            }

            const byStart = [...resolved].sort((a, b) => a.start - b.start);
            for (let i = 1; i < byStart.length; i++) {
                if (byStart[i].start < byStart[i - 1].end) {
                    return {
                        ok: false,
                        overlap: true,
                        reason: `edit targets overlap: "${byStart[i - 1].target}" and "${byStart[i].target}"`,
                    };
                }
            }

            // Compose against the snapshot, highest offset downward, so no
            // edit's coordinates are shifted by another edit in the same batch.
            // Inserted text is normalised to the file's own dominant line
            // ending; everything outside an edit is sliced verbatim — no
            // split/join round trip, which is what keeps CRLF and every
            // untouched byte exactly as they were.
            const eol = textUtil.dominantEol(snapshot.content);
            let candidate = snapshot.content;
            for (const e of [...byStart].reverse()) {
                e.normalisedBody = textUtil.normaliseEol(String(e.body), eol);
                candidate = candidate.slice(0, e.start) + e.normalisedBody + candidate.slice(e.end);
            }
            // Each edit's span in the CANDIDATE — the bookkeeping both the
            // diagnostic offset map and the receipt need.
            let shift = 0;
            for (const e of byStart) {
                e.candidateStart = e.start + shift;
                e.candidateEnd = e.candidateStart + e.normalisedBody.length;
                shift += e.normalisedBody.length - (e.end - e.start);
            }
            const mapOffset = (baselineOffset) => {
                let sh = 0;
                for (const e of byStart) {
                    if (e.start > baselineOffset) break;
                    sh += e.candidateEnd - e.candidateStart - (e.end - e.start);
                }
                return baselineOffset + sh;
            };
            const diagTargets = resolved.map((e) => ({
                name: e.name || e.target,
                baselineStart: e.start,
                baselineEnd: e.end,
                candidateStart: e.candidateStart,
                candidateEnd: e.candidateEnd,
            }));

            // Validate the CANDIDATE — text, never a path — before the target
            // is touched, locally: the target machine needs no toolchain.
            // Commit iff no NEW diagnostic, compared structurally against the
            // baseline, never by count. Baseline and candidate run
            // CONCURRENTLY; both are needed, because a repair may land even
            // while the baseline is already broken, provided the candidate
            // introduces nothing NEW.
            const moduleKind = await resolveModuleKind(snapshot.path);
            const pal = await palette();
            const [baselineV, candidateV] = await Promise.all([
                validateSourceAsync({ rel: snapshot.rel, text: snapshot.content, moduleKind, palette: pal }),
                validateSourceAsync({ rel: snapshot.rel, text: candidate, moduleKind, palette: pal }),
            ]);
            const cmp = compareDiagnostics(baselineV.diagnostics, candidateV.diagnostics, {
                mapOffset,
                targets: diagTargets,
            });

            // candidateV.validators' status is a WHOLE-CANDIDATE scan — it
            // fires 'failed' on a literal this edit never touched. The receipt
            // reports convention-color's status from cmp.newDiagnostics
            // (scoped to the edited regions) so it only ever judges the lines
            // this edit added.
            const newSources = new Set(cmp.newDiagnostics.map((d) => d.source));
            const reportedValidators = candidateV.validators.map((v) =>
                v.validator === 'convention-color'
                    ? { ...v, status: newSources.has('convention-color') ? 'failed' : 'passed' }
                    : v,
            );

            if (cmp.newDiagnostics.length) {
                return {
                    ok: false,
                    reason:
                        `candidate introduces ${cmp.newDiagnostics.length} new diagnostic(s): ` +
                        cmp.newDiagnostics
                            .map((d) => d.message)
                            .slice(0, 3)
                            .join('; '),
                    newDiagnostics: cmp.newDiagnostics,
                    validation: reportedValidators,
                    moduleKind,
                };
            }

            // ── the commit point ────────────────────────────────────────
            const candidateBuf = Buffer.from(candidate, 'utf8');
            const candidateHash = sha1(candidateBuf);
            const operationId = crypto.randomBytes(8).toString('hex');
            const commit = await commitGuarded(
                snapshot.path,
                candidateBuf,
                { expectedHash: snapshot.hash },
                operationId,
            );
            if (commit.locked) {
                return {
                    ok: false,
                    locked: true,
                    reason: `could not acquire the write lock for ${snapshot.rel} (${commit.reason})`,
                };
            }
            if (commit.outcome === 'stale') {
                return {
                    ok: false,
                    stale: true,
                    reason: 'the file changed since that token was issued',
                    at: commit.current || commit.hash || null,
                };
            }
            if (commit.outcome === 'noatomic') {
                return {
                    ok: false,
                    reason: `${snapshot.rel} could not be published atomically${commit.error ? ` (${commit.error})` : ''} — nothing was touched`,
                };
            }
            if (commit.outcome === 'missing') return { ok: false, reason: `${snapshot.rel} no longer exists` };
            if (commit.outcome === 'symlink') {
                return {
                    ok: false,
                    reason: `${snapshot.rel} is a symlink — refusing to replace the link itself rather than what it points to`,
                };
            }

            // Past this line the publish ran (outcome 'ok') OR its RESPONSE
            // was lost — in both cases the file may already have changed, so
            // an independent readback is ALWAYS attempted before anything is
            // reported.
            const readback = await readBack(snapshot.path);
            const readbackHash = readback ? sha1(readback) : null;
            const commitReportedHash = commit.outcome === 'ok' ? commit.hash : null;
            const outcome = classifyCommit({
                commit,
                candidateHash,
                beforeHash: snapshot.hash,
                readbackHash,
                readAvailable: !!readback,
            });
            await ingestOwnWrite(snapshot.path, readback);

            let diff = [];
            try {
                diff = textUtil.hunksFor(
                    snapshot.content,
                    candidate,
                    byStart.map((e) => ({
                        start: e.start,
                        end: e.end,
                        body: e.normalisedBody,
                        target: e.name || e.target,
                    })),
                );
            } catch {
                /* the commit already happened; a diff-rendering failure must not hide the outcome */
            }

            const receipt = {
                ok: outcome === 'ok',
                outcome,
                operationId,
                file: snapshot.rel,
                beforeHash: snapshot.hash,
                candidateHash,
                commitReportedHash,
                readbackHash,
                edits: resolved.map((e) => ({
                    target: e.target,
                    name: e.name,
                    before: { start: e.start, end: e.end, lineStart: e.lineStart, lineEnd: e.lineEnd },
                    after: { start: e.candidateStart, end: e.candidateEnd },
                })),
                diff,
                validation: reportedValidators,
                coverage: candidateV.coverage,
                moduleKind,
                baselineBroken: cmp.baselineBroken,
                resolved: cmp.resolved,
                remaining: cmp.remaining,
                newDiagnostics: [],
            };
            if (commit.error) receipt.commitError = commit.error;
            if (outcome !== 'ok') {
                receipt.reason = `commit outcome: ${outcome} — read ${snapshot.rel} again before trying anything further`;
            }
            return receipt;
        },

        // A one-edit batch. `read` takes a symbol, a file, or `path:from-to`;
        // so does this, because a caller that read a region by range has no
        // symbol to name when it wants to change that region — an HTML
        // template or a CSS rule is not a symbol at all.
        edit(query, newBody, { at = null, from = null, to = null } = {}) {
            const m = /^(.*?):(\d+)(?:-(\d+))?$/.exec(String(query || ''));
            let target = query;
            if (m && m[1].length > 1) {
                const F = from || Number(m[2]);
                const T = to || (m[3] ? Number(m[3]) : Number(m[2]));
                target = `${m[1]}:${F}-${T}`;
            } else if (from) {
                target = `${query}:${from}-${to || from}`;
            }
            return api.editBatch({ at, edits: [{ target, body: newBody }] });
        },

        // A positional range edit; a thin wrapper, not a second, weaker writer.
        writeAt(filePath, lineStart, lineEnd, newBody, { at = null } = {}) {
            return api.editBatch({ at, edits: [{ target: `${filePath}:${lineStart}-${lineEnd}`, body: newBody }] });
        },

        // Whole-file writes. `create: true` publishes a brand-new file through
        // the facade's atomic exclusive create; replacing an EXISTING file is
        // not a separate writer at all — it is editBatch with a single target
        // spanning the whole file, with the exact same snapshot/validate/
        // commit/readback guarantees as any other edit.
        async writeWholeFile(relPath, text, { expectedHash = null, create = false } = {}) {
            await ensureOpen();
            if (!create) {
                if (!expectedHash) {
                    return {
                        ok: false,
                        reason: 'replacing an existing file requires expectedHash — read it first and pass back its token',
                    };
                }
                const f = fileOf(relPath);
                if (!f) return { ok: false, reason: `${relPath} does not exist — pass create: true to create it` };
                return api.editBatch({
                    at: expectedHash,
                    edits: [{ target: `${f.rel}:1-${Number.MAX_SAFE_INTEGER}`, body: text }],
                });
            }

            const rel = String(relPath || '')
                .replace(/\\/g, '/')
                .replace(/^\.\//, '');
            if (!rel || rel.startsWith('/') || /^[a-zA-Z]:/.test(rel) || rel.split('/').includes('..')) {
                return { ok: false, reason: `"${relPath}" is not a workspace-relative path` };
            }
            const moduleKind = await resolveModuleKind(rel);
            // A new file has no baseline, so ANY diagnostic refuses it.
            const v = await validateSourceAsync({ rel, text: String(text), moduleKind });
            if (v.diagnostics.length) {
                return {
                    ok: false,
                    reason: `candidate does not validate: ${v.diagnostics
                        .map((d) => d.message)
                        .slice(0, 3)
                        .join('; ')}`,
                    newDiagnostics: v.diagnostics,
                    validation: v.validators,
                    moduleKind,
                };
            }

            const candidateBuf = Buffer.from(String(text), 'utf8');
            const candidateHash = sha1(candidateBuf);
            const operationId = crypto.randomBytes(8).toString('hex');
            let commit;
            try {
                commit = await commitGuarded(rel, candidateBuf, { create: true }, operationId);
            } catch (err) {
                return { ok: false, reason: err.message };
            }
            if (commit.locked) {
                return {
                    ok: false,
                    locked: true,
                    reason: `could not acquire the write lock for ${rel} (${commit.reason})`,
                };
            }
            if (commit.outcome === 'exists') {
                return {
                    ok: false,
                    reason: `${rel} already exists — use edit, or writeWholeFile with expectedHash, to replace it`,
                };
            }
            if (commit.outcome === 'noatomic') {
                return { ok: false, reason: `${rel} could not be published atomically — nothing was touched` };
            }
            if (commit.outcome === 'symlink') return { ok: false, reason: `${rel} is a symlink — refusing` };

            const readback = await readBack(rel);
            const readbackHash = readback ? sha1(readback) : null;
            const outcome = classifyCommit({
                commit,
                candidateHash,
                beforeHash: null,
                readbackHash,
                readAvailable: !!readback,
            });
            await ingestOwnWrite(rel, readback);
            return {
                ok: outcome === 'ok',
                outcome,
                operationId,
                file: rel,
                beforeHash: null,
                candidateHash,
                commitReportedHash: commit.outcome === 'ok' ? commit.hash : null,
                readbackHash,
                validation: v.validators,
                coverage: v.coverage,
                moduleKind,
                ...(outcome !== 'ok'
                    ? { reason: `commit outcome: ${outcome} — read ${rel} again before trying anything further` }
                    : {}),
            };
        },

        // Language-appropriate proof ON THE TARGET that what landed is still
        // valid. Opt-in: needs the facade's `exec` capability, which a host
        // grants per workspace. Edits are already validated locally without it.
        async syntaxCheck(filePath) {
            const f = fileOf(filePath);
            const rel = f ? f.rel : String(filePath).replace(/\\/g, '/');
            if (langOf(rel) !== 'javascript') return { ok: true, out: '(no checker for this language)' };
            if (typeof access.exec !== 'function') {
                return { ok: false, unsupported: true, out: 'this workspace grants no exec capability' };
            }
            return access.exec(`node --check '${rel.replace(/'/g, "'\\''")}'`);
        },

        async stats() {
            await ensureOpen();
            const all = list();
            const byLang = {};
            for (const f of all) byLang[f.lang] = (byLang[f.lang] || 0) + 1;
            return {
                id,
                access: access.kind || null,
                store: store ? store.envName : null,
                lastScanAt,
                files: all.length,
                symbols: all.reduce((a, f) => a + (f.symbols || []).length, 0),
                // Three honest categories instead of one misleading
                // "unparsed": a markdown file is not a failure, it is a file
                // with no symbols to extract.
                withSymbols: all.filter((f) => (f.symbols || []).length).length,
                searchable: all.filter((f) => f.indexed !== false).length,
                opaque: all.filter((f) => f.indexed === false).length,
                failedParse: all.filter((f) => f.lang === 'javascript' && !f.parsed).length,
                cachedText: all.filter((f) => typeof f.content === 'string').length,
                pendingWrites: pending.size,
                byLang,
            };
        },

        // Persist what is pending and detach from the store. The store and
        // the okdb instance stay open — they belong to the caller.
        async close() {
            await flush();
            if (store) store.setTextSource(null);
        },
    };
    return api;
}

module.exports = { openWorkspace, compileGitignore, carveRegions, splitPiece, classifyCommit, parseRange };
