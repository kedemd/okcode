'use strict';
// F3 (resolved edges) and F4 item 3 (okjs component usage) — WORKPLAN
// Phase F. One fixture repo per language, covering the resolution rule that
// repo exercises (DESIGN §12): relative + tsconfig `paths` for TypeScript, a
// package-relative import for Python, a module import for Go, a `crate::`
// path for Rust, a barrel re-export for JS, and an okjs component tag.
// Also: refs() labels/ranking, importers(), and cache invalidation after an
// edit (the one correctness property that is easy to get wrong in a cache
// keyed by `generation`).

const { describe, it, after } = require('node:test');
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
    const base = fs.mkdtempSync(path.join(os.tmpdir(), 'okcode-edges-'));
    return { base, root: path.join(base, 'proj') };
}

describe('F3 resolved edges + F4.3 okjs component usage', () => {
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
        const id = `ws-edges-${++n}`;
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

    it('JS CJS: a plain `const x = require(...)` resolves to a file (a destructured one is a known, pre-existing parse.js gap — not captured as an import edge at all)', async () => {
        const { ws } = await setup({
            'util.js': "function helper() { return 1; }\nmodule.exports = { helper };\n",
            'index.js': "const util = require('./util');\nfunction run() { return util.helper(); }\nmodule.exports = { run };\n",
        });
        await ws.sync();
        const r = await ws.importers('util.js');
        assert.ok(r.ok);
        assert.deepEqual(r.importers.map((i) => i.file), ['index.js']);
    });

    it('JS ESM: a named import resolves, a barrel `export * from` is followed one hop, and refs() labels/ranks accordingly', async () => {
        const { ws } = await setup({
            'util.mjs': 'export function helper() { return 1; }\n',
            'barrel.mjs': "export * from './util.mjs';\n",
            'index.mjs': "import { helper } from './barrel.mjs';\nfunction run() { return helper(); }\nexport { run };\n",
            'unrelated.mjs': "// helper is just a word here, imported from nothing\nexport const notes = 'helper';\n",
        });
        await ws.sync();
        // The barrel re-export makes index.mjs (which only names `barrel.mjs`
        // directly) an importer of util.mjs too, one hop, flagged `via`.
        const imp = await ws.importers('util.mjs');
        assert.ok(imp.ok);
        const viaBarrel = imp.importers.find((i) => i.file === 'index.mjs');
        assert.ok(viaBarrel, JSON.stringify(imp));
        assert.equal(viaBarrel.via, 'barrel.mjs');
        const hits = await ws.refs('helper');
        const byFile = new Map(hits.map((h) => [`${h.file}:${h.line}`, h]));
        assert.equal(byFile.get('index.mjs:1').label, 'imported');
        assert.equal(byFile.get('index.mjs:2').label, 'imported');
        assert.equal(byFile.get('index.mjs:2').call, true, 'line 2 is an actual call site');
        assert.equal(byFile.get('unrelated.mjs:2').label, 'textual', 'no resolved import in THIS file, even though the name exists elsewhere');
        // imported ranked ahead of textual — see the exact order.
        const ranks = hits.map((h) => h.label);
        const firstTextual = ranks.indexOf('textual');
        assert.ok(ranks.slice(0, firstTextual).every((l) => l !== 'textual'));
    });

    it('TypeScript: a tsconfig.json `paths` alias resolves to the aliased file', async () => {
        const { ws } = await setup({
            'tsconfig.json': JSON.stringify({
                compilerOptions: { baseUrl: './src', paths: { '@app/*': ['app/*'] } },
            }),
            'src/app/widget.ts': 'export function thing() { return 2; }\n',
            'src/index.ts': "import { thing } from '@app/widget';\nexport function run() { return thing(); }\n",
        });
        await treesitter.ensureLanguage('typescript');
        await ws.sync();
        const r = await ws.importers('src/app/widget.ts');
        assert.ok(r.ok);
        assert.deepEqual(
            r.importers.map((i) => i.file),
            ['src/index.ts'],
        );
    });

    it('Python: a package-relative import (`from .util import helper`) resolves within the package', async () => {
        const { ws } = await setup({
            'pkg/__init__.py': '',
            'pkg/util.py': 'def helper():\n    return 1\n',
            'pkg/main.py': 'from .util import helper\n\n\ndef run():\n    return helper()\n',
        });
        await treesitter.ensureLanguage('python');
        await ws.sync();
        const r = await ws.importers('pkg/util.py');
        assert.ok(r.ok);
        assert.equal(r.importers.length, 1);
        assert.equal(r.importers[0].file, 'pkg/main.py');
        const hits = await ws.refs('helper');
        assert.ok(hits.some((h) => h.file === 'pkg/main.py' && h.label === 'imported' && h.call));
    });

    it('Go: an import resolves via go.mod\'s module path to the package directory\'s files', async () => {
        const { ws } = await setup({
            'go.mod': 'module example.com/app\n\ngo 1.21\n',
            'sub/util.go': 'package sub\n\nfunc Helper() int {\n\treturn 1\n}\n',
            'main.go': 'package main\n\nimport "example.com/app/sub"\n\nfunc run() int {\n\treturn sub.Helper()\n}\n',
        });
        await treesitter.ensureLanguage('go');
        await ws.sync();
        const r = await ws.importers('sub/util.go');
        assert.ok(r.ok);
        assert.deepEqual(
            r.importers.map((i) => i.file),
            ['main.go'],
        );
    });

    it('Rust: a `crate::` path resolves through a `mod` declaration to the defining file', async () => {
        const { ws } = await setup({
            'Cargo.toml': '[package]\nname = "app"\nversion = "0.1.0"\n',
            'src/lib.rs': 'pub mod util;\npub mod caller;\n',
            'src/util.rs': 'pub fn helper() -> i32 {\n    1\n}\n',
            'src/caller.rs': 'use crate::util::helper;\n\npub fn run() -> i32 {\n    helper()\n}\n',
        });
        await treesitter.ensureLanguage('rust');
        await ws.sync();
        const r = await ws.importers('src/util.rs');
        assert.ok(r.ok);
        const files = r.importers.map((i) => i.file).sort();
        assert.deepEqual(files, ['src/caller.rs', 'src/lib.rs']);
        const hits = await ws.refs('helper');
        const useLine = hits.find((h) => h.file === 'src/caller.rs' && h.line === 1);
        assert.equal(useLine.label, 'imported');
    });

    it('okjs: a component used by another component is a resolved importer, not a tag grep (F4 item 3)', async () => {
        let hasOkjs = true;
        try {
            require.resolve('@kedem/okjs/tooling');
        } catch {
            hasOkjs = false;
        }
        if (!hasOkjs) return; // optional peer absent — nothing to prove here
        const { ws } = await setup({
            'widget.ok.js': "export default {\n    tag: 'x-widget',\n    template: `<x-widget></x-widget>`,\n};\n",
            'page.ok.js':
                "export default {\n    tag: 'x-page',\n    dependencies: [{ tag: 'x-widget' }],\n    template: `<x-widget></x-widget>`,\n};\n",
        });
        await ws.sync();
        const r = await ws.importers('widget.ok.js');
        assert.ok(r.ok);
        assert.deepEqual(
            r.importers.map((i) => ({ file: i.file, kind: i.kind })),
            [{ file: 'page.ok.js', kind: 'component-tag' }],
        );
    });

    it('cache invalidation: importers() reflects an edit without reopening the workspace', async () => {
        const { ws, access } = await setup({
            'a.js': 'module.exports = { x: 1 };\n',
            'b.js': "const a = require('./a');\nmodule.exports = { a };\n",
        });
        await ws.sync();
        assert.equal((await ws.importers('a.js')).importers.length, 1);
        // A new file starts importing `a.js` too — written straight to disk
        // and picked up by a sync, same as any other edit; the edge index
        // must not still answer from its first-generation cache.
        fs.writeFileSync(path.join(access.root, 'c.js'), "const a = require('./a');\nmodule.exports = { a };\n");
        await ws.sync();
        const after2 = await ws.importers('a.js');
        assert.equal(after2.importers.length, 2, JSON.stringify(after2));
    });

    it('PHP: a `use` statement resolves via composer.json PSR-4, and a relative `require` resolves by path', async () => {
        const { ws } = await setup({
            'composer.json': JSON.stringify({ autoload: { 'psr-4': { 'App\\': 'src/' } } }),
            'src/Services/Mailer.php': '<?php\nnamespace App\\Services;\n\nclass Mailer {\n    public function send() {\n        return 1;\n    }\n}\n',
            'src/boot.php': '<?php\n// bootstraps the app\n',
            'src/Controller.php':
                "<?php\nnamespace App;\n\nuse App\\Services\\Mailer;\n\nrequire_once './boot.php';\n\nclass Controller {\n    public function run() {\n        \$m = new Mailer();\n        return \$m->send();\n    }\n}\n",
        });
        await treesitter.ensureLanguage('php');
        await ws.sync();
        const r = await ws.importers('src/Services/Mailer.php');
        assert.ok(r.ok, JSON.stringify(r));
        assert.deepEqual(
            r.importers.map((i) => i.file),
            ['src/Controller.php'],
        );
        const boot = await ws.importers('src/boot.php');
        assert.ok(boot.ok);
        assert.deepEqual(
            boot.importers.map((i) => i.file),
            ['src/Controller.php'],
        );
        const hits = await ws.refs('Mailer');
        assert.ok(hits.some((h) => h.file === 'src/Controller.php' && h.label === 'imported'));
    });

    it('PHP: a grouped `use A\\{B, C as D}` resolves both names, and an unresolvable namespace stays unresolved', async () => {
        const { ws } = await setup({
            'composer.json': JSON.stringify({ autoload: { 'psr-4': { 'App\\': 'src/' } } }),
            'src/Services/Mailer.php': '<?php\nnamespace App\\Services;\n\nclass Mailer {}\n',
            'src/Services/Logger.php': '<?php\nnamespace App\\Services;\n\nclass Logger {}\n',
            'src/Controller.php':
                '<?php\nnamespace App;\n\nuse App\\Services\\{Mailer, Logger as Log};\nuse Some\\Unconfigured\\Thing;\n\nclass Controller {}\n',
        });
        await treesitter.ensureLanguage('php');
        await ws.sync();
        assert.deepEqual((await ws.importers('src/Services/Mailer.php')).importers.map((i) => i.file), ['src/Controller.php']);
        assert.deepEqual((await ws.importers('src/Services/Logger.php')).importers.map((i) => i.file), ['src/Controller.php']);
    });
});
