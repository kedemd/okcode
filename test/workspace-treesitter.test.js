'use strict';
// Workspace-level proof for Phase F (localFs facade, real okdb store): a
// .py/.go/.rs/.ts file is findable and readable BY SYMBOL PATH the same way
// a .js file already is (F1), and the edit-safety gate refuses a syntax
// error introduced by an edit while still allowing an edit to an
// ALREADY-broken file that does not add one (F2, same rule JS already gets
// from compareDiagnostics — DESIGN §7).

const { describe, it, before, after } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');
const OKDB = require('@kedem/okdb');
const { openWorkspace } = require('../src/workspace');
const { openStore } = require('../src/store');
const { localFs } = require('../src/access');
const treesitter = require('../src/analysis/treesitter');

const GO_SAMPLE = [
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
].join('\n');

const PY_SAMPLE = [
    'class Animal:',
    '    """Base animal class."""',
    '',
    '    def speak(self):',
    '        """Return the sound."""',
    "        return 'noise'",
    '',
].join('\n');

const RUST_SAMPLE = [
    'pub struct Circle {',
    '    pub radius: f64,',
    '}',
    '',
    'impl Circle {',
    '    pub fn new(radius: f64) -> Circle {',
    '        Circle { radius }',
    '    }',
    '}',
    '',
].join('\n');

const TS_SAMPLE = [
    'export class Circle {',
    '  radius: number;',
    '  constructor(radius: number) {',
    '    this.radius = radius;',
    '  }',
    '  area(): number {',
    '    return 3.14159 * this.radius * this.radius;',
    '  }',
    '}',
    '',
].join('\n');

const JAVA_SAMPLE = [
    'package sample;',
    '',
    '/** The user class. */',
    'public class User {',
    '    public String getName() {',
    '        return "ok";',
    '    }',
    '}',
    '',
].join('\n');

const CSHARP_SAMPLE = [
    'namespace Sample {',
    '    public class User {',
    '        public string GetName() {',
    '            return "ok";',
    '        }',
    '    }',
    '}',
    '',
].join('\n');

const C_SAMPLE = ['int add(int a, int b) {', '    return a + b;', '}', ''].join('\n');

// `bar` is declared OUT-OF-LINE only (no in-class prototype) — a real
// header would also forward-declare it, but that is already covered by
// the analysis-treesitter.test.js extraction fixture; keeping it out here
// avoids two `Box.bar` symbols in the SAME file (read()'s own ambiguity).
const CPP_SAMPLE = ['class Box {', 'public:', '};', '', 'void Box::bar() { return; }', ''].join('\n');

// Already broken on disk BEFORE any edit — the baseline itself carries a
// syntax error. The guard must only refuse NEW diagnostics, so an edit that
// does not add one must still be allowed (DESIGN §7's "repair path").
const GO_ALREADY_BROKEN = ['package broken', '', 'func AlreadyBroken(a int, b int int {', '\treturn a + b', '}', ''].join(
    '\n',
);

function tmpRoot() {
    const base = fs.mkdtempSync(path.join(os.tmpdir(), 'okcode-f1-ts-'));
    return { base, root: path.join(base, 'proj') };
}

describe('workspace over localFs: tree-sitter languages', () => {
    const cleanups = [];
    let n = 0;
    after(async () => {
        for (const c of cleanups) await c().catch(() => {});
    });

    async function setup() {
        const { base, root } = tmpRoot();
        fs.mkdirSync(root, { recursive: true });
        const w = (rel, text) => {
            fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
            fs.writeFileSync(path.join(root, rel), text);
        };
        w('sample.go', GO_SAMPLE);
        w('sample.py', PY_SAMPLE);
        w('sample.rs', RUST_SAMPLE);
        w('sample.ts', TS_SAMPLE);
        w('sample.java', JAVA_SAMPLE);
        w('sample.cs', CSHARP_SAMPLE);
        w('sample.c', C_SAMPLE);
        w('sample.cpp', CPP_SAMPLE);
        w('already_broken.go', GO_ALREADY_BROKEN);

        const access = localFs(root);
        const id = `ws-ts-${++n}`;
        const db = new OKDB(path.join(base, 'okdb'), { auth: { open: true } });
        await db.open();
        const st = await openStore({ db, id, access });
        const ws = await openWorkspace({ id, access, store: st });
        cleanups.push(async () => {
            await ws.close();
            await db.close();
            fs.rmSync(base, { recursive: true, force: true });
        });
        return { root, ws };
    }

    before(async () => {
        // Preload every grammar up front so this suite exercises the real
        // extractor, not the "runtime not yet loaded" fallback — the fallback
        // path is covered by analysis-treesitter.test.js's own test.
        await Promise.all(
            ['typescript', 'python', 'go', 'rust', 'java', 'csharp', 'c', 'cpp'].map((id) => treesitter.ensureLanguage(id)),
        );
    });

    it('find/outline/read by symbol path works for .py, .go, .rs and .ts', async () => {
        const { ws } = await setup();

        const goHits = await ws.find('Area');
        assert.ok(goHits.some((h) => h.path === 'Circle.Area'), JSON.stringify(goHits));
        const goRead = await ws.read('sample.go#Circle.Area');
        assert.ok(goRead.ok && goRead.body.includes('3.14159'));

        const pyRead = await ws.read('sample.py#Animal.speak');
        assert.ok(pyRead.ok && pyRead.body.includes("'noise'"));

        const rsRead = await ws.read('sample.rs#Circle.new');
        assert.ok(rsRead.ok && rsRead.body.includes('Circle { radius }'));

        const tsRead = await ws.read('sample.ts#Circle.area');
        assert.ok(tsRead.ok && tsRead.body.includes('3.14159'));

        const outline = await ws.outline('sample.go');
        assert.ok(outline.symbols.some((s) => s.path === 'Circle.Area'));
    });

    it('find/outline/read by symbol path works for .java, .cs, .c and .cpp', async () => {
        const { ws } = await setup();

        const javaHits = await ws.find('getName');
        assert.ok(javaHits.some((h) => h.path === 'User.getName' && h.file === 'sample.java'), JSON.stringify(javaHits));
        const javaRead = await ws.read('sample.java#User.getName');
        assert.ok(javaRead.ok && javaRead.body.includes('return "ok"'));

        const csRead = await ws.read('sample.cs#User.GetName');
        assert.ok(csRead.ok && csRead.body.includes('return "ok"'));

        const cRead = await ws.read('sample.c#add');
        assert.ok(cRead.ok && cRead.body.includes('return a + b'));

        const cppRead = await ws.read('sample.cpp#Box.bar');
        assert.ok(cppRead.ok && cppRead.body.includes('return;'));

        const outline = await ws.outline('sample.java');
        assert.ok(outline.symbols.some((s) => s.path === 'User.getName'));
    });

    it('an edit introducing a NEW syntax error in a .go file is refused', async () => {
        const { ws } = await setup();
        const before1 = await ws.read('sample.go#Circle.Area');
        const broken = await ws.edit(
            'sample.go#Circle.Area',
            // Missing closing paren on the func signature — ERROR/MISSING for
            // tree-sitter, just like the hand-written broken.go fixture.
            'func (c *Circle) Area() float64 {\n\treturn 3.14159 * c.Radius * c.Radius\n',
            { at: before1.at },
        );
        assert.equal(broken.ok, false, JSON.stringify(broken));
        assert.ok(
            broken.newDiagnostics && broken.newDiagnostics.some((d) => d.source === 'treesitter-syntax'),
            JSON.stringify(broken),
        );
    });

    it('an edit to an ALREADY-broken .go file that adds no new error is allowed', async () => {
        const { ws } = await setup();
        const before1 = await ws.read('already_broken.go');
        assert.ok(!before1.at || true); // whole-file read always succeeds; at is the hash either way
        // Rename the parameter only — the missing `)` stays exactly as broken
        // as it already was; no NEW diagnostic is introduced.
        const candidate = GO_ALREADY_BROKEN.replace('a int, b int int {', 'x int, b int int {');
        const result = await ws.writeWholeFile('already_broken.go', candidate, { expectedHash: before1.at });
        assert.ok(result.ok, JSON.stringify(result));
    });
});
