'use strict';
// Chunking is testable without a model, and it is the part that decides what
// the vector index can ever answer: a chunk that starts in the middle of a
// function embeds half an idea. No model, no pipeline, no network.

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const {
    chunkCode,
    headedChunks,
    prepareText,
    markLength,
    pipelineName,
    slug,
    boundariesFor,
    CHUNK_MAX,
    MARK,
    MIN_EMBED_CHARS,
} = require('../src/analysis/chunk');

// A real, comment-dense source file of this package.
const REAL = fs.readFileSync(path.join(__dirname, '..', 'src', 'analysis', 'parse.js'), 'utf8');

describe('chunkCode', () => {
    it('chunks are symbols, not slices of a fixed length', () => {
        const chunks = chunkCode(REAL);
        assert.ok(chunks.length > 3, `${chunks.length} chunks`);
        assert.equal(
            chunks.reduce((a, c) => a + c.text.length, 0),
            REAL.length,
            'every byte is covered exactly once',
        );
        assert.ok(
            chunks.every((c, i) => c.start === (i ? chunks[i - 1].end : 0) && c.end > c.start),
            'offsets are contiguous and in order',
        );
        assert.ok(
            chunks.every((c) => c.text.length <= CHUNK_MAX),
            `max ${Math.max(...chunks.map((c) => c.text.length))}`,
        );
        // The property that matters: a boundary lands where a declaration
        // starts, not mid-statement.
        const startsClean = chunks.filter((c) =>
            /^\s*(?:\/\/|\/\*|'use strict'|const |let |var |function |class |async |module\.|[A-Za-z_$][\w$]*\s*[({:])/.test(
                c.text.trimStart().split('\n')[0] || '',
            ),
        );
        assert.ok(startsClean.length / chunks.length > 0.7, `${startsClean.length}/${chunks.length}`);
    });

    it('a file the parser cannot read still chunks; empty input yields nothing', () => {
        const md = chunkCode('# Title\n\nA paragraph about things.\n\nAnother paragraph.\n');
        assert.ok(md.length >= 1);
        assert.equal(chunkCode('').length, 0);
    });

    it('an oversized indivisible segment is cut at a line break, never past the cap', () => {
        const big = Array.from({ length: 400 }, (_, i) => `  "k${i}": "${'x'.repeat(20)}"`).join('\n');
        const text = `const giant = \`\n${big}\n\`;\n`;
        const chunks = chunkCode(text, 'giant.js');
        assert.ok(chunks.length > 1);
        assert.ok(chunks.every((c) => c.text.length <= CHUNK_MAX));
        assert.ok(
            chunks.slice(1).every((c) => text[c.start - 1] === '\n'),
            'each cut lands after a newline',
        );
        assert.equal(chunks.map((c) => c.text).join(''), text);
    });

    it('the language decides the seams', () => {
        const md = '# A\n\ntext\n## B\nmore\n';
        assert.ok(boundariesFor('x.md', md).includes(md.indexOf('## B')));
        const json = '{\n  "a": 1,\n  "b": { "c": 2 }\n}\n';
        assert.ok(boundariesFor('x.json', json).length > 3);
        const html = '<div>\n  <p>x</p>\n<script>\nfunction f() {}\nfunction g() {}\n</script>\n</div>\n';
        const hb = boundariesFor('x.html', html);
        assert.ok(hb.includes(html.indexOf('function g')), 'script contents are parsed as JavaScript');
        assert.equal(boundariesFor('x.rs', 'fn main() {}'), null);
    });
});

describe('headedChunks + prepareText', () => {
    it('stamps the path, heads each chunk with its owning symbol, keeps offsets into the prepared text', () => {
        const src = [
            "'use strict';",
            '',
            '// Wait longer each time the server says no.',
            'function backoff(attempt) {',
            '    return Math.min(30000, 2 ** attempt * 100);',
            '}',
            '',
            'module.exports = { backoff };',
            '',
        ].join('\n');
        const prepared = prepareText('lib/retry.js', src);
        assert.ok(prepared.startsWith(`${MARK}lib/retry.js\n`));
        const chunks = headedChunks(prepared);
        assert.ok(chunks.length >= 1);
        const c = chunks.find((x) => x.text.includes('function backoff'));
        assert.ok(c.text.startsWith('// lib/retry.js'), c.text);
        // Offsets index the PREPARED text; minus the marker they index the file.
        const off = markLength('lib/retry.js');
        assert.equal(prepared.slice(c.start, c.end), src.slice(c.start - off, c.end - off));
        const owned = chunks.find((x) => /› backoff/.test(x.text));
        if (owned) assert.match(owned.text, /Wait longer each time/);
    });

    it('prepareText refuses what should not be embedded', () => {
        assert.equal(prepareText('a.js', 'short'), '');
        assert.equal(prepareText('a.js', 'x'.repeat(MIN_EMBED_CHARS), { indexed: false }), '');
        assert.equal(prepareText('a.js', null), '');
        assert.ok(prepareText('a.js', 'x'.repeat(MIN_EMBED_CHARS)).length > MIN_EMBED_CHARS);
    });

    it('text without the marker chunks plainly', () => {
        const chunks = headedChunks('function a() {}\n\nfunction b() {}\n');
        assert.ok(chunks.length >= 1 && !chunks[0].text.startsWith('// '));
    });
});

describe('pipelineName', () => {
    it('encodes model and dims, so two configurations can never share a store', () => {
        assert.equal(pipelineName('qwen3-embedding:0.6b', 1024), 'code_qwen3_embedding_0_6b_1024');
        assert.notEqual(pipelineName('m', 8), pipelineName('m', 16));
        assert.notEqual(pipelineName('a', 8), pipelineName('b', 8));
        assert.throws(() => pipelineName('m', null));
        assert.throws(() => pipelineName(null, 8));
        assert.equal(slug('--A.b--'), 'a_b');
    });
});
