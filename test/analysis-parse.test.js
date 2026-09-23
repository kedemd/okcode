'use strict';
// Symbol extraction on text — the parts of the brain's codeindex suite that
// asserted what the parser finds (symbols, paths into closures, signatures,
// docs, imports/exports, the .mjs fallback), without an index around them.

const test = require('node:test');
const assert = require('node:assert/strict');
const {
    extract,
    langOf,
    extOf,
    isTextual,
    isSecret,
    registerExtractor,
    parseSource,
    TEXT_MAX,
} = require('../src/analysis/parse');

const MATH = [
    "'use strict';",
    '',
    '// Adds two numbers. The doc line above a symbol is indexed with it,',
    '// because prose is how a codebase says what a thing is for.',
    'function add(a, b) {',
    '    return a + b;',
    '}',
    '',
    'const TAX_RATE = 0.17;',
    '',
    'const scale = (v, by) => v * by;',
    '',
    '// A shorthand that calls add twice.',
    'const twice = (v) => add(v, v);',
    '',
    'module.exports = { add, scale, TAX_RATE, twice };',
    '',
].join('\n');

// An .mjs file: proves sourceType fallback, which a regex extractor would
// have silently mis-indexed rather than reported.
const ESM = ["export const GREETING = 'hi';", 'export function greet(name) { return `${GREETING} ${name}`; }', ''].join(
    '\n',
);

// A factory: the shape where all the interesting code hides inside a
// closure. A top-level-only extractor sees one symbol here and offers no way
// to read or edit `plus` without carrying the whole of makeCounter.
const FACTORY = [
    "'use strict';",
    'function makeCounter(start) {',
    '    let n = start;',
    '    // Bumps the counter by a step and reports the new total.',
    '    function bump(by) { n += by; return n; }',
    '    return {',
    '        value() { return n; },',
    '        plus(by) { return bump(by); },',
    '    };',
    '}',
    '',
    'module.exports = { makeCounter };',
    '',
].join('\n');

const byPath = (r) => new Map(r.symbols.map((s) => [s.path, s]));

test('a CommonJS file: symbols, signatures, docs, exports', () => {
    const r = extract('lib/math.js', MATH);
    assert.equal(r.parsed, true);
    assert.equal(r.lang, 'javascript');
    assert.equal(r.lines, MATH.split('\n').length);
    const s = byPath(r);
    const add = s.get('add');
    assert.equal(add.kind, 'function');
    assert.equal(add.signature, 'add(a, b)');
    assert.match(add.doc, /prose is how a codebase/);
    assert.equal(add.lineStart, 5);
    assert.equal(add.lineEnd, 7);
    assert.equal(MATH.slice(add.start, add.end).split('\n')[0], 'function add(a, b) {');
    assert.equal(s.get('TAX_RATE').kind, 'const');
    assert.equal(s.get('scale').signature, 'scale(v, by)');
    assert.match(s.get('twice').doc, /calls add twice/);
    assert.deepEqual(r.exports, ['add', 'scale', 'TAX_RATE', 'twice']);
    for (const n of ['add', 'scale', 'TAX_RATE', 'twice']) assert.equal(s.get(n).exported, true, n);
    assert.equal(s.get('module.exports').kind, 'exports');
});

test('an .mjs file parses via the module fallback', () => {
    const r = extract('lib/esm.mjs', ESM);
    assert.equal(r.parsed, true, r.reason);
    const s = byPath(r);
    assert.equal(s.get('greet').exported, true);
    // Ported as-is from the brain: `export const X` (a VariableDeclaration,
    // which has no `.id`) is not captured as a symbol. Asserted so a change
    // to that is a decision, not an accident.
    assert.ok(!s.has('GREETING'));
});

test('reaching INSIDE a closure: methods on the object a factory returns', () => {
    const r = extract('lib/factory.js', FACTORY);
    const s = byPath(r);
    assert.ok(s.has('makeCounter'));
    const bump = s.get('makeCounter.bump');
    assert.equal(bump.parent, 'makeCounter');
    assert.match(bump.doc, /Bumps the counter/);
    const plus = s.get('makeCounter.plus');
    assert.equal(plus.kind, 'method');
    assert.equal(plus.lineStart, plus.lineEnd, 'a one-line method costs one line');
    // The exact acorn span: no indentation, no trailing comma — those belong
    // to the enclosing object literal.
    assert.equal(FACTORY.slice(plus.start, plus.end), 'plus(by) { return bump(by); }');
    assert.ok(!s.has('makeCounter.n'), 'a local non-function is not a symbol');
    assert.ok(!s.get('makeCounter.plus').exported, 'only top-level symbols are exported');
});

test('imports: require edges, ESM imports and re-exports', () => {
    const cjs = extract('a.js', "const fs = require('fs');\nconst { x } = require('./x');\n");
    assert.deepEqual(cjs.imports, [{ local: 'fs', from: 'fs', line: 1 }]);
    assert.equal(cjs.symbols.length, 0, 'a top-level require is an edge, not a symbol');
    const esm = extract(
        'a.mjs',
        "import a from './a.js';\nexport * from './b.js';\nexport { c as d } from './c.js';\n",
    );
    assert.deepEqual(
        esm.imports.map((i) => [i.from, !!i.reexport]),
        [
            ['./a.js', false],
            // `export * from` (ExportAllDeclaration) is not recorded as an
            // edge — the brain extractor's existing behaviour, kept as-is.
            ['./c.js', true],
        ],
    );
    assert.deepEqual(esm.exports, ['d']);
});

test('signatures drop defaults, not everything after the first =', () => {
    const r = extract('a.js', 'async function f({ kind = null, limit = 20 } = {}, x = 1) {}\n');
    assert.equal(r.symbols[0].signature, 'async f({ kind = null, limit = 20 }, x)');
});

test('a file acorn cannot parse is recorded unparsed with a reason, never half-indexed', () => {
    const r = extract('bad.js', 'function {{{');
    assert.equal(r.parsed, false);
    assert.equal(r.indexed, true);
    assert.ok(r.reason && r.reason.length > 0);
    assert.throws(() => parseSource('function {{{'));
});

test('opaque and non-JS files still get a record', () => {
    const bin = extract('img.png', null);
    assert.equal(bin.indexed, false);
    assert.match(bin.reason, /binary or oversized/);
    const secret = extract('.env', null);
    assert.match(secret.reason, /credentials/);
    const md = extract('README.md', '# hi\n');
    assert.equal(md.lang, 'markdown');
    assert.equal(md.parsed, false);
    assert.equal(md.indexed, true);
    assert.match(md.reason, /no symbol extractor/);
});

test('language, extension, textual and secret classification', () => {
    assert.equal(extOf('a/B.JSX'), '.jsx');
    assert.equal(extOf('Makefile'), '');
    assert.equal(langOf('x.cjs'), 'javascript');
    assert.equal(langOf('x.ts'), 'typescript');
    assert.equal(langOf('x.ok'), 'html');
    assert.equal(langOf('x.bin'), 'other');
    assert.ok(isTextual('a.md', 10));
    assert.ok(!isTextual('a.md', TEXT_MAX + 1), 'oversized text is not carried');
    assert.ok(!isTextual('a.png', 10));
    for (const p of [
        '.env',
        'app/.env.local',
        'x/.npmrc',
        'id_ed25519.pub',
        'config/credentials.json',
        'k.pem',
        'a\\secrets.yml',
    ])
        assert.ok(isSecret(p), p);
    assert.ok(!isTextual('.env', 10), 'a secret is never textual');
    assert.ok(!isSecret('src/env.js'));
});

test('registerExtractor adds a language without touching extract()', () => {
    registerExtractor('yaml', (path, src) => ({
        symbols: [{ name: 'root', kind: 'key', path: 'root', lineStart: 1, lineEnd: 1 }],
        imports: [],
        exports: [],
    }));
    const r = extract('c.yml', 'root: 1\n');
    assert.equal(r.parsed, true);
    assert.equal(r.symbols[0].name, 'root');
    registerExtractor('throws', () => {
        throw new Error('x');
    });
});
