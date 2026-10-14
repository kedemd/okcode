'use strict';
// JSX support (DESIGN §12/§6/§3 amendment): acorn cannot parse JSX at all, so
// before this fix every `.jsx` file, and every `.js`/`.mjs`/`.cjs` file
// written with JSX in it, got `parsed: false` and no symbols. The fix is a
// tree-sitter TSX fallback (treesitter.js's 'typescript' grammar, forced
// regardless of the path's own extension) that only runs when acorn fails —
// see parse.js's extract() and treesitter.js's extractJsxFallback.
//
// Three layers, same split the rest of the suite uses: raw extraction
// (parse.js, no workspace), resolved edges/refs (a real workspace), and the
// edit-time syntax gate (validate.js, exercised through ws.edit()).

const { describe, test, it, before, after } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');
const OKDB = require('@kedem/okdb');
const { extract } = require('../src/analysis/parse');
const treesitter = require('../src/analysis/treesitter');
const { openWorkspace } = require('../src/workspace');
const { openStore } = require('../src/store');
const { localFs } = require('../src/access');

before(async () => {
    // The fallback needs the 'typescript' grammar (it carries the .tsx wasm
    // too) resident before extract()'s synchronous call can use it — the
    // same seam treesitter.js's module comment documents for every other
    // tree-sitter language.
    await treesitter.ensureLanguage('typescript');
});

const JSX_COMPONENT = [
    "import React from 'react';",
    '',
    '// A doc comment above a component.',
    'export function Greeting({ name }) {',
    '  return <div className="g">Hello, {name}!</div>;',
    '}',
    '',
    'export class Panel extends React.Component {',
    '  // method doc',
    '  render() {',
    '    return <section>{this.props.children}</section>;',
    '  }',
    '}',
    '',
    'const DEFAULT_LABEL = <span>default</span>;',
    '',
    'export const Labeled = (props) => {',
    '  return helper(props) ? <b>{props.label}</b> : null;',
    '};',
    '',
    "const { useState } = require('react');",
    '',
].join('\n');

describe('JSX extraction (parse.js extract(), no workspace)', () => {
    test('a .jsx file with no acorn-parseable syntax at all still gets symbols via the TSX fallback', () => {
        const r = extract('a.jsx', 'export function A(){ return <div/>; }\n');
        assert.equal(r.parsed, true, JSON.stringify(r));
        assert.equal(r.lang, 'javascript');
        assert.equal(r.extractor, 'treesitter-tsx');
        assert.ok(/tsx/i.test(r.reason || ''), r.reason);
        assert.ok(r.symbols.some((s) => s.name === 'A' && s.kind === 'function'), JSON.stringify(r.symbols));
        assert.ok(r.analyzerVersion && r.analyzerVersion.includes('jsx:'), r.analyzerVersion);
    });

    test('a .js file containing JSX (common in React code) gets the same fallback', () => {
        const r = extract('a.js', 'module.exports = function A(){ return <div/>; };\n');
        assert.equal(r.parsed, true, JSON.stringify(r));
        assert.equal(r.extractor, 'treesitter-tsx');
    });

    test('plain .js/.mjs/.cjs with no JSX still goes through acorn, unchanged', () => {
        const r = extract('a.js', 'function add(a, b) { return a + b; }\n');
        assert.equal(r.parsed, true);
        assert.equal(r.extractor, 'acorn');
        assert.ok(r.symbols.some((s) => s.name === 'add'));
    });

    test('functions, classes/methods, consts and call-site references all extract from real JSX', () => {
        const r = extract('component.jsx', JSX_COMPONENT);
        assert.equal(r.parsed, true, JSON.stringify(r));
        assert.equal(r.extractor, 'treesitter-tsx');
        const byName = new Map(r.symbols.map((s) => [s.path, s]));
        assert.ok(byName.has('Greeting') && byName.get('Greeting').kind === 'function');
        assert.equal(byName.get('Greeting').doc, 'A doc comment above a component.');
        assert.ok(byName.has('Panel') && byName.get('Panel').kind === 'class');
        assert.ok(byName.has('Panel.render') && byName.get('Panel.render').kind === 'method');
        assert.ok(byName.has('DEFAULT_LABEL'), 'a top-level const is a symbol even when unexported');
        assert.ok(byName.has('Labeled') && byName.get('Labeled').kind === 'function', 'an exported const arrow is kind=function');
        assert.ok(r.references.some((ref) => ref.name === 'helper' && ref.kind === 'call'), JSON.stringify(r.references));
        assert.deepEqual(r.exports.sort(), ['Greeting', 'Labeled', 'Panel'].sort());
    });

    test('ESM import and CommonJS require() both produce import edges from a .jsx file', () => {
        const r = extract('component.jsx', JSX_COMPONENT);
        const reactImports = r.imports.filter((i) => i.from === 'react');
        assert.equal(reactImports.length, 2, JSON.stringify(r.imports));
        assert.ok(reactImports.some((i) => (i.names || []).some((n) => n.local === 'React')), 'the ESM default import');
        assert.ok(
            reactImports.some((i) => (i.names || []).some((n) => n.local === 'useState')),
            'the require() destructure (grammars/typescript/imports.scm — added for this fallback path)',
        );
    });

    test('a file genuinely broken under BOTH acorn and tree-sitter stays unparsed, never falsely "parsed"', () => {
        const r = extract('bad.jsx', 'function {{{ broken\n');
        assert.equal(r.parsed, false, JSON.stringify(r));
        assert.equal(r.indexed, true);
        assert.ok(r.reason && r.reason.length > 0);
    });
});

function tmpRoot() {
    const base = fs.mkdtempSync(path.join(os.tmpdir(), 'okcode-jsx-'));
    return { base, root: path.join(base, 'proj') };
}

describe('JSX over a real workspace: refs/importers and the edit-time syntax gate', () => {
    const cleanups = [];
    let n = 0;
    after(async () => {
        for (const c of cleanups) await c().catch(() => {});
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
        const id = `ws-jsx-${++n}`;
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

    it('find/outline/read by symbol path works across .jsx files; importers()/refs() resolve a .jsx import', async () => {
        const { ws } = await setup({
            'Button.jsx': [
                'export function Button({ onClick, children }) {',
                '  return <button onClick={onClick}>{children}</button>;',
                '}',
                '',
            ].join('\n'),
            'App.jsx': [
                "import { Button } from './Button.jsx';",
                '',
                'export function App() {',
                '  return <Button onClick={() => Button}>Go</Button>;',
                '}',
                '',
            ].join('\n'),
        });
        await ws.sync();

        const hits = await ws.find('Button');
        assert.ok(hits.some((h) => h.file === 'Button.jsx' && h.path === 'Button'), JSON.stringify(hits));
        const read = await ws.read('Button.jsx#Button');
        assert.ok(read.ok && read.body.includes('onClick={onClick}'));
        const outline = await ws.outline('App.jsx');
        assert.ok(outline.symbols.some((s) => s.path === 'App'));

        const imp = await ws.importers('Button.jsx');
        assert.ok(imp.ok, JSON.stringify(imp));
        assert.deepEqual(imp.importers.map((i) => i.file), ['App.jsx']);

        const refs = await ws.refs('Button');
        const appHit = refs.find((h) => h.file === 'App.jsx' && h.label === 'imported');
        assert.ok(appHit, JSON.stringify(refs));
    });

    it('a clean edit to a .jsx file passes; an edit that adds a real syntax error is refused', async () => {
        const { ws } = await setup({
            'Clean.jsx': [
                'export function Clean() {',
                '  return <div className="x">hi</div>;',
                '}',
                '',
            ].join('\n'),
        });
        await ws.sync();

        // Clean: a plain edit by symbol name, still valid JSX.
        const before1 = await ws.read('Clean.jsx#Clean');
        const ok = await ws.edit('Clean.jsx#Clean', 'export function Clean() {\n  return <div className="y">hi there</div>;\n}', {
            at: before1.at,
        });
        assert.ok(ok.ok && ok.outcome === 'ok', JSON.stringify(ok));

        // Broken: a missing closing brace — a real syntax error, not just
        // JSX acorn alone dislikes. The baseline's own "javascript-acorn"/
        // "javascript-node-check" validators are `skipped` (clean JSX, see
        // validateSource's jsxFallback), so whichever validator catches
        // this (acorn/node-check on the shifted failure position, or
        // tree-sitter directly) is unconditionally NEW relative to the
        // baseline — refused either way.
        const before2 = await ws.read('Clean.jsx#Clean');
        const bad = await ws.edit(
            'Clean.jsx#Clean',
            'export function Clean() {\n  return <div className="y">hi there</div>;\n',
            { at: before2.at },
        );
        assert.equal(bad.ok, false, JSON.stringify(bad));
        assert.ok(bad.newDiagnostics && bad.newDiagnostics.length > 0, JSON.stringify(bad));
    });
});
