'use strict';
// A decorator/attribute belongs to the symbol it decorates (DESIGN §12): a
// decorated definition's stored span starts at its first decorator, not at
// `def`/`class`/`fn` — so `read('file#name')` shows the decorator, and an
// edit by symbol name touches (or, if it omits it, REMOVES) the decorator
// too, because the span is the one source of truth for "what this symbol
// is". Covers Python decorators, TypeScript decorators (class + method),
// Rust outer attributes, and PHP attributes.

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

function tmpRoot() {
    const base = fs.mkdtempSync(path.join(os.tmpdir(), 'okcode-decorators-'));
    return { base, root: path.join(base, 'proj') };
}

describe('decorators/attributes belong to the symbol they decorate', () => {
    const cleanups = [];
    let n = 0;
    after(async () => {
        for (const c of cleanups) await c().catch(() => {});
    });

    before(async () => {
        await Promise.all(
            ['typescript', 'python', 'rust', 'php', 'java', 'csharp', 'cpp'].map((id) => treesitter.ensureLanguage(id)),
        );
    });

    async function setup(files) {
        const { base, root } = tmpRoot();
        fs.mkdirSync(root, { recursive: true });
        for (const [rel, text] of Object.entries(files)) {
            const full = path.join(root, rel);
            fs.mkdirSync(path.dirname(full), { recursive: true });
            fs.writeFileSync(full, text);
        }
        const access = localFs(root);
        const id = `ws-decorators-${++n}`;
        const db = new OKDB(path.join(base, 'okdb'), { auth: { open: true } });
        await db.open();
        const st = await openStore({ db, id, access });
        const ws = await openWorkspace({ id, access, store: st });
        cleanups.push(async () => {
            await ws.close();
            await db.close();
            fs.rmSync(base, { recursive: true, force: true });
        });
        return { root, ws, access };
    }

    it('Python: read() includes the decorator(s), lineStart is the decorator\'s own line', async () => {
        const src = ['@app.route("/x")', '@login_required', 'def get_user():', '    return 1', ''].join('\n');
        const { ws } = await setup({ 'app.py': src });
        await ws.sync();
        const r = await ws.read('app.py#get_user');
        assert.ok(r.ok, JSON.stringify(r));
        assert.ok(r.body.startsWith('@app.route("/x")'), r.body);
        assert.ok(r.body.includes('@login_required'));
        const outline = await ws.outline('app.py');
        const sym = outline.symbols.find((s) => s.name === 'get_user');
        assert.equal(sym.lineStart, 1);
        // The SIGNATURE is the full def up to the body, trailing ':' trimmed.
        assert.equal(sym.signature, 'def get_user()');
    });

    it('TypeScript: a method\'s/property\'s decorator (a sibling, not a child) extends the stored span backward', async () => {
        const src = ['class Foo {', '  @Input()', '  bar: number = 1;', '', '  @Log()', '  baz() {', '    return 1;', '  }', '}', ''].join(
            '\n',
        );
        const { ws } = await setup({ 'foo.ts': src });
        await ws.sync();
        const r = await ws.read('foo.ts#Foo.baz');
        assert.ok(r.ok, JSON.stringify(r));
        assert.ok(r.body.startsWith('@Log()'), r.body);
        const outline = await ws.outline('foo.ts');
        const sym = outline.symbols.find((s) => s.path === 'Foo.baz');
        assert.equal(sym.signature, 'baz()');
    });

    it('Rust: outer attributes (`#[derive]`, `#[test]`) as preceding siblings extend the stored span', async () => {
        const src = ['#[derive(Debug)]', '#[test]', 'pub fn foo() -> i32 {', '    1', '}', ''].join('\n');
        const { ws } = await setup({ 'lib.rs': src });
        await ws.sync();
        const r = await ws.read('lib.rs#foo');
        assert.ok(r.ok, JSON.stringify(r));
        assert.ok(r.body.startsWith('#[derive(Debug)]'), r.body);
        assert.ok(r.body.includes('#[test]'));
        const outline = await ws.outline('lib.rs');
        const sym = outline.symbols.find((s) => s.name === 'foo');
        assert.equal(sym.signature, 'pub fn foo() -> i32');
    });

    it('PHP: an attribute is already part of the node\'s own span; read() shows it and the signature still skips it', async () => {
        const src = ['<?php', 'class User {', '    #[Deprecated]', '    public function getName() {', '        return 1;', '    }', '}', ''].join(
            '\n',
        );
        const { ws } = await setup({ 'user.php': src });
        await ws.sync();
        const r = await ws.read('user.php#User.getName');
        assert.ok(r.ok, JSON.stringify(r));
        assert.ok(r.body.startsWith('#[Deprecated]'), r.body);
        const outline = await ws.outline('user.php');
        const sym = outline.symbols.find((s) => s.path === 'User.getName');
        assert.equal(sym.signature, 'public function getName()');
    });

    it("Java: an annotation is already part of the node's own span (bundled with the modifiers); read() shows it, and the signature skips the WHOLE modifiers node (not just the annotation)", async () => {
        const src = ['class User {', '    @Deprecated', '    public String getName() {', '        return "x";', '    }', '}', ''].join(
            '\n',
        );
        const { ws } = await setup({ 'user.java': src });
        await ws.sync();
        const r = await ws.read('user.java#User.getName');
        assert.ok(r.ok, JSON.stringify(r));
        assert.ok(r.body.startsWith('@Deprecated'), r.body);
        const outline = await ws.outline('user.java');
        const sym = outline.symbols.find((s) => s.path === 'User.getName');
        // `public` is bundled into the SAME `modifiers` node as the
        // annotation in this grammar (unlike PHP/C#) — skipping the
        // annotation from the signature costs `public` too, a documented
        // trade-off (DESIGN §12), not a bug.
        assert.equal(sym.signature, 'String getName() {');
    });

    it("C#: an attribute is its own field, separate from the modifier(s); read() shows it, and the signature keeps `public` while skipping only the attribute", async () => {
        const src = ['class User {', '    [Obsolete]', '    public string GetName() {', '        return "x";', '    }', '}', ''].join(
            '\n',
        );
        const { ws } = await setup({ 'user.cs': src });
        await ws.sync();
        const r = await ws.read('user.cs#User.GetName');
        assert.ok(r.ok, JSON.stringify(r));
        assert.ok(r.body.startsWith('[Obsolete]'), r.body);
        const outline = await ws.outline('user.cs');
        const sym = outline.symbols.find((s) => s.path === 'User.GetName');
        assert.equal(sym.signature, 'public string GetName() {');
    });

    it("C++: a `[[deprecated]]` attribute is already part of the (possibly widened) node's own span; read() shows it, and the signature skips it", async () => {
        const src = ['[[deprecated]]', 'int helper() {', '    return 1;', '}', ''].join('\n');
        const { ws } = await setup({ 'helper.cpp': src });
        await ws.sync();
        const r = await ws.read('helper.cpp#helper');
        assert.ok(r.ok, JSON.stringify(r));
        assert.ok(r.body.startsWith('[[deprecated]]'), r.body);
        const outline = await ws.outline('helper.cpp');
        const sym = outline.symbols.find((s) => s.name === 'helper');
        assert.equal(sym.signature, 'int helper() {');
    });

    it('editBatch by symbol name still works, and a replacement body that OMITS the decorator removes it — the span is the source of truth', async () => {
        const src = ['@app.route("/x")', 'def get_user():', '    return 1', ''].join('\n');
        const { ws, access } = await setup({ 'app.py': src });
        await ws.sync();
        const before1 = await ws.read('app.py#get_user');
        assert.ok(before1.body.startsWith('@app.route("/x")'));

        // A replacement that KEEPS the decorator: the decorator survives,
        // because it is still part of the body being written.
        const kept = await ws.edit('app.py#get_user', '@app.route("/x")\ndef get_user():\n    return 2\n', { at: before1.at });
        assert.ok(kept.ok, JSON.stringify(kept));
        const onDiskKept = fs.readFileSync(path.join(access.root, 'app.py'), 'utf8');
        assert.ok(onDiskKept.includes('@app.route("/x")'), onDiskKept);
        assert.ok(onDiskKept.includes('return 2'));

        // A SECOND edit, this time passing a body that omits the decorator
        // entirely — since the stored span (the thing editBatch replaces)
        // starts AT the decorator, the decorator is gone from the file
        // afterward, not preserved by magic. This is the behaviour the task
        // calls out explicitly: a caller who wants to keep a decorator MUST
        // include it in the replacement body.
        const after1 = await ws.read('app.py#get_user');
        const stripped = await ws.edit('app.py#get_user', 'def get_user():\n    return 3\n', { at: after1.at });
        assert.ok(stripped.ok, JSON.stringify(stripped));
        const onDiskStripped = fs.readFileSync(path.join(access.root, 'app.py'), 'utf8');
        assert.ok(!onDiskStripped.includes('@app.route'), onDiskStripped);
        assert.ok(onDiskStripped.includes('return 3'));
    });
});
