'use strict';
// Library code on demand: node_modules (any skip-named directory) is never
// indexed, but a path that names it is searched, read and outlined DIRECTLY —
// a fresh listing of just that subtree, nothing recorded, nothing kept in the
// store. Through each facade (localFs, the shell facade over a local bash, and
// over real ssh), with a real okdb store behind it.

const { describe, it, before, after } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const OKDB = require('@kedem/okdb');
const { openWorkspace } = require('../lib/workspace');
const { openStore } = require('../lib/store');
const { localFs, shell } = require('../lib/access');
const { createTools } = require('../lib/tools');
const { writeFixture, tmpRoot, facades } = require('./fixtures/code-fixture');

const PKG = 'node_modules/@kedem/okjs';
const EACH = [
    "'use strict';", //                                            1
    '// Bind the loop variable of an <each> block.', //            2
    'function bindEach(items, name) {', //                         3
    '    return items.map((it) => ({ [name]: it, tag: "<each>" }));', // 4
    '}', //                                                        5
    'const TEMPLATE = `', //                                       6
    '  <each items="rows" as="row">', //                           7
    '    <td>${row.name}</td>', //                                 8
    '  </each>`;', //                                              9
    'module.exports = { bindEach, TEMPLATE };', //                 10
    '',
].join('\n');

function writeLibrary(root, outside) {
    const w = (rel, text) => {
        fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
        fs.writeFileSync(path.join(root, rel), text);
    };
    w(`${PKG}/package.json`, '{ "name": "@kedem/okjs", "main": "src/each.js" }\n');
    w(`${PKG}/src/each.js`, EACH);
    // A package's dist/ is often all the code it ships: searched once named.
    w(`${PKG}/dist/okjs.js`, 'var tag = "<each>";\n');
    // A nested node_modules is another library, reached by naming it.
    w(`${PKG}/node_modules/dep/index.js`, 'module.exports = "<each> in a dep";\n');
    w(`${PKG}/.npmrc`, '//registry/:_authToken="<each>-secret"\n');
    // Skip names are directories: a symbol and a project FILE named like one
    // stay on the index.
    w('src/tasks.js', 'function build() {\n    return 1;\n}\nmodule.exports = { build };\n');
    w('dist', '#!/bin/sh\necho "dist script"\n');
    // A symlink out of the workspace must never be followed.
    fs.mkdirSync(outside, { recursive: true });
    fs.writeFileSync(path.join(outside, 'leak.js'), 'const leaked = "<each>";\n');
    fs.symlinkSync(outside, path.join(root, 'node_modules/escape'));
}

const FACADES = [
    ...facades().slice(0, 1),
    { name: 'shell/bash', make: (root) => shell({ root, run: shell.localRunner('bash') }) },
    ...facades().slice(1),
];

describe('access list({ under, limit })', () => {
    const { base, root } = tmpRoot('lib-access');
    const outside = path.join(base, 'outside');
    writeFixture(root);
    writeLibrary(root, outside);
    after(() => fs.rmSync(base, { recursive: true, force: true }));
    const facs = [
        ['localFs', localFs(root)],
        ['shell/bash', shell({ root, run: shell.localRunner('bash') })],
    ];
    for (const [name, f] of facs) {
        it(`${name}: a subtree, skip names applied below it only, confined`, async () => {
            const all = (await f.list()).map((r) => r.path);
            assert.ok(!all.some((p) => p.startsWith('node_modules/')), 'the default listing never enters node_modules');
            const sub = (await f.list({ under: PKG, skip: ['node_modules'] })).map((r) => r.path).sort();
            // The facade lists the secret file; the workspace withholds it.
            assert.deepEqual(sub, [
                `${PKG}/.npmrc`,
                `${PKG}/dist/okjs.js`,
                `${PKG}/package.json`,
                `${PKG}/src/each.js`,
            ]);
            const one = await f.list({ under: `${PKG}/src/each.js`, skip: [] });
            assert.deepEqual(
                one.map((r) => [r.path, r.size]),
                [[`${PKG}/src/each.js`, Buffer.byteLength(EACH)]],
            );
            assert.deepEqual(await f.list({ under: `${PKG}/nope`, skip: [] }), []);
            const capped = await f.list({ under: PKG, skip: [], limit: 2 });
            assert.equal(capped.length, 2);
            assert.equal(capped.truncated, true);
            assert.equal((await f.list({ under: PKG, skip: [], limit: 50 })).truncated, undefined);
            await assert.rejects(f.list({ under: 'node_modules/escape' }), /escapes the root/);
            await assert.rejects(f.list({ under: 'node_modules/escape/leak.js' }), /escapes the root/);
            await assert.rejects(f.list({ under: 'node_modules/../..' }), /escapes the root/);
        });
    }
});

for (const fac of FACADES) {
    describe(`library code over ${fac.name}`, { skip: fac.skip }, () => {
        const cleanups = [];
        let n = 0;
        before(async () => {
            if (fac.before) await fac.before();
        });
        after(async () => {
            for (const c of cleanups) await c().catch(() => {});
            if (fac.after) await fac.after();
        });

        async function setup(options = {}) {
            const { base, root } = tmpRoot('lib');
            writeFixture(root);
            writeLibrary(root, path.join(base, 'outside'));
            const access = fac.make(root);
            const id = `lib-${++n}`;
            const db = new OKDB(path.join(base, 'okdb'), { auth: { open: true } });
            await db.open();
            const st = await openStore({ db, id, access });
            const ws = await openWorkspace({ id, access, store: st, options });
            await ws.refresh();
            await ws.flush();
            cleanups.push(async () => {
                await ws.close();
                await db.close();
                fs.rmSync(base, { recursive: true, force: true });
            });
            return { ws, st, root, base };
        }

        // Nothing from the library in the working set, the stats or the store.
        async function assertUntouched(ws, st, before) {
            await ws.flush();
            const after = await ws.stats();
            assert.equal(after.files, before.files);
            assert.equal(after.symbols, before.symbols);
            assert.equal(after.pendingWrites, 0);
            assert.ok(!(await ws.files({})).files.some((f) => f.file.startsWith('node_modules/')));
            assert.ok(![...st.load().files.keys()].some((k) => k.startsWith('node_modules/')), 'nothing stored');
            assert.ok(!st.search('bindEach', 20).length, 'no library symbol in full-text search');
            assert.ok(!(st.searchContent('bindEach', 20) || []).length, 'no library text in full-text search');
        }

        it('grep: no paths never sees the library; a library path finds it, exact text, nothing recorded', async () => {
            const { ws, st } = await setup();
            const before = await ws.stats();
            const plain = await ws.grep('"<each>"');
            assert.equal(plain.total.lines, 0, 'a plain search covers exactly the project');
            assert.equal(plain.library, undefined, 'and its answer has no library part');

            const r = await ws.grep('"<each>"', { paths: [PKG] });
            assert.deepEqual(
                r.matches.map((m) => `${m.file}:${m.line} ${m.text}`),
                [`${PKG}/dist/okjs.js:1 var tag = "<each>";`, `${PKG}/src/each.js:4 ${EACH.split('\n')[3]}`],
            );
            assert.equal(r.library.files, 3, 'dist in, nested node_modules and .npmrc out');
            assert.equal(r.library.truncated, false);
            assert.deepEqual(r.library.paths, [PKG]);
            // Multi-line content: each line its own, with context.
            const m = await ws.grep('<each items="rows" as="row">', { paths: [`${PKG}/src/each.js`], context: 1 });
            assert.equal(m.matches.length, 1);
            assert.equal(m.matches[0].line, 7);
            assert.deepEqual(m.matches[0].after, ['    <td>${row.name}</td>']);
            // Globs narrow inside a named library; never reach one alone.
            assert.equal((await ws.grep('<each>', { paths: [PKG], glob: '*.json' })).total.lines, 0);
            assert.equal((await ws.grep('<each>', { glob: 'node_modules/**' })).total.lines, 0);
            // The nested library, named explicitly.
            assert.equal((await ws.grep('in a dep', { paths: [`${PKG}/node_modules/dep`] })).total.lines, 1);
            // Project and library paths together, in path order.
            const both = await ws.grep('use strict', { paths: ['lib', PKG] });
            assert.ok(both.counts.some((c) => c.file.startsWith('lib/')));
            assert.ok(both.counts.some((c) => c.file === `${PKG}/src/each.js`));
            const order = both.counts.map((c) => c.file);
            assert.deepEqual(order, [...order].sort());
            await assertUntouched(ws, st, before);
        });

        it('read: a library file by path — whole, range, #symbol — directly, nothing recorded', async () => {
            const { ws, st } = await setup();
            const before = await ws.stats();
            const whole = await ws.read(`${PKG}/src/each.js`);
            assert.equal(whole.ok, true);
            assert.equal(whole.library, true);
            assert.equal(whole.body, EACH.split('\n').slice(0, 11).join('\n'));
            const range = await ws.read(`${PKG}/src/each.js:6-9`);
            assert.equal(range.body, EACH.split('\n').slice(5, 9).join('\n'));
            const sym = await ws.read(`${PKG}/src/each.js#bindEach`);
            assert.equal(sym.kind, 'function');
            assert.deepEqual([sym.lineStart, sym.lineEnd], [3, 5]);
            assert.match(sym.body, /tag: "<each>"/);
            assert.equal((await ws.readFile(`${PKG}/package.json`)).library, true);
            assert.equal((await ws.readAt(`${PKG}/src/each.js`, 4, 4)).body, EACH.split('\n')[3]);
            assert.equal((await ws.read(`${PKG}/.npmrc`)).ok, false, 'secrets stay withheld');
            const dir = await ws.read(PKG);
            assert.equal(dir.ok, false);
            assert.match(dir.reason, /is a directory/);
            // A symbol-addressed read without a path stays index-only.
            assert.equal((await ws.read('bindEach')).ok, false);
            const build = await ws.read('build');
            assert.equal(build.file, 'src/tasks.js');
            assert.equal(build.library, undefined);
            // (No extension: opaque, so the INDEX's answer is "not text".)
            const script = await ws.readFile('dist');
            assert.equal(script.library, undefined, 'a project file named dist is answered by the index');
            assert.match(script.reason, /dist is not indexed as text/);
            await assertUntouched(ws, st, before);
        });

        it('outline and find-by-name of a package: parsed in memory, fresh, never persisted', async () => {
            const { ws, st, root } = await setup();
            const before = await ws.stats();
            const o = await ws.outline(PKG);
            assert.equal(o.ok, true);
            assert.equal(o.by, 'files');
            assert.deepEqual(
                o.entries.map((e) => e.file),
                [`${PKG}/dist/okjs.js`, `${PKG}/package.json`, `${PKG}/src/each.js`],
            );
            const each = o.entries.find((e) => e.file.endsWith('each.js'));
            assert.ok(each.symbols.some((x) => x.name === 'bindEach' && x.exported));
            const file = await ws.outline(`${PKG}/src/each.js`);
            assert.equal(file.by, 'symbol');
            assert.ok(file.symbols.some((x) => x.name === 'bindEach' && x.lineStart === 3));

            const hits = await ws.find('bindEach', { path: PKG });
            assert.equal(hits[0].name, 'bindEach');
            assert.equal(hits[0].file, `${PKG}/src/each.js`);
            assert.equal(hits[0].via, 'library');
            assert.ok(!(await ws.find('bindEach')).some((h) => h.file.startsWith('node_modules/')));
            // An install rewrites files: the next scan sees it.
            fs.appendFileSync(path.join(root, PKG, 'src/each.js'), 'function addedLater() {}\n');
            assert.ok((await ws.find('addedLater', { path: PKG })).length);
            await assert.rejects(
                ws.ask('how is the loop bound', { path: PKG }),
                (e) => e.code === 'OKCODE_LIBRARY_NOT_INDEXED',
            );
            await assertUntouched(ws, st, before);
        });

        it('confined: `..` and a symlink out of the root are refused', async () => {
            const { ws } = await setup();
            await assert.rejects(
                ws.grep('<each>', { paths: ['node_modules/../..'] }),
                (e) => e.code === 'OKCODE_BAD_PATH',
            );
            await assert.rejects(
                ws.grep('<each>', { paths: ['node_modules/escape'] }),
                (e) => e.code === 'OKCODE_BAD_PATH' && /escapes the root/.test(e.message),
            );
            const r = await ws.read('node_modules/escape/leak.js');
            assert.equal(r.ok, false);
            assert.match(r.reason, /escapes the root/);
            assert.equal((await ws.outline('node_modules/escape')).ok, false);
            await assert.rejects(
                ws.find('leaked', { path: 'node_modules/escape' }),
                (e) => e.code === 'OKCODE_BAD_PATH',
            );
        });

        it('over the file cap: a partial answer that says so (narrow the path)', async () => {
            const { ws, root } = await setup({ libraryFiles: 3, libraryParseFiles: 2 });
            for (let i = 0; i < 8; i++) fs.writeFileSync(path.join(root, PKG, `src/m${i}.js`), `// "<each>" ${i}\n`);
            const r = await ws.grep('"<each>"', { paths: [PKG] });
            assert.equal(r.library.truncated, true);
            assert.equal(r.library.cap, 3);
            assert.ok(r.searched <= 3);
            const o = await ws.outline(PKG);
            assert.equal(o.capped, true);
            const tools = createTools({ workspace: async () => ws, workspaces: () => [{ id: ws.id }] });
            const text = await tools.render('code_grep', { pattern: '"<each>"', paths: [PKG] });
            assert.match(
                text.split('\n')[1],
                /PARTIAL: listing node_modules\/@kedem\/okjs stopped at 3 files — narrow the path/,
            );
            assert.match(await tools.render('code_outline', { file: PKG }), /PARTIAL: stopped at 2 files/);
        });

        it('a library path with nothing there says so: never a silent "no match"', async () => {
            const { ws } = await setup();
            const r = await ws.grep('"<each>"', { paths: ['node_modules/not-installed'] });
            assert.deepEqual(r.library.missing, ['node_modules/not-installed']);
            assert.equal(r.total.lines, 0);
            const found = await ws.grep('"<each>"', { paths: [PKG] });
            assert.equal(found.library.missing, undefined, 'a package that is there is never missing');
            const f = await ws.find('bindEach', { path: 'node_modules/not-installed' });
            assert.equal(f.library.missing, true);
            const tools = createTools({ workspace: async () => ws, workspaces: () => [{ id: ws.id }] });
            const text = await tools.render('code_grep', { pattern: '"<each>"', paths: ['node_modules/not-installed'] });
            assert.match(text.split('\n')[1], /NOT FOUND: nothing at node_modules\/not-installed — not installed, or a wrong path/);
            assert.match(await tools.render('code_find', { query: 'bindEach', path: 'node_modules/not-installed' }), /NOT FOUND/);
        });

        it('tools: grep/read/outline/find take a library path; ask says it is unsupported', async () => {
            const { ws } = await setup();
            const tools = createTools({ workspace: async () => ws, workspaces: () => [{ id: ws.id }] });
            const g = await tools.render('code_grep', { pattern: 'tag: "<each>"', paths: [PKG] });
            assert.match(g, /library code read directly, not indexed/);
            assert.ok(g.includes(`each.js:4: ${EACH.split('\n')[3]}`), g);
            const rd = await tools.render('code_read', { symbol: `${PKG}/src/each.js:4-4` });
            assert.match(rd, /library code, read directly — not indexed/);
            assert.doesNotMatch(rd, /at=/);
            const ol = await tools.render('code_outline', { file: PKG });
            assert.match(ol, /src\/each\.js {2}\(11 lines\) {2}bindEach\*/);
            const fd = await tools.render('code_find', { query: 'bindEach', path: PKG });
            assert.match(fd, /function bindEach {2}node_modules\/@kedem\/okjs\/src\/each\.js:3-5/);
            const ask = await tools.render('code_ask', { question: 'how is the loop bound', path: PKG });
            assert.match(ask, /library code, which is never embedded: searching it by meaning is not supported/);
            const schemas = tools.schemas();
            const grepDef = schemas.find((d) => d.name === 'code_grep');
            assert.match(grepDef.description, /no node_modules/);
            assert.match(grepDef.description, /paths: \["node_modules\/<pkg>"\]/);
        });
    });
}

// The remote shell facade's PowerShell script must take the same fields as
// the bash one (root, under, limit, skip names…); with no pwsh here, at least
// keep the scripts in step textually.
it('powershell list script reads the subtree and limit fields', () => {
    const ps = fs.readFileSync(path.join(__dirname, '../crates/okcode-access/scripts/powershell/list.ps1'), 'utf8');
    assert.match(ps, /\$under = NextField\s*\n\$limit = \[int\]\(NextField\)/);
});
