'use strict';
// The candidate validator — acorn plus `node --check`, on TEXT, never a
// path. Every case here runs with no fixture on disk at all: that is the
// whole point, a candidate is judged before anything is touched.

const test = require('node:test');
const assert = require('node:assert/strict');
const { validateSource, validateSourceAsync, compareDiagnostics } = require('../src/analysis/validate');

// Named, not counted: template-balance runs alongside the two JS validators,
// and these assertions are about the JS pair.
const jsOf = (r) => r.validators.filter((v) => v.validator.startsWith('javascript-'));

for (const [label, run] of [
    ['validateSource', validateSource],
    ['validateSourceAsync', validateSourceAsync],
]) {
    test(`${label}: both JavaScript validators run, on text alone`, async () => {
        const clean = await run({ rel: 'a.js', text: 'const x = 1;\n', moduleKind: 'commonjs' });
        assert.equal(jsOf(clean).length, 2);
        assert.ok(
            clean.validators.every((v) => v.status === 'passed'),
            JSON.stringify(clean.validators),
        );
        assert.equal(clean.diagnostics.length, 0);
        assert.equal(clean.coverage, 'fail-fast', 'JS validation is fail-fast, never a complete inventory');

        const broken = await run({ rel: 'a.js', text: 'function {{{ broken', moduleKind: 'commonjs' });
        assert.equal(jsOf(broken).length, 2);
        assert.ok(
            jsOf(broken).every((v) => v.status === 'failed'),
            JSON.stringify(broken.validators),
        );
        assert.ok(broken.diagnostics.some((d) => d.source === 'javascript-acorn'));
        assert.ok(broken.diagnostics.some((d) => d.source === 'javascript-node-check'));
        assert.ok(
            broken.diagnostics.every(
                (d) => Number.isInteger(d.start) && Number.isInteger(d.line) && Number.isInteger(d.column),
            ),
            JSON.stringify(broken.diagnostics),
        );

        const nonJs = await run({ rel: 'a.md', text: '# not code', moduleKind: 'commonjs' });
        assert.equal(nonJs.validators.length, 1, 'a non-JS file records one skipped validator');
        assert.equal(nonJs.validators[0].status, 'skipped');
    });

    test(`${label}: moduleKind governs the verdict, not a guess from a successful parse`, async () => {
        const tla = 'await 1;';
        const asModule = await run({ rel: 'a.js', text: tla, moduleKind: 'module' });
        assert.ok(
            asModule.validators.every((v) => v.status === 'passed'),
            JSON.stringify(asModule.validators),
        );
        const asCommonjs = await run({ rel: 'a.js', text: tla, moduleKind: 'commonjs' });
        assert.ok(
            jsOf(asCommonjs).every((v) => v.status === 'failed'),
            JSON.stringify(asCommonjs.validators),
        );
        const asAmbiguous = await run({ rel: 'a.js', text: tla, moduleKind: 'ambiguous' });
        assert.ok(
            asAmbiguous.validators.every((v) => v.status === 'passed'),
            '"ambiguous" is tried as both goals — never a blanket CommonJS default',
        );
    });
}

test('the sync and async validators answer identically', async () => {
    for (const text of ['const x = 1;\n', 'function {{{', 'const t = `<div><span>x</span>`;\n', 'await 1;']) {
        for (const moduleKind of ['module', 'commonjs', 'ambiguous']) {
            const a = validateSource({ rel: 'a.js', text, moduleKind });
            const b = await validateSourceAsync({ rel: 'a.js', text, moduleKind });
            assert.deepEqual(b, a, `${JSON.stringify(text)} as ${moduleKind}`);
        }
    }
});

test('structural diagnostic comparison, never by count', () => {
    const d = (message, start, extra = {}) => ({
        source: 'javascript-acorn',
        code: 'SyntaxError',
        severity: 'error',
        message,
        start,
        end: start,
        line: 1,
        column: 1,
        ...extra,
    });
    const mapped = compareDiagnostics([d('Unexpected token', 50)], [d('Unexpected token', 62)], {
        mapOffset: (o) => (o === 50 ? 62 : null),
        targets: [],
    });
    assert.equal(mapped.newDiagnostics.length, 0, 'a baseline diagnostic mapped forward is not new');
    assert.equal(mapped.remaining.length, 1);
    assert.equal(mapped.baselineBroken, true);

    const targets = [{ name: 'thing', baselineStart: 40, baselineEnd: 60, candidateStart: 40, candidateEnd: 70 }];
    const inside = compareDiagnostics([d('bad token', 45)], [d('bad token', 66)], { mapOffset: () => null, targets });
    assert.equal(inside.newDiagnostics.length, 0, 'inside the edited region, matched by target identity');
    assert.equal(inside.remaining.length, 1);

    const swapped = compareDiagnostics([d('bad token', 45)], [d('a totally different problem', 66)], {
        mapOffset: () => null,
        targets,
    });
    assert.equal(swapped.newDiagnostics.length, 1, 'swapping one error for another is new');
    assert.equal(swapped.resolved.length, 1);

    const worse = compareDiagnostics([d('bad token', 45, { severity: 'warning' })], [d('bad token', 66)], {
        mapOffset: () => null,
        targets,
    });
    assert.equal(worse.newDiagnostics.length, 1, 'a severity increase is always new');
});
