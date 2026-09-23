'use strict';
// Pure text primitives for the transactional editor. No filesystem, no shell,
// no child process — everything here is a function of the bytes it is given,
// which is what makes it testable without a fixture and safe to call before
// any target is touched.

// Decode bytes as UTF-8, and say whether the round trip is LOSSLESS —
// `Buffer.from(text, 'utf8').equals(buf)`. A lossy decode (invalid UTF-8, a
// stray byte-order mark misread, a different encoding entirely) must never be
// silently "fixed" by re-encoding it: that is how four of this repo's own
// files lost an em-dash to a console codepage. The write path refuses rather
// than composing an edit onto a decode it cannot prove round-trips.
function decode(buf) {
    const text = buf.toString('utf8');
    const lossless = Buffer.from(text, 'utf8').equals(buf);
    return { text, lossless };
}

// Which line ending a file actually uses, by majority vote — CRLF counts
// separately from a bare LF that is not part of one. A file with no newline
// at all defaults to LF, since there is nothing to normalise against.
function dominantEol(src) {
    const crlf = (src.match(/\r\n/g) || []).length;
    const totalNl = (src.match(/\n/g) || []).length;
    const lf = totalNl - crlf;
    return crlf > lf ? '\r\n' : '\n';
}

// Model-authored text arrives with whatever line ending the model happened to
// use, almost always bare LF. Composing that into a CRLF file mid-document
// would leave one file with two conventions from a single edit, so inserted
// text is normalised to the file's own dominant ending before it is spliced
// in — never the reverse: bytes OUTSIDE an edit are sliced verbatim and never
// touched by this.
function normaliseEol(text, eol) {
    const unified = text.replace(/\r\n/g, '\n').replace(/\r/g, '\n');
    return eol === '\r\n' ? unified.replace(/\n/g, '\r\n') : unified;
}

// The character offset each 1-indexed line starts at, computed once so line
// <-> offset conversion is a lookup rather than a re-scan per call. Matches
// `String.split('\n').length` exactly: a trailing newline yields one more
// (empty) "line" at the end, same as split does.
function lineOffsets(src) {
    const offsets = [0];
    for (let i = 0; i < src.length; i++) {
        if (src[i] === '\n') offsets.push(i + 1);
    }
    return offsets;
}

// The offset SPAN of a 1-indexed inclusive line range [from, to], chosen to
// be byte-identical to the old `lines.slice(from-1, to).join('\n')` on an
// LF-only file — the shape every existing range test was written against —
// while doing the right thing on CRLF, which split('\n')/join('\n') would
// silently corrupt (join always re-inserts a bare LF). The span excludes the
// line terminator that ends line `to` itself; it does not exclude terminators
// BETWEEN from and to, which is what keeps multi-line ranges intact.
function lineRangeToOffsets(src, from, to) {
    const offsets = lineOffsets(src);
    const total = offsets.length;
    const f = Math.max(1, Math.min(from, total));
    const t = Math.max(f, Math.min(to, total));
    const start = offsets[f - 1];
    let end = t < total ? offsets[t] : src.length;
    if (t < total) {
        if (src[end - 1] === '\n') end -= 1;
        if (src[end - 1] === '\r') end -= 1;
    }
    return { start, end };
}

// 1-indexed line/column for a character offset. O(offset), which is fine —
// this runs a handful of times per edit (once per diagnostic), never per
// character of a file.
function lineColAt(src, offset) {
    let line = 1,
        col = 1;
    const stop = Math.max(0, Math.min(offset, src.length));
    for (let i = 0; i < stop; i++) {
        if (src[i] === '\n') {
            line++;
            col = 1;
        } else col++;
    }
    return { line, column: col };
}

// The inverse of lineColAt — used for a validator (node --check) that only
// reports a line and an approximate column, so its diagnostic can still carry
// an offset in the shared shape.
function offsetAtLineCol(src, line, column) {
    let idx = 0,
        ln = 1;
    while (ln < line && idx < src.length) {
        if (src[idx] === '\n') ln++;
        idx++;
    }
    return Math.min(src.length, idx + Math.max(0, (column || 1) - 1));
}

function lineStartOf(src, offset) {
    let i = Math.max(0, Math.min(offset, src.length));
    while (i > 0 && src[i - 1] !== '\n') i--;
    return i;
}

function lineEndOf(src, offset) {
    let i = Math.max(0, Math.min(offset, src.length));
    while (i < src.length && src[i] !== '\n') i++;
    return i;
}

// Widen [start, end) by up to `context` whole lines on each side, clamped to
// the file. Used to build a hunk's surrounding lines without re-deriving line
// boundaries by hand at every call site.
function widenToContext(src, start, end, context) {
    let lo = lineStartOf(src, start);
    for (let n = 0; n < context && lo > 0; n++) lo = lineStartOf(src, lo - 1);
    let hi = lineEndOf(src, Math.max(end - 1, start));
    for (let n = 0; n < context && hi < src.length; n++) hi = lineEndOf(src, hi + 1);
    return {
        start: lo,
        end: hi,
        lineStart: lineColAt(src, lo).line,
        lineEnd: lineColAt(src, Math.max(hi - 1, lo)).line,
    };
}

// Bounded per-edit hunks, built from offsets the caller already knows exactly
// — every edit's before-span and its body — rather than from a general diff
// algorithm. That is possible only because the caller (editBatch) knows
// precisely what changed; it is not a general-purpose text differ. Edits are
// composed in ascending offset order so each one's AFTER position accounts
// for the length change of everything before it in the file — the same
// bookkeeping editBatch itself does when it composes highest-offset-downward
// (the two directions of the same shift, done once here for reporting).
function hunksFor(before, after, edits, { context = 3 } = {}) {
    const sorted = [...edits].sort((a, b) => a.start - b.start);
    let shift = 0;
    return sorted.map((e) => {
        const beforeStart = e.start;
        const beforeEnd = e.end;
        const afterStart = beforeStart + shift;
        const afterEnd = afterStart + e.body.length;
        shift += e.body.length - (beforeEnd - beforeStart);

        const b = widenToContext(before, beforeStart, beforeEnd, context);
        const a = widenToContext(after, afterStart, afterEnd, context);
        const removed = before
            .slice(b.start, b.end)
            .split('\n')
            .map((l) => `- ${l}`);
        const added = after
            .slice(a.start, a.end)
            .split('\n')
            .map((l) => `+ ${l}`);
        return {
            target: e.target || null,
            before: { start: beforeStart, end: beforeEnd, lineStart: b.lineStart, lineEnd: b.lineEnd },
            after: { start: afterStart, end: afterEnd, lineStart: a.lineStart, lineEnd: a.lineEnd },
            // Not a unified diff — there is no line-alignment/LCS step, only
            // the exact before/after text around a known span. Honest and
            // bounded is the goal; a real diff library is not a dependency
            // this needs.
            text: [
                `@@ ${e.target ? e.target + ' ' : ''}lines ${b.lineStart}-${b.lineEnd} -> ${a.lineStart}-${a.lineEnd} @@`,
                ...removed,
                ...added,
            ].join('\n'),
        };
    });
}

module.exports = {
    decode,
    dominantEol,
    normaliseEol,
    lineOffsets,
    lineRangeToOffsets,
    lineColAt,
    offsetAtLineCol,
    lineStartOf,
    lineEndOf,
    widenToContext,
    hunksFor,
};
