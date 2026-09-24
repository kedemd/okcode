'use strict';
// The generic extension registry (extensions.js) and its one registrant, the
// okjs extension (ext-okjs.js). Nothing here re-implements OKJS syntax — it
// asserts the CONTRACT: a `.ok.js` file gets structural symbols when the
// optional @kedem/okjs peer is installed, a plain-JS fallback with a visible
// "unavailable" reason when it is not, a throwing analyser cannot crash extraction, and `.ok.html` gets
// validated. Passes whether or not @kedem/okjs resolves; the tooling-present
// cases are skipped when it does not (run with NODE_PATH pointing at an
// install to exercise them).

const test = require('node:test');
const assert = require('node:assert/strict');
const extensions = require('../src/analysis/extensions');
const okjs = require('../src/analysis/ext-okjs');
const { extract } = require('../src/analysis/parse');
const { validateSource } = require('../src/analysis/validate');

let hasTooling = true;
try {
    require.resolve('@kedem/okjs/tooling');
} catch {
    hasTooling = false;
}

const MATCH = /\.(ok\.js|ok\.mjs|ok\.html)$/i;
const REAL_VERSION = okjs.toolingVersion();
const registerReal = () =>
    extensions.registerExtension({
        id: 'okjs',
        match: (p) => MATCH.test(p),
        analyze: okjs.analyze,
        version: REAL_VERSION,
    });

const CARD = [
    'export default {',
    "    tag: 'x-card',",
    '    template: `<div class="card"><h1>${title}</h1></div>`,',
    '    style: `.card { padding: 8px; }`,',
    '    mount() {',
    '        return true;',
    '    },',
    '};',
    '',
].join('\n');

test('the registry itself, no OKJS knowledge involved', () => {
    extensions.registerExtension({
        id: 'test-ext',
        match: (p) => p.endsWith('.zz'),
        analyze: () => ({}),
        version: 'v1',
    });
    assert.equal(extensions.extensionFor('thing.zz').id, 'test-ext');
    assert.equal(extensions.extensionFor('thing.js'), null);
    extensions.registerExtension({
        id: 'test-ext',
        match: (p) => p.endsWith('.zz'),
        analyze: () => ({}),
        version: 'v2',
    });
    const mine = extensions.listExtensions().filter((e) => e.id === 'test-ext');
    assert.deepEqual(mine, [{ id: 'test-ext', version: 'v2' }], 're-registering the same id replaces it');
    extensions.registerExtension({
        id: 'broken-match',
        match: () => {
            throw new Error('x');
        },
        analyze: () => ({}),
    });
    assert.equal(extensions.extensionFor('thing.js'), null, 'a throwing match() disqualifies only itself');
    assert.throws(() => extensions.registerExtension({ match: () => true, analyze: () => ({}) }), /needs an id/);
    assert.ok(
        extensions.listExtensions().some((e) => e.id === 'okjs'),
        'parse.js registers okjs on require',
    );
});

test('the analyser version is stamped on every .ok.js extraction', () => {
    const r = extract('card.ok.js', CARD);
    assert.equal(r.analyzerVersion, REAL_VERSION);
    assert.equal(r.indexed, true);
    assert.match(REAL_VERSION, /^[^:]+:[^:]+$/);
    assert.equal(okjs.baseURLFor('\\a\\b.ok.js'), 'file:///a/b.ok.js');
});

// @kedem/okjs is a dev dependency here, so absence is SIMULATED: the real
// analyser, built with a tooling loader that finds nothing.
test('without @kedem/okjs: plain-JS fallback with a visible reason, never unparsed', () => {
    const src = [
        "import { html } from './lib.js';",
        "export * from './shared.js';",
        '// Formats a title.',
        'export const fmt = (t) => t.toUpperCase();',
        CARD,
    ].join('\n');
    extensions.registerExtension({
        id: 'okjs',
        match: (p) => MATCH.test(p),
        analyze: okjs.makeAnalyze(() => null),
        version: '0:0',
    });
    try {
        const r = extract('card.ok.js', src);
        assert.equal(r.parsed, true, r.reason);
        assert.equal(r.indexed, true);
        assert.equal(r.lang, 'javascript');
        assert.equal(r.analyzerVersion, '0:0');
        assert.match(r.reason, /okjs tooling unavailable.*okjs-specific analysis skipped/);
        assert.equal(r.okAnalysis, undefined, 'no okjs envelope is invented');
        const fmt = r.symbols.find((x) => x.name === 'fmt');
        assert.equal(fmt.kind, 'function');
        assert.equal(fmt.signature, 'fmt(t)');
        assert.equal(fmt.exported, true);
        assert.equal(src.slice(fmt.start, fmt.end), 'export const fmt = (t) => t.toUpperCase();');
        assert.deepEqual(
            r.imports.map((i) => i.from),
            ['./lib.js', './shared.js'],
        );
        assert.deepEqual(r.exports, ['fmt', 'default']);
        // Source acorn cannot parse is still unparsed, with both reasons.
        const bad = extract('bad.ok.js', 'export default {{{');
        assert.equal(bad.parsed, false);
        assert.match(bad.reason, /okjs tooling unavailable/);
        // .ok.html has no generic extractor: unparsed, but still says why.
        const h = extract('x.ok.html', '<script type="module">\n</script>\n');
        assert.equal(h.parsed, false);
        assert.match(h.reason, /okjs tooling unavailable/);
        const v = validateSource({ rel: 'x.ok.html', text: '<script type="module">\n</script>\n' });
        // Nothing was checked without the tooling: "skipped", never "passed".
        assert.ok(v.validators.some((x) => x.validator === 'okjs-analyze' && x.status === 'skipped'));
    } finally {
        registerReal();
    }
});

test(
    'without @kedem/okjs (genuinely absent): the real loader reports 0:0',
    { skip: hasTooling && 'okjs tooling installed' },
    () => {
        assert.equal(REAL_VERSION, '0:0');
        const r = extract('card.ok.js', CARD);
        assert.equal(r.parsed, true, r.reason);
        assert.match(r.reason, /okjs tooling unavailable/);
    },
);

test(
    'with @kedem/okjs: the component is a symbol addressed by its tag',
    { skip: !hasTooling && 'okjs tooling not installed' },
    () => {
        const r = extract('card.ok.js', CARD);
        assert.equal(r.parsed, true, r.reason);
        const tag = r.symbols.find((s) => s.name === 'x-card');
        assert.equal(tag.kind, 'component');
        assert.ok(CARD.slice(tag.start, tag.end).includes('mount()'));
        assert.ok(r.okAnalysis, 'the raw analysis rides along');
        assert.ok(Array.isArray(r.regions) && r.regions.some((x) => x.kind === 'template'));
    },
);

test(
    '.ok.html gets structural validation; plain .html gets none',
    { skip: !hasTooling && 'okjs tooling not installed' },
    () => {
        const brokenHtml = '<script type="module">\n</script>\n'; // no default component export
        const before = validateSource({ rel: 'x.html', text: brokenHtml });
        assert.deepEqual(
            before.validators.map((v) => v.validator),
            ['none'],
        );
        const v = validateSource({ rel: 'x.ok.html', text: brokenHtml });
        assert.ok(v.validators.some((x) => x.validator === 'okjs-analyze' && x.status === 'failed'));
        assert.ok(
            v.diagnostics.some((d) => d.code === 'OKJS_COMPONENT_DEFINITION_MISSING'),
            JSON.stringify(v.diagnostics),
        );
    },
);

test('a throwing analyser marks the file unparsed, not invisible, and never breaks validation', () => {
    extensions.registerExtension({
        id: 'okjs',
        match: (p) => MATCH.test(p),
        version: 'throws',
        analyze: () => {
            throw new Error('boom — simulated analyser crash');
        },
    });
    try {
        const r = extract('card.ok.js', CARD);
        assert.equal(r.parsed, false);
        assert.equal(r.indexed, true);
        assert.equal(r.analyzerVersion, 'throws');
        assert.match(r.reason, /boom/);
        const v = validateSource({ rel: 'card.ok.js', text: CARD, moduleKind: 'module' });
        assert.ok(v.validators.some((x) => x.validator === 'okjs-analyze' && x.status === 'skipped'));
        assert.ok(v.validators.some((x) => x.validator === 'javascript-acorn' && x.status === 'passed'));
    } finally {
        registerReal();
    }
});

// A symbol name is an indexed field, and okdb (≥ 2.3.2) aborts a write whose
// indexed value is not a scalar — so whatever the analyser reports as a
// component's name leaves this adapter as a string, and a nameless one is
// dropped rather than indexed as "".
test('component names from the analyser are always strings', () => {
    const range = { start: 0, end: 10, loc: { start: { line: 1 }, end: { line: 2 } } };
    const analyze = okjs.makeAnalyze(() => ({
        analyzeOKSource: () => ({
            symbols: [
                { kind: 'component', name: 'x-card', range },
                { kind: 'component', name: /x-rx/, range },
                { kind: 'component', name: 42, range },
                { kind: 'component', name: null, range },
            ],
            dependencies: [],
        }),
    }));
    const r = analyze({ path: 'c.ok.js', source: '' });
    assert.equal(r.parsed, true);
    assert.deepEqual(
        r.symbols.map((s) => [s.name, s.path]),
        [
            ['x-card', 'x-card'],
            ['/x-rx/', '/x-rx/'],
            ['42', '42'],
        ],
    );
});
