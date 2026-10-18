'use strict';
// The generic tree-sitter extractor and syntax gate — one engine,
// data per language. Asserts the CONTRACT the overrides exist for (see each
// queries/<lang>/tags.local.scm in the tree-sitter crate for what upstream's
// tags.scm misses and why), nesting/parent, exported-by-convention and dedupe,
// through the public analysis API.

const test = require('node:test');
const assert = require('node:assert/strict');
const { extract, langOf } = require('../lib/analysis/parse');
const { validateSource } = require('../lib/analysis/validate');

// <grammar version>[+<bundled grammar>]:q<queries>.<extractor>
const ANALYZER_VERSION = /^\d+\.\d+\.\d+\S*:q\d+\.\d+$/;

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

const PHP = [
    '<?php',
    'namespace App\\Models;',
    '',
    'const VERSION = 1;',
    '',
    '/** The user model. */',
    'class User {',
    '    const MAX_NAME = 255;',
    '',
    '    public $name;',
    '',
    '    // Returns the display name.',
    '    public function getName() {',
    '        return $this->name;',
    '    }',
    '',
    '    private function secret() {',
    '        return null;',
    '    }',
    '}',
    '',
    'function helper() {',
    '    return 1;',
    '}',
    '',
    'enum Suit {',
    '    case Hearts;',
    '    case Spades;',
    '}',
    '',
    'trait Greets {}',
    'interface Named {}',
    '',
].join('\n');

const PHP_BROKEN = ['<?php', 'class Broken {', '    function method(a {', '        return a;', '    }', '}', ''].join('\n');

const JAVA = [
    'package com.example.app;',
    '',
    '/** The user class. */',
    'public class User {',
    '    public static final int MAX_NAME = 255;',
    '',
    '    public User() {}',
    '',
    '    // Returns the display name.',
    '    public String getName() {',
    '        return null;',
    '    }',
    '',
    '    private void secret() {}',
    '}',
    '',
    'enum Suit {',
    '    HEARTS, SPADES',
    '}',
    '',
    'interface Named {}',
    '',
    'record Point(int x, int y) {}',
    '',
].join('\n');

const JAVA_BROKEN = ['package app;', '', 'class Broken {', '    void method(int a {', '        return;', '    }', '}', ''].join('\n');

const CSHARP = [
    'namespace App.Models {',
    '    public struct Point {}',
    '',
    '    public enum Color { Red, Blue }',
    '',
    '    /// The user class.',
    '    public class User {',
    '        public string GetName() { return null; }',
    '',
    '        public int Age { get; set; }',
    '',
    '        private void Secret() {}',
    '    }',
    '}',
    '',
].join('\n');

const CSHARP_BROKEN = ['namespace App {', '    class Broken {', '        void Method(int a {', '            return;', '        }', '    }', '}', ''].join('\n');

const C = [
    '#define MAX 10',
    '#define SQ(x) ((x)*(x))',
    '',
    'typedef struct { int x; int y; } Point;',
    '',
    'enum Color { RED, BLUE };',
    '',
    'static int internalHelper(int a) {',
    '    return a;',
    '}',
    '',
    'int add(int a, int b) {',
    '    return a + b;',
    '}',
    '',
].join('\n');

const C_BROKEN = ['int add(int a, int b) {', '    return a +', '}', ''].join('\n');

const CPP = [
    'class Box {',
    'public:',
    '  void bar();',
    '};',
    '',
    'void Box::bar() { return; }',
    '',
    'namespace App {',
    '  enum Color { Red, Blue };',
    '',
    '  [[deprecated]]',
    '  int helper() { return 1; }',
    '',
    '  template<typename T>',
    '  class Wrapper { T value; };',
    '}',
    '',
    'static int internalHelper() { return 2; }',
    '',
].join('\n');

const CPP_BROKEN = ['class Broken {', '  void method(int a {', '    return;', '  }', '};', ''].join('\n');

test('the tree-sitter extensions map to their languages', () => {
    assert.equal(langOf('x.ts'), 'typescript');
    assert.equal(langOf('x.tsx'), 'typescript');
    assert.equal(langOf('x.mts'), 'typescript');
    assert.equal(langOf('x.cts'), 'typescript');
    assert.equal(langOf('x.py'), 'python');
    assert.equal(langOf('x.go'), 'go');
    assert.equal(langOf('x.rs'), 'rust');
    assert.equal(langOf('x.js'), 'javascript');
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
    assert.equal(byPath.get('Circle.area.helper').parent, 'Circle.area');
    assert.equal(byPath.get('Point').kind, 'type');
    assert.ok(byPath.get('Point').exported);
    assert.equal(byPath.get('makeCircle').kind, 'function'); // export const f = (...) => ...
    assert.ok(byPath.get('makeCircle').exported);
    assert.equal(byPath.get('notExported').kind, 'function');
    assert.ok(!byPath.get('notExported').exported);
    assert.equal(byPath.get('Color').kind, 'enum');
    assert.match(r.analyzerVersion, ANALYZER_VERSION);
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
    assert.equal(byPath.get('Animal.speak.inner_helper').doc, '');
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

test('PHP: namespace/class/interface/trait/enum/function/method/constant — Class.method path, public/default visibility', () => {
    const r = extract('sample.php', PHP);
    assert.equal(langOf('sample.php'), 'php');
    const byPath = new Map(r.symbols.map((s) => [s.path, s]));

    assert.equal(byPath.get('App\\Models').kind, 'module');
    assert.equal(byPath.get('VERSION').kind, 'constant');
    assert.ok(byPath.get('VERSION').exported, 'namespace-level constant has nothing to hide behind');
    assert.equal(byPath.get('User').kind, 'class');
    assert.equal(byPath.get('User').doc, 'The user model.');
    assert.equal(byPath.get('User.MAX_NAME').kind, 'constant');
    assert.equal(byPath.get('User.MAX_NAME').parent, 'User');
    assert.equal(byPath.get('User.getName').kind, 'method');
    assert.equal(byPath.get('User.getName').doc, 'Returns the display name.');
    assert.ok(byPath.get('User.getName').exported, 'public method');
    assert.ok(!byPath.get('User.secret').exported, 'private method');
    // Exactly one symbol for getName, not two (upstream's own
    // @definition.function capture and the local @definition.method
    // override both match the SAME method_declaration node).
    assert.equal(r.symbols.filter((s) => s.name === 'getName').length, 1);
    assert.ok(byPath.get('helper').exported);
    assert.equal(byPath.get('Suit').kind, 'enum');
    assert.equal(byPath.get('Greets').kind, 'interface'); // trait, same kind as an interface
    assert.equal(byPath.get('Named').kind, 'interface');
    assert.match(r.analyzerVersion, ANALYZER_VERSION);
});

test('Java: package/class/interface/enum/record/method/constructor/static-final-constant — Class.method path, public/package-private visibility', () => {
    const r = extract('sample.java', JAVA);
    assert.equal(langOf('sample.java'), 'java');
    const byPath = new Map(r.symbols.map((s) => [s.path, s]));

    assert.equal(byPath.get('com.example.app').kind, 'module');
    assert.equal(byPath.get('User').kind, 'class');
    assert.equal(byPath.get('User').doc, 'The user class.');
    assert.ok(byPath.get('User').exported, 'public class');
    assert.equal(byPath.get('User.MAX_NAME').kind, 'constant');
    assert.equal(byPath.get('User.MAX_NAME').parent, 'User');
    assert.ok(byPath.get('User.MAX_NAME').exported);
    assert.equal(byPath.get('User.User').kind, 'method'); // constructor, same name as its class
    assert.equal(byPath.get('User.getName').kind, 'method');
    assert.equal(byPath.get('User.getName').doc, 'Returns the display name.');
    assert.ok(byPath.get('User.getName').exported, 'public method');
    assert.ok(!byPath.get('User.secret').exported, 'package-private method, no `public`');
    assert.equal(byPath.get('Suit').kind, 'enum');
    assert.equal(byPath.get('Named').kind, 'interface');
    assert.equal(byPath.get('Point').kind, 'class'); // record
    assert.ok(!byPath.get('Suit').exported, 'top-level type with no `public` modifier');
    assert.match(r.analyzerVersion, ANALYZER_VERSION);
});

test('C#: namespace/struct/enum/class/method/property — dotted namespace containment, public/default visibility', () => {
    const r = extract('sample.cs', CSHARP);
    assert.equal(langOf('sample.cs'), 'csharp');
    const byPath = new Map(r.symbols.map((s) => [s.path, s]));

    assert.equal(byPath.get('App.Models').kind, 'module');
    assert.equal(byPath.get('App.Models.Point').kind, 'class'); // struct
    assert.equal(byPath.get('App.Models.Color').kind, 'enum');
    assert.equal(byPath.get('App.Models.User').kind, 'class');
    assert.equal(byPath.get('App.Models.User').doc, 'The user class.');
    assert.equal(byPath.get('App.Models.User.GetName').kind, 'method');
    assert.equal(byPath.get('App.Models.User.GetName').parent, 'App.Models.User');
    assert.ok(byPath.get('App.Models.User.GetName').exported, 'public method');
    assert.equal(byPath.get('App.Models.User.Age').kind, 'property');
    assert.ok(byPath.get('App.Models.User.Age').exported);
    assert.ok(!byPath.get('App.Models.User.Secret').exported, 'private method');
    assert.match(r.analyzerVersion, ANALYZER_VERSION);
});

test('C: macro/typedef/enum/function — non-static-at-file-scope visibility, full span for a function (not just its declarator)', () => {
    const r = extract('sample.c', C);
    assert.equal(langOf('sample.c'), 'c');
    const byPath = new Map(r.symbols.map((s) => [s.path, s]));

    assert.equal(byPath.get('MAX').kind, 'macro');
    assert.equal(byPath.get('SQ').kind, 'macro');
    assert.equal(byPath.get('Point').kind, 'type'); // typedef struct
    assert.equal(byPath.get('Color').kind, 'enum');
    assert.ok(!byPath.get('internalHelper').exported, 'static — internal linkage');
    assert.ok(byPath.get('add').exported);
    // The widened span reaches the body, not just the `add(int a, int b)`
    // declarator upstream's own tags.scm captures on its own.
    assert.ok(byPath.get('add').signature.startsWith('int add('));
    assert.equal(byPath.get('add').lineEnd, byPath.get('add').lineStart + 2);
    assert.match(r.analyzerVersion, ANALYZER_VERSION);
});

test('C++: namespace/class/method (in-class and out-of-line)/enum/template/attribute — Class.method path incl. Foo::bar, non-static-at-file-scope visibility', () => {
    const r = extract('sample.cpp', CPP);
    assert.equal(langOf('sample.cpp'), 'cpp');
    const byPath = new Map(r.symbols.map((s) => [s.path, s]));

    assert.equal(byPath.get('Box').kind, 'class');
    const bars = r.symbols.filter((s) => s.name === 'bar');
    assert.equal(bars.length, 2, 'the in-class prototype AND the out-of-line definition are both symbols');
    assert.ok(bars.every((s) => s.kind === 'method' && s.parent === 'Box'));
    // The signature stops at the body now; the SPAN is what carries it.
    const outOfLine = bars.find((s) => CPP.slice(s.start, s.end).includes('return'));
    assert.ok(outOfLine, 'the out-of-line definition carries the full widened span, body included');
    assert.equal(byPath.get('App').kind, 'module');
    assert.equal(byPath.get('App.Color').kind, 'enum');
    assert.equal(byPath.get('App.helper').kind, 'function');
    assert.equal(byPath.get('App.Wrapper').kind, 'class');
    // The template's own SPAN reaches back to `template<typename T>` via
    // the same `wrapperTypes` mechanism Python's `@decorator` already
    // uses — the one-line signature itself still starts at `class
    // Wrapper`, same "decorator skipped from the signature" convention.
    assert.ok(byPath.get('App.Wrapper').start < CPP.indexOf('class Wrapper'), 'span widened back to the template line');
    assert.ok(byPath.get('App.Wrapper').signature.startsWith('class Wrapper'));
    assert.ok(!byPath.get('internalHelper').exported, 'static — internal linkage');
    assert.match(r.analyzerVersion, ANALYZER_VERSION);
});

test('signature: the full definition up to its BODY, not just the first line — multi-line params, per language', () => {
    const ts = [
        'export async function createUser(',
        '  name: string,',
        '  age: number,',
        '): Promise<User> {',
        '  return { name, age };',
        '}',
        '',
    ].join('\n');
    const tsSym = extract('multi.ts', ts).symbols.find((s) => s.name === 'createUser');
    // `export` wraps the declaration one level up (the typescript entry of
    // the tree-sitter crate's language table) — the signature, like the
    // definition's span, starts at the function itself.
    assert.equal(tsSym.signature, 'export async function createUser(name: string, age: number): Promise<User>');

    const py = ['def create_user(', '    name,', '    age,', '):', '    return {name: name, age: age}', ''].join('\n');
    const pySym = extract('multi.py', py).symbols.find((s) => s.name === 'create_user');
    assert.equal(pySym.signature, 'def create_user(name, age)');

    const go = ['package main', '', 'func CreateUser(', '\tname string,', '\tage int,', ') string {', '\treturn name', '}', ''].join('\n');
    const goSym = extract('multi.go', go).symbols.find((s) => s.name === 'CreateUser');
    assert.equal(goSym.signature, 'func CreateUser(name string, age int) string');

    const rust = ['pub fn create_user(', '    name: String,', '    age: i32,', ') -> String {', '    name', '}', ''].join('\n');
    const rustSym = extract('multi.rs', rust).symbols.find((s) => s.name === 'create_user');
    assert.equal(rustSym.signature, 'pub fn create_user(name: String, age: i32) -> String');

    const php = ['<?php', 'function createUser(', '    string $name,', '    int $age', ') {', '    return $name;', '}', ''].join('\n');
    const phpSym = extract('multi.php', php).symbols.find((s) => s.name === 'createUser');
    assert.equal(phpSym.signature, 'function createUser(string $name, int $age)');
});

test('signature: no body (an interface method, a bare declaration) is the whole node up to `;`/end', () => {
    const ts = ['export interface Shape {', '  area(radius: number): number;', '}', ''].join('\n');
    const sym = extract('nobody.ts', ts).symbols.find((s) => s.name === 'area');
    // The grammar's `method_signature` node itself does not span the
    // trailing `;` (a sibling token in `interface_body`) — the signature
    // follows the node's own end, same as every other symbol's span does.
    assert.equal(sym.signature, 'area(radius: number): number');
});

test('signature: capped at a sane length, with an explicit … when cut — never a silent truncation', () => {
    const params = Array.from({ length: 60 }, (_, i) => `paramNumber${i}: string`).join(', ');
    const ts = `export function manyParams(${params}): void {\n  return;\n}\n`;
    const sym = extract('capped.ts', ts).symbols.find((s) => s.name === 'manyParams');
    assert.ok(sym.signature.length <= 301, sym.signature.length);
    assert.ok(sym.signature.endsWith('…'), sym.signature);
    assert.ok(sym.signature.startsWith('export function manyParams(paramNumber0'), sym.signature);
});

test('a broken file in each language: parsed structure still empty, no crash', () => {
    for (const [p, src] of [
        ['broken.ts', TS_BROKEN],
        ['broken.py', PY_BROKEN],
        ['broken.go', GO_BROKEN],
        ['broken.rs', RUST_BROKEN],
        ['broken.php', PHP_BROKEN],
        ['broken.java', JAVA_BROKEN],
        ['broken.cs', CSHARP_BROKEN],
        ['broken.c', C_BROKEN],
        ['broken.cpp', CPP_BROKEN],
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
        ['broken.php', PHP_BROKEN],
        ['broken.java', JAVA_BROKEN],
        ['broken.cs', CSHARP_BROKEN],
        ['broken.c', C_BROKEN],
        ['broken.cpp', CPP_BROKEN],
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
        ['sample.php', PHP],
        ['sample.java', JAVA],
        ['sample.cs', CSHARP],
        ['sample.c', C],
        ['sample.cpp', CPP],
    ]) {
        const v = validateSource({ rel, text: src });
        assert.ok(!v.diagnostics.some((d) => d.source === 'treesitter-syntax'), `${rel} should be clean`);
    }
});

test('.h: parsed with the C++ grammar — a C++ header keeps its classes and full paths, a C header still parses', () => {
    assert.equal(langOf('box.h'), 'cpp');
    const cpp = extract('box.h', 'namespace App {\nclass Box {\npublic:\n  void bar();\n};\n}\n');
    assert.deepEqual(cpp.symbols.map((s) => s.path), ['App', 'App.Box', 'App.Box.bar']);
    const c = extract('util.h', '#define MAX 3\nint add(int a, int b);\n');
    assert.deepEqual(c.symbols.map((s) => s.name).sort(), ['MAX', 'add']);
});

// ── real-repo fixes: call sites, top-level consts (DESIGN §12) ─────────────
// Running okcode on a 435-file Next.js/TypeScript app and a Python package
// found these empty/missing where fixture tests never looked.

const CALL_SRC = 'function f(){ helper(1); o.m(2); }';

test('call sites: helper(1); o.m(2) — TypeScript, TSX, C#, C, C++, PHP', () => {
    const ts = extract('call.ts', CALL_SRC);
    assert.deepEqual(
        ts.references.filter((r) => r.kind === 'call').map((r) => r.name).sort(),
        ['helper', 'm'],
    );
    const tsx = extract('call.tsx', `function f(){ helper(1); o.m(2); return <div/>; }`);
    assert.deepEqual(
        tsx.references.filter((r) => r.kind === 'call').map((r) => r.name).sort(),
        ['helper', 'm'],
    );
    const cs = extract('call.cs', `class C { void f(){ helper(1); o.m(2); } }`);
    assert.deepEqual(
        cs.references.filter((r) => r.kind === 'call').map((r) => r.name).sort(),
        ['helper', 'm'],
    );
    const c = extract('call.c', `void f(){ helper(1); o.m(2); }`);
    assert.deepEqual(
        c.references.filter((r) => r.kind === 'call').map((r) => r.name).sort(),
        ['helper', 'm'],
    );
    const cpp = extract('call.cpp', `void f(){ helper(1); o.m(2); ns::q(3); }`);
    assert.deepEqual(
        cpp.references.filter((r) => r.kind === 'call').map((r) => r.name).sort(),
        ['helper', 'm', 'q'],
    );
    const php = extract('call.php', `<?php function f(){ helper(1); $o->m(2); } ?>`);
    assert.deepEqual(
        php.references.filter((r) => r.kind === 'call').map((r) => r.name).sort(),
        ['helper', 'm'],
    );
});

test('top-level const/let/var becomes a symbol — TypeScript (incl. a call-built component)', () => {
    const src = [
        'export const CHART_COLORS = { a: 1 };',
        'export const label = "x";',
        'const config = { a: 1 };',
        'export default config;',
        'const Card = forwardRef((p, ref) => null);',
        'const Card2 = memo(() => null);',
        'function f(){ const local = 1; return local; }',
    ].join('\n');
    const r = extract('consts.ts', src);
    const byName = new Map(r.symbols.map((s) => [s.name, s]));
    assert.equal(byName.get('CHART_COLORS').kind, 'constant');
    assert.ok(byName.get('CHART_COLORS').exported);
    assert.equal(byName.get('label').kind, 'constant');
    assert.equal(byName.get('config').kind, 'constant');
    assert.equal(byName.get('Card').kind, 'constant', 'forwardRef(...) component gets a symbol');
    assert.equal(byName.get('Card2').kind, 'constant', 'memo(...) component gets a symbol');
    assert.ok(!byName.has('local'), 'a local const inside a scope stays unaddressed');
});

test('top-level const/var becomes a symbol — Go (incl. a grouped const block), Rust, C, C++, PHP define(), C# const field', () => {
    const go = extract('consts.go', [
        'package main',
        'const Foo = 1',
        'const (',
        '  A = 1',
        '  b = 2',
        ')',
        'var Bar = "x"',
        'func f() { var x = 1; _ = x }',
    ].join('\n'));
    const goNames = new Map(go.symbols.map((s) => [s.name, s]));
    assert.equal(goNames.get('Foo').kind, 'constant');
    assert.ok(goNames.get('Foo').exported);
    assert.equal(goNames.get('A').kind, 'constant');
    assert.ok(!goNames.get('b').exported, 'lowercase is not package-exported');
    assert.equal(goNames.get('Bar').kind, 'constant');
    assert.ok(!goNames.has('x'), 'a local var inside a function stays unaddressed');

    const rust = extract('consts.rs', 'const FOO: i32 = 1;\nstatic BAR: i32 = 2;\nfn f(){ let x = 1; }\n');
    const rustNames = new Set(rust.symbols.map((s) => s.name));
    assert.ok(rustNames.has('FOO') && rustNames.has('BAR'));
    assert.ok(!rustNames.has('x'));

    const c = extract('consts.c', 'int counter = 0;\nstatic int hidden = 1;\nvoid f(){ int local = 1; }\n');
    const cByName = new Map(c.symbols.map((s) => [s.name, s]));
    assert.equal(cByName.get('counter').kind, 'constant');
    assert.ok(cByName.get('counter').exported);
    assert.ok(!cByName.get('hidden').exported, 'static at file scope is not exported');
    assert.ok(!cByName.has('local'));

    const cpp = extract('consts.cpp', 'int counter = 0;\nconstexpr int MAX = 10;\n');
    const cppNames = new Set(cpp.symbols.map((s) => s.name));
    assert.ok(cppNames.has('counter') && cppNames.has('MAX'));

    const php = extract('consts.php', `<?php\nconst FOO = 1;\ndefine('BAR', 2);\n?>`);
    const phpNames = new Map(php.symbols.map((s) => [s.name, s]));
    assert.equal(phpNames.get('FOO').kind, 'constant');
    assert.equal(phpNames.get('BAR').kind, 'constant');

    const cs = extract('consts.cs', `class C { public int x = 1; const int W = 3; private static readonly int Y = 2; }`);
    const csNames = new Map(cs.symbols.map((s) => [s.name, s]));
    assert.equal(csNames.get('W').kind, 'constant');
    assert.ok(!csNames.has('x'), 'a plain field stays unindexed');
    assert.ok(!csNames.has('Y'), 'static readonly is not the `const` keyword this override targets');
});

test('globals inside a namespace / inline mod, and pointer globals, are symbols; locals are not', () => {
    const cpp = extract('a.cpp', 'namespace app {\nconstexpr int kMax = 3;\nconst char* name = "x";\nvoid f() { int local = 1; }\n}\nint *p;\n');
    assert.deepEqual(
        cpp.symbols.filter((s) => s.kind === 'constant').map((s) => s.path).sort(),
        ['app.kMax', 'app.name', 'p'],
    );
    const c = extract('a.c', 'static const char *top = "y";\nint *p;\nvoid f(void) { char *local = 0; }\n');
    assert.deepEqual(c.symbols.filter((s) => s.kind === 'constant').map((s) => s.path).sort(), ['p', 'top']);
    const rs = extract('a.rs', 'mod cfg {\n    pub const MAX: usize = 3;\n    static NAME: &str = "x";\n    fn f() { const LOCAL: u8 = 2; }\n}\n');
    assert.deepEqual(rs.symbols.filter((s) => s.kind === 'constant').map((s) => s.path).sort(), ['cfg.MAX', 'cfg.NAME']);
});
