'use strict';
// The generic tree-sitter extractor (F1) and syntax gate (F2) — one engine,
// data per language. Asserts the CONTRACT the overrides exist for (see each
// grammars/<lang>/tags.local.scm for what upstream's tags.scm misses and
// why), nesting/parent, exported-by-convention, dedupe, and the "runtime
// absent" degrade path parse.js's extract() relies on.

const test = require('node:test');
const assert = require('node:assert/strict');
const treesitter = require('../src/analysis/treesitter');
const { extract, langOf } = require('../src/analysis/parse');
const { validateSource } = require('../src/analysis/validate');

const TS = [
    '// Doc comment for Shape interface.',
    'export interface Shape {',
    '  area(): number;',
    '}',
    '',
    '/** Doc comment for Circle class. */',
    'export class Circle implements Shape {',
    '  radius: number;',
    '',
    '  constructor(radius: number) {',
    '    this.radius = radius;',
    '  }',
    '',
    '  // method doc',
    '  area(): number {',
    '    function helper(x: number): number {',
    '      return x * x;',
    '    }',
    '    return Math.PI * helper(this.radius);',
    '  }',
    '}',
    '',
    'export type Point = { x: number; y: number };',
    '',
    'export const makeCircle = (r: number): Circle => {',
    '  return new Circle(r);',
    '};',
    '',
    'function notExported(a: number, b: number): number {',
    '  return a + b;',
    '}',
    '',
    'export enum Color {',
    '  Red,',
    '  Green,',
    '}',
    '',
].join('\n');

const TS_BROKEN = ['export class Broken {', '  method(a: number {', '    return a;', '  }', '}', ''].join('\n');

const PY = [
    '"""Module docstring."""',
    '',
    'class Animal:',
    '    """Base animal class."""',
    '',
    '    def __init__(self, name):',
    '        self.name = name',
    '',
    '    def speak(self):',
    '        """Return the sound."""',
    '        def inner_helper(n):',
    '            return n.upper()',
    '        return inner_helper(self.name)',
    '',
    '    def _private(self):',
    '        return None',
    '',
    '',
    'def make_animal(name):',
    '    """Factory function."""',
    '    return Animal(name)',
    '',
    'CONST_VALUE = 42',
    '',
].join('\n');

const PY_BROKEN = ['def broken(a, b)', '    return a + b', ''].join('\n');

const GO = [
    'package sample',
    '',
    '// Circle is a doc comment for a struct.',
    'type Circle struct {',
    '\tRadius float64',
    '}',
    '',
    '// Area computes the circle area.',
    'func (c *Circle) Area() float64 {',
    '\treturn 3.14159 * c.Radius * c.Radius',
    '}',
    '',
    '// NewCircle constructs a Circle.',
    'func NewCircle(r float64) *Circle {',
    '\treturn &Circle{Radius: r}',
    '}',
    '',
    'func notExported(a int, b int) int {',
    '\treturn a + b',
    '}',
    '',
].join('\n');

const GO_BROKEN = ['package broken', '', 'func Broken(a int, b int int {', '\treturn a + b', '}', ''].join('\n');

const RUST = [
    '/// Doc comment for Circle struct.',
    'pub struct Circle {',
    '    pub radius: f64,',
    '}',
    '',
    'impl Circle {',
    '    pub fn new(radius: f64) -> Circle {',
    '        Circle { radius }',
    '    }',
    '',
    '    fn helper(x: f64) -> f64 {',
    '        x * x',
    '    }',
    '}',
    '',
    'fn not_exported(a: i32, b: i32) -> i32 {',
    '    a + b',
    '}',
    '',
].join('\n');

const RUST_BROKEN = ['fn broken(a: i32, b: i32) -> i32 {', '    a +', '}', ''].join('\n');

test.before(async () => {
    await Promise.all(['typescript', 'python', 'go', 'rust'].map((id) => treesitter.ensureLanguage(id)));
});

test('langIdForPath maps the F1 extensions, and only those', () => {
    assert.equal(treesitter.langIdForPath('x.ts'), 'typescript');
    assert.equal(treesitter.langIdForPath('x.tsx'), 'typescript');
    assert.equal(treesitter.langIdForPath('x.mts'), 'typescript');
    assert.equal(treesitter.langIdForPath('x.cts'), 'typescript');
    assert.equal(treesitter.langIdForPath('x.py'), 'python');
    assert.equal(treesitter.langIdForPath('x.go'), 'go');
    assert.equal(treesitter.langIdForPath('x.rs'), 'rust');
    assert.equal(treesitter.langIdForPath('x.js'), null);
});

test('TypeScript: class/function/arrow-const/type/enum/method — all of upstream\'s gaps', () => {
    const r = extract('sample.ts', TS);
    assert.equal(langOf('sample.ts'), 'typescript');
    const byPath = new Map(r.symbols.map((s) => [s.path, s]));

    assert.equal(byPath.get('Shape').kind, 'interface');
    assert.equal(byPath.get('Circle').kind, 'class');
    assert.ok(byPath.get('Circle').exported);
    assert.equal(byPath.get('Circle.area').kind, 'method');
    assert.equal(byPath.get('Circle.area').doc, 'method doc');
    // Nested function inside a method, by containment — same rule as acorn's.
    assert.equal(byPath.get('area.helper').parent, 'area');
    assert.equal(byPath.get('Point').kind, 'type');
    assert.ok(byPath.get('Point').exported);
    assert.equal(byPath.get('makeCircle').kind, 'function'); // export const f = (...) => ...
    assert.ok(byPath.get('makeCircle').exported);
    assert.equal(byPath.get('notExported').kind, 'function');
    assert.ok(!byPath.get('notExported').exported);
    assert.equal(byPath.get('Color').kind, 'enum');
    assert.equal(r.analyzerVersion, treesitter.currentVersionFor('sample.ts'));
    assert.ok(Array.isArray(r.references) && r.references.some((x) => x.name === 'Circle'));
});

test('Python: docstring override (class own vs nested method), underscore convention', () => {
    const r = extract('sample.py', PY);
    const byPath = new Map(r.symbols.map((s) => [s.path, s]));

    assert.equal(byPath.get('Animal').doc, 'Base animal class.');
    assert.equal(byPath.get('Animal.speak').doc, 'Return the sound.');
    // The class's own docstring must not leak onto a nested method's doc,
    // and vice versa — each is owned by its SMALLEST enclosing definition.
    assert.notEqual(byPath.get('Animal').doc, byPath.get('Animal.speak').doc);
    assert.equal(byPath.get('speak.inner_helper').doc, '');
    assert.ok(byPath.get('make_animal').exported);
    assert.equal(byPath.get('make_animal').doc, 'Factory function.');
    assert.ok(byPath.get('CONST_VALUE').exported);
    assert.ok(!byPath.get('Animal._private').exported, 'leading underscore is not exported');
    assert.ok(!byPath.get('Animal.speak').exported, 'a method is not a MODULE export, regardless of its name');
});

test('Go: method path is Receiver.Method, receiver captured generically via @parent', () => {
    const r = extract('sample.go', GO);
    const byPath = new Map(r.symbols.map((s) => [s.path, s]));

    assert.equal(byPath.get('Circle.Area').kind, 'method');
    assert.equal(byPath.get('Circle.Area').parent, 'Circle');
    assert.equal(byPath.get('Circle.Area').doc, 'Area computes the circle area.');
    assert.ok(byPath.get('Circle.Area').exported, 'capitalised name is Go-exported');
    assert.ok(byPath.get('NewCircle').exported);
    assert.ok(!byPath.get('notExported').exported);
    // Exactly one symbol for Area, not two (upstream's doc-carrying match and
    // this project's receiver-carrying override both match the SAME node).
    assert.equal(r.symbols.filter((s) => s.name === 'Area').length, 1);
});

test('Rust: impl methods dedupe to kind=method (not also function), parent = impl type', () => {
    const r = extract('sample.rs', RUST);
    const byPath = new Map(r.symbols.map((s) => [s.path, s]));

    assert.equal(r.symbols.filter((s) => s.name === 'new').length, 1, 'method+function double-capture must dedupe');
    assert.equal(byPath.get('Circle.new').kind, 'method');
    assert.equal(byPath.get('Circle.new').parent, 'Circle');
    assert.ok(byPath.get('Circle.new').exported, 'pub fn');
    assert.ok(!byPath.get('Circle.helper').exported, 'fn (no pub)');
    assert.equal(byPath.get('Circle').doc, 'Doc comment for Circle struct.');
    assert.ok(!byPath.get('not_exported').exported);
});

test('a broken file in each language: parsed structure still empty, no crash', () => {
    for (const [p, src] of [
        ['broken.ts', TS_BROKEN],
        ['broken.py', PY_BROKEN],
        ['broken.go', GO_BROKEN],
        ['broken.rs', RUST_BROKEN],
    ]) {
        const r = extract(p, src);
        assert.equal(r.indexed, true);
        assert.ok(Array.isArray(r.symbols));
    }
});

test('F2: ERROR/MISSING nodes become treesitter-syntax diagnostics, offsets usable by lineColAt', () => {
    for (const [rel, src] of [
        ['broken.ts', TS_BROKEN],
        ['broken.py', PY_BROKEN],
        ['broken.go', GO_BROKEN],
        ['broken.rs', RUST_BROKEN],
    ]) {
        const v = validateSource({ rel, text: src });
        assert.ok(v.diagnostics.some((d) => d.source === 'treesitter-syntax'), `${rel} should report a syntax diagnostic`);
        const d = v.diagnostics.find((d2) => d2.source === 'treesitter-syntax');
        assert.equal(d.severity, 'error');
        assert.ok(Number.isInteger(d.line) && d.line >= 1);
        assert.ok(Number.isInteger(d.column) && d.column >= 1);
    }
    // A CLEAN file of each language reports none.
    for (const [rel, src] of [
        ['sample.ts', TS],
        ['sample.py', PY],
        ['sample.go', GO],
        ['sample.rs', RUST],
    ]) {
        const v = validateSource({ rel, text: src });
        assert.ok(!v.diagnostics.some((d) => d.source === 'treesitter-syntax'), `${rel} should be clean`);
    }
});

test('runtime unavailable (injected loader): extract() degrades to "searchable, no symbol extractor yet", never throws', async () => {
    treesitter._resetForTest({
        loader: () => {
            throw new Error('simulated: web-tree-sitter not installed');
        },
    });
    try {
        const r = extract('x.py', PY);
        assert.equal(r.indexed, true, 'still searchable — isTextual does not depend on the grammar');
        assert.equal(r.parsed, false);
        assert.deepEqual(r.symbols, []);
        assert.match(r.reason, /no symbol extractor yet/);

        // The syntax gate degrades the same way: no grammar, no diagnostics —
        // not a false "no errors", a true "nothing was checked".
        const v = validateSource({ rel: 'x.py', text: PY_BROKEN });
        assert.ok(!v.diagnostics.some((d) => d.source === 'treesitter-syntax'));
    } finally {
        treesitter._resetForTest();
        await Promise.all(['typescript', 'python', 'go', 'rust'].map((id) => treesitter.ensureLanguage(id)));
    }
});
