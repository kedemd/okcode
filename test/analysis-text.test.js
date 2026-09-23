'use strict';
// Pure text primitives — no filesystem, no fixture. These are what the
// transactional editor composes edits with, so a bug here would be silent in
// every other test: fixture files are usually LF-only, and split('\n')/
// join('\n') happen to agree with offsets on those. CRLF is where they would
// have diverged.

const test = require('node:test');
const assert = require('node:assert/strict');
const T = require('../src/analysis/text');

test('decode: lossless only when the UTF-8 round trip is byte-identical', () => {
    assert.ok(T.decode(Buffer.from('const x = "—…é";', 'utf8')).lossless);
    assert.ok(!T.decode(Buffer.from([0x41, 0x80, 0x42])).lossless, 'a lone continuation byte decodes lossy');
});

test('dominantEol: majority vote, LF when there is nothing to vote on', () => {
    assert.equal(T.dominantEol('a\r\nb\r\nc\n'), '\r\n');
    assert.equal(T.dominantEol('a\nb\nc\n'), '\n');
    assert.equal(T.dominantEol('a'), '\n');
});

test('normaliseEol converts every ending and is idempotent', () => {
    assert.equal(T.normaliseEol('a\nb\nc', '\r\n'), 'a\r\nb\r\nc');
    assert.equal(T.normaliseEol('a\r\nb\r\nc', '\n'), 'a\nb\nc');
    assert.equal(T.normaliseEol(T.normaliseEol('a\nb', '\r\n'), '\r\n'), 'a\r\nb');
});

test("line ranges agree with split('\\n').join('\\n') on LF, and do not corrupt CRLF", () => {
    const lf = 'one\ntwo\nthree\nfour\n';
    const r1 = T.lineRangeToOffsets(lf, 1, 2);
    assert.equal(lf.slice(r1.start, r1.end), lf.split('\n').slice(0, 2).join('\n'));
    const rLast = T.lineRangeToOffsets(lf, 3, 4);
    assert.equal(
        lf.slice(rLast.start, rLast.end),
        'three\nfour',
        'a range ending on the last real line excludes its trailing newline',
    );

    const crlf = 'one\r\ntwo\r\nthree\r\n';
    const r2 = T.lineRangeToOffsets(crlf, 1, 2);
    assert.equal(crlf.slice(r2.start, r2.end), 'one\r\ntwo', 'internal CRLF kept, only the trailing one stripped');
});

test('line/column <-> offset round trip', () => {
    const multi = 'first\nsecond\nthird';
    const at = multi.indexOf('third');
    const lc = T.lineColAt(multi, at);
    assert.deepEqual(lc, { line: 3, column: 1 });
    assert.equal(T.offsetAtLineCol(multi, lc.line, lc.column), at);
    assert.deepEqual(T.lineOffsets('a\nb\n'), [0, 2, 4]);
});

test('bounded per-edit hunks, built from known offsets, not a diff algorithm', () => {
    const before = 'line1\nline2\nline3\nline4\nline5\nline6\nline7\n';
    const target = T.lineRangeToOffsets(before, 4, 4); // "line4"
    const body = 'CHANGED';
    const after = before.slice(0, target.start) + body + before.slice(target.end);
    const hunks = T.hunksFor(before, after, [{ start: target.start, end: target.end, body, target: 'thing' }]);
    assert.equal(hunks.length, 1);
    assert.equal(hunks[0].target, 'thing');
    assert.equal(hunks[0].before.lineStart, 1, 'context is bounded to 3 lines each side');
    assert.equal(hunks[0].before.lineEnd, 7);
    assert.ok(hunks[0].text.includes('- line4') && hunks[0].text.includes('+ CHANGED'), hunks[0].text);
});
