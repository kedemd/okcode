'use strict';
// The non-screenshot correctness gate: tag balance inside template literals.
//
// Two obligations, and the second is the one that decides whether this ships:
//   1. it must CATCH the real corruption (an off-by-one range replace that
//      duplicated a <div> and dropped an </if> — both JS validators passed it,
//      the file published, four screenshot passes called the task done);
//   2. it must not false-positive on valid templates. A checker that does
//      refuses valid edits, which is worse than the false-done it prevents.

const test = require('node:test');
const assert = require('node:assert/strict');
const tb = require('../src/analysis/template-balance');
const validate = require('../src/analysis/validate');

const codes = (text) => tb.check(text).map((d) => `${d.code}:${d.name}`);

// An excerpt of the component the corruption happened in (topics-rail.ok.js),
// preceded by filler so the template sits deep in the file like the original.
const FILLER = Array.from({ length: 30 }, (_, i) => `// filler line ${i}`).join('\n');
const RAIL = [
    FILLER,
    'export default {',
    "    tag: 'topics-rail',",
    '    template: `',
    '      <div class="rail" :class="{ narrow: effectiveCollapsed() }">',
    '        <div class="rail-head">',
    '          <if :="!effectiveCollapsed()">',
    '            <span class="rh-label sect" @click:stop="toggleSection(\'topics\')" title="show or hide the topic list">',
    '              <span class="caret" :class="{ shut: !isOpen(\'topics\') }">▾</span> topics',
    '            </span>',
    '          </if>',
    '          <!-- The collapse toggle only exists in the desktop column: <span> -->',
    '          <if :="!mobile">',
    '            <button type="button" class="rh-toggle" @click:stop="toggle()"',
    "                    :title=\"collapsed ? 'expand the topics rail' : 'collapse to dots'\"",
    "                    :aria-expanded=\"collapsed ? 'false' : 'true'\">",
    "              <ok-icon :name=\"collapsed ? 'chevron-right' : 'chevron-left'\"></ok-icon>",
    '            </button>',
    '          </if>',
    '        </div>',
    '        <if :="isOpen(\'topics\') || effectiveCollapsed()">',
    '          <div class="rail-item topic" :class="{ on: topicFilter === \'all\' }" @click="act(\'all\')">',
    '            <span class="dot all"></span>',
    '          </div>',
    '        </if>',
    '      </div>',
    '    `,',
    '};',
    '',
].join('\n');

test('the real corruption (2026-08-17) is caught, and only template-balance catches it', () => {
    const anchor = '        <div class="rail-head">\n          <if :="!effectiveCollapsed()">';
    assert.ok(RAIL.includes(anchor));
    assert.equal(tb.check(RAIL).length, 0, 'the untouched component is clean');
    // Exactly what the edit did: duplicate the opening div, drop the </if>.
    const broken = RAIL.replace(
        anchor,
        '        <div class="rail-head">\n        <div class="rail-head">\n          <if :="!effectiveCollapsed()">',
    ).replace('            </span>\n          </if>\n', '            </span>\n');
    const found = tb.check(broken);
    assert.ok(found.length >= 1, JSON.stringify(codes(broken)));
    assert.ok(
        found.some((d) => d.code === 'UnclosedTag' && d.name === 'div'),
        JSON.stringify(codes(broken)),
    );
    assert.ok(
        found.every((d) => d.line > 30 && d.start > 0),
        'with a line number inside the template: ' + found.map((d) => d.line),
    );

    const js = validate.validateSource({ rel: 'topics-rail.ok.js', text: broken, moduleKind: 'module' });
    assert.ok(
        js.validators.filter((v) => v.validator.startsWith('javascript')).every((v) => v.status === 'passed'),
        'both JS validators still PASS it (this is why the gate exists)',
    );
    assert.ok(js.validators.some((v) => v.validator === 'template-balance' && v.status === 'failed'));
    assert.ok(js.diagnostics.some((d) => d.source === 'template-balance' && d.severity === 'error'));
    assert.ok(tb.templateSpans(RAIL).length > 0, 'the scan is not vacuous');
});

test('what must NOT be read as markup', () => {
    const clean = [
        'const t = `<div :class="a < b">x</div>`;',
        'const t = `<div :class="a > b"><span>x</span></div>`;',
        'const t = `<div>{{ a < b }}</div>`;',
        'const t = `<div><!-- <span> --></div>`;',
        'const t = `<!doctype html><html><body>x</body></html>`;',
        'const t = `<div><input type="text" name="a"><img src="x"></div>`;',
        'const t = `<div><br/><x-thing/></div>`;',
        'const t = `<if :="a"><div>y</div><else><div>n</div></else></if>`;',
        'const a = 1; function f() { return a < 2; }',
        'const q = `select * from t where a < 2`;',
        'const t = `${cond ? `<div>a</div>` : `<div>b</div>`}`;',
        "const t = `<div>it's fine</div><span>x</span>`;",
        'const t = `<a href="http://x">y</a>`;',
        'const t = `<div>${items.map((i) => `<span>${i}</span>`).join("")}</div>`;',
        'const t = `<div>${x}</div>`;',
        'const t = `<div :style="${ {a: "}"}.a }">x</div>`;',
    ];
    for (const src of clean) assert.deepEqual(codes(src), [], src);
});

test('what MUST be caught', () => {
    assert.ok(codes('const t = `<div><span>x</span>`;').includes('UnclosedTag:div'));
    assert.ok(codes('const t = `<div>x</div></span>`;').includes('UnexpectedClosingTag:span'));
    assert.ok(codes('const t = `<div><span>x</div></span>`;').some((c) => c.startsWith('UnexpectedClosingTag')));
    assert.ok(codes('const t = `<div><if :="a"><span>x</span></div>`;').length > 0, 'a dropped </if>');
    assert.ok(codes('const t = `<div class="a"><div class="a"><span>x</span></div>`;').includes('UnclosedTag:div'));
    // The documented limit, asserted so it stays a choice rather than a
    // surprise: markup inside an interpolation is opaque.
    assert.deepEqual(codes('const t = `${x ? `<div><span>y</span>` : ``}`;'), []);
    const d = tb.check('const pre = 1;\nconst t = `<div>\n  <span>x</span>\n`;\n')[0];
    assert.equal(d.code, 'UnclosedTag');
    assert.equal(d.line, 2, 'the diagnostic points at the opener');
});

test('a baseline that is already unbalanced stays editable (compareDiagnostics)', () => {
    const b = validate.validateSource({
        rel: 'a.js',
        text: 'const t = `<div><span>x</span>`;\nconst other = 1;\n',
        moduleKind: 'module',
    });
    const c = validate.validateSource({
        rel: 'a.js',
        text: 'const t = `<div><span>y</span>`;\nconst other = 1;\n',
        moduleKind: 'module',
    });
    assert.equal(b.diagnostics.length, 1);
    assert.equal(c.diagnostics.length, 1);
    const cmp = validate.compareDiagnostics(b.diagnostics, c.diagnostics, {
        mapOffset: (o) => o,
        targets: [{ name: 'span', baselineStart: 16, baselineEnd: 31, candidateStart: 16, candidateEnd: 31 }],
    });
    assert.equal(cmp.newDiagnostics.length, 0);
    assert.equal(cmp.remaining.length, 1);
});
