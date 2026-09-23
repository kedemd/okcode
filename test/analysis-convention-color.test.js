'use strict';
// THE PALETTE GATE, driven by the edit that got past everything else.
//
// Measured: asked to make a label green, the agent wrote
// `style="color:#22c55e"`. The project defines `--green: #3fb950` and uses it
// throughout; #22c55e is Tailwind's green-500 and appears nowhere in the
// repository. A MECHANICALLY TESTABLE convention belongs in the write gate, so
// the rule reads the palette out of the project every time, and stays blunt
// without becoming noisy — because the gate judges candidate against
// baseline and lets through anything that was already there.

const test = require('node:test');
const assert = require('node:assert/strict');
const { paletteFrom, colorLiterals, rgbOf, nearest } = require('../src/analysis/convention-color');
const { validateSource, compareDiagnostics } = require('../src/analysis/validate');

const SHELL = '  --accent: #79c0ff; --green: #3fb950; --amber: #d29922; --red: #f85149; --dim: #6e7681;';
const PALETTE = paletteFrom([SHELL]);

test('the palette is read out of the project, not remembered', () => {
    assert.deepEqual(
        PALETTE.map((p) => p.name),
        ['accent', 'green', 'amber', 'red', 'dim'],
    );
    assert.deepEqual(PALETTE.find((p) => p.name === 'green').rgb, [63, 185, 80]);
    assert.equal(paletteFrom(['body { color: #fff; }']).length, 0);
    assert.deepEqual(rgbOf('#0f0'), [0, 255, 0]);
    assert.deepEqual(rgbOf('rgb(63, 185, 80)'), [63, 185, 80]);
});

test('the edit that got through everything else is caught, with an actionable message', () => {
    const line = `<span class="rh-label sect" style="color:#22c55e" title="x">`;
    const found = colorLiterals(line, PALETTE);
    assert.equal(found.length, 1);
    assert.match(found[0].message, /--green/);
    assert.match(found[0].message, /#3fb950/);
    assert.equal(line.slice(found[0].start, found[0].end), '#22c55e', 'points at the literal, not the line');
    assert.equal(found[0].severity, 'warning');
    assert.equal(found[0].code, 'CONVENTION_COLOR_LITERAL');
});

test('what it must NOT flag', () => {
    const fine = [
        'color: var(--green);',
        'background: transparent;',
        'color: inherit;',
        'border-color: currentColor;',
        'box-shadow: 0 8px 24px rgba(0,0,0,.5);',
        'background: linear-gradient(90deg, #111 0%, #222 100%);',
    ].join('\n');
    assert.deepEqual(colorLiterals(fine, PALETTE), []);
    const odd = colorLiterals('color: #8b00ff;', PALETTE);
    assert.equal(odd.length, 1, 'an unrelated colour is still flagged');
    assert.doesNotMatch(odd[0].message, /defines --\w+ \(.*\) for this/, 'without pretending it meant one of ours');
    assert.equal(nearest([139, 0, 255], PALETTE), null);
    assert.deepEqual(colorLiterals('color: #fff;', []), [], 'no palette, no rule');
});

test('blunt is safe ONLY because the gate compares against the baseline', () => {
    const rel = 'public/components/thing.ok.js';
    const before = 'export default { tag: "x-t", template: `<i></i>`, style: `.a{color:#111} .b{color:#222}` };\n';
    const after =
        'export default { tag: "x-t", template: `<i></i>`, style: `.a{color:#111} .b{color:#222} .c{color:#333}` };\n';
    const bV = validateSource({ rel, text: before, moduleKind: 'module', palette: PALETTE });
    const aV = validateSource({ rel, text: after, moduleKind: 'module', palette: PALETTE });
    assert.equal(bV.diagnostics.filter((d) => d.code === 'CONVENTION_COLOR_LITERAL').length, 2);
    assert.ok(bV.validators.some((v) => v.validator === 'convention-color' && v.status === 'failed'));

    const unchanged = compareDiagnostics(bV.diagnostics, bV.diagnostics, { mapOffset: (o) => o });
    assert.equal(unchanged.newDiagnostics.length, 0);

    const cmp = compareDiagnostics(bV.diagnostics, aV.diagnostics, { mapOffset: (o) => o });
    const fresh = cmp.newDiagnostics.filter((d) => d.code === 'CONVENTION_COLOR_LITERAL');
    assert.equal(fresh.length, 1);
    assert.match(fresh[0].message, /#333/);
    assert.ok(Number.isInteger(fresh[0].line) && Number.isInteger(fresh[0].column));
});

test('with no palette supplied the validator is inert', () => {
    const v = validateSource({
        rel: 'a.ok.js',
        text: 'export default { tag: "x-a", style: `.a{color:#22c55e}` };\n',
        moduleKind: 'module',
    });
    assert.ok(!v.validators.some((x) => x.validator === 'convention-color'));
    assert.ok(!v.diagnostics.some((d) => d.code === 'CONVENTION_COLOR_LITERAL'));
});
