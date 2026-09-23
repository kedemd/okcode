'use strict';
// The workspace end to end — scan, locate, read, edit — through each facade
// (localFs, and the shell facade over real ssh), with a real okdb store behind
// it: the configuration a host actually runs. Ported from the brain's
// test/codeindex.js (index/store/edit sections; the pure-analysis sections live
// in test/analysis-*.test.js).

const { describe, it, before, after } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const OKDB = require('@kedem/okdb');
const { openWorkspace } = require('../src/workspace');
const { openStore } = require('../src/store');
const { writeFixture, FIXTURE_FILES, tmpRoot, facades } = require('./fixtures/code-fixture');

for (const fac of facades()) {
    describe(`workspace over ${fac.name}`, { skip: fac.skip }, () => {
        const cleanups = [];
        let n = 0;

        before(async () => {
            if (fac.before) await fac.before();
        });
        after(async () => {
            for (const c of cleanups) await c().catch(() => {});
            if (fac.after) await fac.after();
        });

        // A fresh fixture, facade, okdb store and workspace per test. Each test
        // gets its own okdb (≈30ms to open) rather than one env apiece in a
        // shared one: every workspace is an okdb env, and an unlicensed okdb
        // allows 5 (incl. default) — a shared store would stop at test 5.
        async function setup({ store = true } = {}) {
            const { base, root } = tmpRoot();
            writeFixture(root);
            const access = fac.make(root);
            const id = `ws-${++n}`;
            let db = null;
            if (store) {
                db = new OKDB(path.join(base, 'okdb'), { auth: { open: true } });
                await db.open();
            }
            const st = store ? await openStore({ db, id, access }) : null;
            const ws = await openWorkspace({ id, access, store: st });
            const file = (rel) => path.join(root, ...rel.split('/'));
            const readDisk = (rel) => fs.readFileSync(file(rel), 'utf8');
            const writeDisk = (rel, text) => {
                fs.mkdirSync(path.dirname(file(rel)), { recursive: true });
                fs.writeFileSync(file(rel), text);
            };
            cleanups.push(async () => {
                // The db dir is deleted whole; removing each env (≈1.2s of okdb
                // close grace apiece) is covered by the store suite.
                await ws.close();
                if (db) await db.close();
                fs.rmSync(base, { recursive: true, force: true });
            });
            return { root, ws, st, access, file, readDisk, writeDisk };
        }

        it('open scans the project: .gitignore, dot-directories and node_modules stay out', async () => {
            const { ws } = await setup();
            const s = await ws.structure();
            assert.equal(s.files, FIXTURE_FILES.length, JSON.stringify(s.dirs));
            assert.deepEqual(s.unparsed, [], 'nothing failed to parse (incl. the .mjs)');
            assert.ok(s.symbols >= 6, `saw ${s.symbols}`);
            for (const hidden of [
                'session-state/key-1.json',
                'debug.log',
                '.cache/blob.js',
                'node_modules/widget/package.json',
            ]) {
                const r = await ws.readFile(hidden);
                assert.equal(r.ok, false, `${hidden} must not be indexed`);
            }
            const st = await ws.stats();
            assert.equal(st.files, FIXTURE_FILES.length);
            assert.equal(st.failedParse, 0);
        });

        it('locates by name, by prose, and by content through full-text search', async () => {
            const { ws } = await setup();
            const byName = await ws.find('add');
            assert.equal(byName[0].name, 'add', 'exact name ranks first with FTS on');
            assert.equal(byName[0].file, 'lib/math.js');
            assert.match(byName[0].signature || '', /add\(a, b\)/);
            assert.ok(byName.findIndex((h) => h.name === 'twice') > 0, 'a doc-only mention ranks below the exact name');
            assert.equal(new Set(byName.map((h) => `${h.file}:${h.name}`)).size, byName.length, 'no hit listed twice');

            const byDoc = await ws.find('prose is how a codebase');
            assert.ok(
                byDoc.some((h) => h.name === 'add'),
                JSON.stringify(byDoc.map((h) => h.name)),
            );

            const outOfOrder = await ws.find('indexed prose symbol');
            assert.ok(
                outOfOrder.some((h) => h.name === 'add'),
                'a doc phrase matches out of order',
            );
            assert.ok(
                outOfOrder.some((h) => h.via === 'fts' || (h.alsoVia || []).includes('fts')),
                JSON.stringify(outOfOrder.map((h) => h.via)),
            );

            const phrase = await ws.find('where is the prose above a symbol handled');
            assert.ok(
                phrase.some((h) => h.name === 'add'),
                'stopwords do not block a natural question',
            );

            // Content: a word that exists only inside a comment in a body.
            // The line matching the most terms is quoted, attributed to the
            // symbol containing it.
            const content = await ws.find('counter step total');
            const hit = content.find(
                (h) => h.via === 'content' && h.window && h.window.text.includes('Bumps the counter'),
            );
            assert.ok(hit, JSON.stringify(content));
            assert.equal(hit.file, 'lib/factory.js');
            assert.equal(hit.name, 'makeCounter');
            assert.equal(hit.line, 4);
        });

        it('refs: uses of a name, attributed to the innermost enclosing symbol', async () => {
            const { ws } = await setup();
            const uses = await ws.refs('TAX_RATE');
            assert.ok(uses.length >= 1);
            assert.ok(uses.every((u) => u.confidence === 'textual' && typeof u.symbol === 'string'));
            assert.ok(
                !uses.some((u) => u.file === 'lib/math.js' && /^const TAX_RATE/.test(u.text)),
                'declaration is not a use',
            );
            const inUse = await ws.refs('bump');
            assert.ok(
                inUse.some((h) => h.symbol === 'makeCounter.plus'),
                JSON.stringify(inUse),
            );
        });

        it('reads a named thing — symbols, nested symbols, files, ranges — with an `at` token', async () => {
            const { ws } = await setup();
            const r = await ws.read('add');
            assert.ok(r.ok && /^function add/.test(r.body) && /}$/.test(r.body.trim()), JSON.stringify(r));
            assert.match(r.at, /^[0-9A-F]{40}$/);

            const nested = await ws.read('makeCounter.plus');
            assert.ok(nested.ok && /return bump\(by\)/.test(nested.body) && nested.lineStart === nested.lineEnd);
            const bare = await ws.read('bump');
            assert.equal(bare.path, 'makeCounter.bump');
            assert.equal(bare.parent, 'makeCounter');

            const u = await ws.read('MARKER');
            assert.ok(u.ok && u.body.includes('—…é'), 'bytes survive the round trip');

            const whole = await ws.read('lib/math.js');
            assert.ok(whole.ok && whole.body.includes('function add') && whole.lines > 5 && whole.at);
            const ranged = await ws.read('lib/math.js:5-7');
            assert.ok(
                ranged.ok &&
                    ranged.lineStart === 5 &&
                    ranged.body.startsWith('function add') &&
                    !ranged.body.includes('TAX_RATE'),
            );
            assert.equal((await ws.read('lib/math.js', { from: 5, to: 7 })).body, ranged.body);
            assert.equal((await ws.read('lib/math.js#5-7')).body, ranged.body, 'path#from-to reads the same range');
            assert.equal(
                (await ws.read('math.js#5-7')).body,
                ranged.body,
                'a bare basename with a # range still lands',
            );

            const small = await ws.readFile('lib/math.js', { maxLines: 3 });
            assert.ok(small.truncated && small.body.split('\n').length === 3 && whole.body.startsWith(small.body));
            assert.match(small.more, /^lib\/math\.js:4-\d+$/);
        });

        it('misses say where to go next', async () => {
            const { ws } = await setup();
            const ghost = await ws.read('lib/other-math.js');
            assert.ok(
                !ghost.ok && ghost.noSuchFile && /no file "lib\/other-math\.js"/.test(ghost.reason),
                ghost.reason,
            );
            assert.match(ghost.reason, /lib\/math\.js/, 'points at the file sharing a name token');
            const alone = await ws.read('lib/zzzqqq.js');
            assert.ok(alone.noSuchFile && /structure\(\)/.test(alone.reason));
            const badSuffix = await ws.readFile('lib/math.js#nosuchsymbol');
            assert.ok(
                !badSuffix.ok &&
                    /IS indexed/.test(badSuffix.reason) &&
                    /lib\/math\.js:<from>-<to>/.test(badSuffix.reason),
            );
            const miss = await ws.read('TAX_RATE_IS_MENTIONED_NOWHERE');
            assert.ok(!miss.ok && /check the spelling|read the file|find\(\)/.test(miss.reason));
            const asText = await ws.read('shorthand');
            assert.ok(
                !asText.ok && asText.mentions.some((m) => m.rel === 'lib/math.js'),
                'text-only name reports where it is',
            );
            const noOutline = await ws.outline('lib/does-not-exist.js');
            assert.ok(
                !noOutline.ok &&
                    /^there is no file/.test(noOutline.reason) &&
                    !/no indexed file matching/.test(noOutline.reason),
            );
            const lit = await ws.mentions('TAX_RATE = 0.17', { limit: 5 });
            assert.deepEqual(lit, [{ rel: 'lib/math.js', line: 9 }]);
            assert.deepEqual(await ws.mentions('ThisStringIsNowhereInTheFixture'), []);
        });

        it('outlines by symbol, and by chunk when a file has none', async () => {
            const { ws, writeDisk } = await setup();
            const out = await ws.outline('lib/math.js');
            assert.ok(out.ok && out.by === 'symbol' && out.symbols.length > 0 && out.at);
            assert.ok(out.symbols.every((s) => s.lineStart >= 1 && s.lineEnd >= s.lineStart));
            writeDisk(
                'lib/template.js',
                [
                    'export default `',
                    '  <div class="rail">',
                    '    <span>one</span>',
                    '  </div>',
                    '',
                    '  <div class="foot">',
                    '    <span>two</span>',
                    '  </div>',
                    '`;',
                    '',
                ].join('\n'),
            );
            await ws.sync();
            const tpl = await ws.outline('lib/template.js');
            assert.ok(tpl.ok && tpl.by === 'chunk' && tpl.symbols.length > 0, JSON.stringify(tpl));
            assert.ok(
                tpl.symbols.every(
                    (s, i, a) => s.lineEnd >= s.lineStart && (i === 0 || s.lineStart >= a[i - 1].lineStart),
                ),
            );
            assert.ok(tpl.symbols.every((s) => s.path.startsWith('lib/template.js:')));
        });

        it('verify-on-read: an out-of-band edit is seen by the very next read; a deleted file is gone', async () => {
            const { ws, readDisk, writeDisk, file } = await setup();
            const before = readDisk('lib/math.js');
            const first = await ws.read('lib/math.js');
            writeDisk('lib/math.js', before.replace('const TAX_RATE = 0.17;', 'const TAX_RATE = 0.25;'));
            const second = await ws.read('lib/math.js');
            assert.ok(second.body.includes('0.25') && !second.body.includes('0.17'));
            assert.notEqual(first.at, second.at, 'the token moves with the file');
            const sym = await ws.read('TAX_RATE');
            assert.ok(sym.ok && sym.body.includes('0.25'));
            writeDisk('lib/math.js', before);
            assert.equal((await ws.read('lib/math.js')).at, first.at, 'reverting is seen too');

            writeDisk('lib/doomed.js', 'const gone = 1;\nmodule.exports = { gone };\n');
            await ws.sync();
            assert.ok((await ws.read('lib/doomed.js')).ok);
            fs.unlinkSync(file('lib/doomed.js'));
            assert.equal((await ws.read('lib/doomed.js')).ok, false, 'not served from cache');
        });

        it('`at` tokens: a stale or missing token is refused, with the current one', async () => {
            const { ws } = await setup();
            const fresh = await ws.read('scale');
            const stale = await ws.edit('scale', 'const scale = (v, by) => v * by;', { at: 'A'.repeat(40) });
            assert.ok(!stale.ok && stale.stale === true);
            assert.equal(stale.at, fresh.at, 'the refusal hands back the current token');
            const noAt = await ws.edit('scale', 'const scale = (v, by) => v * by * 2;');
            assert.ok(!noAt.ok && !noAt.outcome);
            const staleRange = await ws.readAt('lib/math.js', 1, 2, 'B'.repeat(40));
            assert.ok(!staleRange.ok && staleRange.stale === true);
            const good = await ws.edit('scale', 'const scale = (v, by) => v * by * 1;', { at: fresh.at });
            assert.ok(good.ok, JSON.stringify(good));
        });

        it(
            '`export const` arrow: find → read → edit by name replaces exactly its declaration',
            { skip: fac.name !== 'localFs' && 'localFs only' },
            async () => {
                const { ws, readDisk, writeDisk } = await setup();
                const head = "import { x } from './x.mjs';\n\n";
                const decl = [
                    '// Doubles a number, exported as a const arrow.',
                    'export const doubleIt = (n) => {',
                    '    return n * 2;',
                    '};',
                ].join('\n');
                const tail = '\n\nexport const other = 1;\n';
                writeDisk('lib/exported.mjs', head + decl + tail);
                await ws.sync();

                const hits = await ws.find('doubleIt');
                const hit = hits.find((h) => h.name === 'doubleIt');
                assert.ok(hit, JSON.stringify(hits));
                assert.equal(hit.file, 'lib/exported.mjs');
                assert.match(hit.signature || '', /doubleIt\(n\)/);

                const r = await ws.read('doubleIt');
                assert.ok(r.ok, JSON.stringify(r));
                const declOnly = decl.slice(decl.indexOf('export const'));
                assert.equal(r.body.trim(), declOnly);

                const replacement = 'export const doubleIt = (n) => n + n;';
                const e = await ws.edit('doubleIt', replacement, { at: r.at });
                assert.ok(e.ok, JSON.stringify(e));
                assert.equal(
                    readDisk('lib/exported.mjs'),
                    head + decl.replace(declOnly, replacement) + tail,
                    'only the declaration moved; the doc, the import and the neighbour are untouched',
                );
                const again = await ws.read('doubleIt');
                assert.equal(again.body.trim(), replacement);
            },
        );

        it('editBatch: one snapshot, receipt, diff; old token then stale; overlap and cross-file refused', async () => {
            const { ws, readDisk } = await setup();
            const at1 = (await ws.read('lib/math.js')).at;
            const batch = await ws.editBatch({
                at: at1,
                edits: [
                    { target: 'lib/math.js:1-1', body: "'use strict'; // batch-edited" },
                    { target: 'TAX_RATE', body: 'const TAX_RATE = 0.20;' },
                ],
            });
            assert.ok(batch.ok && batch.outcome === 'ok', JSON.stringify(batch));
            assert.equal(batch.edits.length, 2);
            assert.equal(batch.readbackHash, batch.candidateHash);
            assert.ok(Array.isArray(batch.diff) && batch.diff.length >= 1);
            const after = readDisk('lib/math.js');
            assert.ok(
                after.includes('batch-edited') &&
                    after.includes('TAX_RATE = 0.20') &&
                    after.includes('function add(a, b)'),
            );

            const again = await ws.edit('scale', 'const scale = (v, by) => v * by;', { at: at1 });
            assert.ok(!again.ok && again.stale === true, 'the pre-batch token is stale after the batch');

            const at2 = (await ws.read('lib/math.js')).at;
            const before2 = readDisk('lib/math.js');
            const overlap = await ws.editBatch({
                at: at2,
                edits: [
                    { target: 'lib/math.js:1-3', body: 'x' },
                    { target: 'lib/math.js:2-2', body: 'y' },
                ],
            });
            assert.ok(!overlap.ok && overlap.overlap === true);
            assert.equal(readDisk('lib/math.js'), before2);
            const cross = await ws.editBatch({
                at: at2,
                edits: [
                    { target: 'lib/math.js:1-1', body: 'x' },
                    { target: 'lib/factory.js:1-1', body: 'y' },
                ],
            });
            assert.ok(!cross.ok && cross.crossFile === true);
            const oob = await ws.edit('lib/math.js:99-120', 'x', { at: at2 });
            assert.ok(!oob.ok && /outside it/.test(oob.reason), 'a range past the end is refused, not clamped');
        });

        it('a candidate with a NEW diagnostic is refused before the file is touched', async () => {
            const { ws, readDisk } = await setup();
            const before = readDisk('lib/math.js');
            const scaleAt = (await ws.read('scale')).at;
            const bad = await ws.edit('scale', 'const scale = (v, by) => v * by;;;\nfunction {{{ broken', {
                at: scaleAt,
            });
            assert.ok(!bad.ok && !bad.outcome && bad.newDiagnostics.length > 0, JSON.stringify(bad));
            assert.equal(readDisk('lib/math.js'), before, 'byte-identical after the refusal');

            // Editing inside a closure: the region is the method's exact
            // offsets — no indentation, no trailing comma.
            const plusAt = (await ws.read('makeCounter.plus')).at;
            const ne = await ws.edit('makeCounter.plus', 'plus(by) { return bump(by) + 0; }', { at: plusAt });
            assert.ok(ne.ok && ne.outcome === 'ok', JSON.stringify(ne));
            assert.ok(readDisk('lib/factory.js').includes('value() { return n; },'), 'the sibling survives');
            const valueAt = (await ws.read('makeCounter.value')).at;
            const oldStyle = await ws.edit('makeCounter.value', '        value() { return n; },', { at: valueAt });
            assert.ok(
                !oldStyle.ok && !oldStyle.outcome && oldStyle.newDiagnostics.length > 0,
                'doubled punctuation is refused',
            );

            // An unbalanced template is refused by template-balance.
            const { writeDisk } = { writeDisk: (rel, t) => fs.writeFileSync(path.join(ws.access.root, rel), t) };
            writeDisk(
                'lib/view.js',
                [
                    "'use strict';",
                    'module.exports = {',
                    '    template: `',
                    '      <div class="head">',
                    '        <if :="open">',
                    '          <span class="label">title</span>',
                    '        </if>',
                    '      </div>`,',
                    '};',
                    '',
                ].join('\n'),
            );
            await ws.sync();
            const tplBefore = readDisk('lib/view.js');
            const tplRead = await ws.read('lib/view.js:4-7');
            const unbalanced = await ws.edit(
                'lib/view.js:4-7',
                [
                    '      <div class="head">',
                    '      <div class="head">',
                    '        <if :="open">',
                    '          <span class="label">title</span>',
                ].join('\n'),
                { at: tplRead.at },
            );
            assert.ok(!unbalanced.ok && unbalanced.newDiagnostics.some((d) => d.source === 'template-balance'));
            assert.equal(readDisk('lib/view.js'), tplBefore);
        });

        it('a symbol shifted out of band is edited at its CURRENT position; CRLF and non-ASCII survive', async () => {
            const { ws, readDisk, writeDisk } = await setup();
            writeDisk('lib/math.js', '// inserted line one\n// inserted line two\n' + readDisk('lib/math.js'));
            const moved = await ws.read('scale');
            assert.ok(moved.ok && moved.lineStart > 11);
            const r = await ws.edit('scale', 'const scale = (v, by) => v * by + 100;', { at: moved.at });
            assert.ok(r.ok, JSON.stringify(r));
            const after = readDisk('lib/math.js');
            assert.ok(
                after.startsWith('// inserted line one\n// inserted line two\n') && after.includes('v * by + 100'),
            );

            writeDisk(
                'lib/crlf.js',
                "'use strict';\r\nconst greeting = 'héllo — wörld';\r\nfunction two() { return 2; }\r\n",
            );
            await ws.sync();
            const crlfAt = (await ws.read('lib/crlf.js#two')).at;
            const c = await ws.edit('lib/crlf.js#two', 'function two() { return 22; }', { at: crlfAt });
            assert.ok(c.ok, JSON.stringify(c));
            const crlf = readDisk('lib/crlf.js');
            assert.ok(
                crlf
                    .split('\n')
                    .slice(0, -1)
                    .every((l) => l.endsWith('\r')),
                JSON.stringify(crlf),
            );
            assert.ok(crlf.includes("const greeting = 'héllo — wörld';\r\n") && crlf.includes('return 22;'));
        });

        it('moduleKind comes from the controlling manifest; a baseline error left in place still commits', async () => {
            const { ws, writeDisk } = await setup();
            writeDisk('esm-project/package.json', JSON.stringify({ type: 'module' }));
            writeDisk('esm-project/index.js', 'const value = 1;\nconsole.log(value);\n');
            await ws.sync();
            const r1 = await ws.read('esm-project/index.js:2-2');
            const underModule = await ws.edit('esm-project/index.js:2-2', 'await Promise.resolve(value);', {
                at: r1.at,
            });
            assert.ok(underModule.ok && underModule.moduleKind === 'module', JSON.stringify(underModule));
            const revertAt = (await ws.read('esm-project/index.js:2-2')).at;
            await ws.edit('esm-project/index.js:2-2', 'console.log(value);', { at: revertAt });
            writeDisk('esm-project/package.json', JSON.stringify({ type: 'commonjs' }));
            const r2 = await ws.read('esm-project/index.js:2-2');
            const underCjs = await ws.edit('esm-project/index.js:2-2', 'await Promise.resolve(value);', { at: r2.at });
            assert.ok(!underCjs.ok && underCjs.newDiagnostics.length > 0, 'the manifest was re-read, not cached');

            writeDisk(
                'lib/brokenbase.js',
                "'use strict';\nfunction ok1() { return 1; }\nfunction broken( {\n    return 2;\n}\n",
            );
            await ws.sync();
            const b1 = await ws.read('lib/brokenbase.js:3-3');
            assert.ok(b1.ok, 'a range reads even though the file does not parse');
            const same = await ws.edit('lib/brokenbase.js:3-3', 'function broken( {', { at: b1.at });
            assert.ok(same.ok && same.baselineBroken === true && same.remaining.length > 0, JSON.stringify(same));
            const b2 = await ws.read('lib/brokenbase.js:3-3');
            const swapped = await ws.edit('lib/brokenbase.js:3-3', 'function broken( ) {{{', { at: b2.at });
            assert.ok(!swapped.ok && swapped.newDiagnostics.length > 0, 'a DIFFERENT error is new');
        });

        it('writeWholeFile: create is exclusive, replace goes through editBatch', async () => {
            const { ws, readDisk, file } = await setup();
            const text = "'use strict';\nconst x = 1;\nmodule.exports = { x };\n";
            const created = await ws.writeWholeFile('lib/brand-new.js', text, { create: true });
            assert.ok(created.ok && created.outcome === 'ok', JSON.stringify(created));
            assert.equal(readDisk('lib/brand-new.js'), text);
            assert.ok((await ws.read('lib/brand-new.js')).ok, 'an own write is visible without a sync');
            const again = await ws.writeWholeFile('lib/brand-new.js', 'anything', { create: true });
            assert.ok(!again.ok && !again.outcome && /already exists/.test(again.reason));
            assert.equal(readDisk('lib/brand-new.js'), text);
            const noHash = await ws.writeWholeFile('lib/brand-new.js', 'replacement');
            assert.ok(!noHash.ok && !noHash.outcome);
            const at1 = (await ws.read('lib/brand-new.js')).at;
            const replaced = await ws.writeWholeFile(
                'lib/brand-new.js',
                "'use strict';\nconst x = 2;\nmodule.exports = { x };\n",
                {
                    expectedHash: at1,
                },
            );
            assert.ok(replaced.ok && replaced.outcome === 'ok', JSON.stringify(replaced));
            assert.ok(readDisk('lib/brand-new.js').includes('const x = 2;'));
            const staleReplace = await ws.writeWholeFile('lib/brand-new.js', 'x', { expectedHash: at1 });
            assert.ok(!staleReplace.ok && staleReplace.stale === true);
            const brokenCreate = await ws.writeWholeFile('lib/another-new.js', 'function {{{ broken', { create: true });
            assert.ok(!brokenCreate.ok && !brokenCreate.outcome);
            assert.equal(fs.existsSync(file('lib/another-new.js')), false);
            const escape = await ws.writeWholeFile('../outside.js', 'const a = 1;\n', { create: true });
            assert.equal(escape.ok, false);
        });

        it('ambiguity is reported, never guessed; syntaxCheck runs on the target', async () => {
            const { ws, writeDisk } = await setup();
            writeDisk('lib/other.js', "'use strict';\nfunction add(x) { return x; }\nmodule.exports = { add };\n");
            await ws.sync();
            const amb = await ws.read('add');
            assert.ok(!amb.ok && amb.ambiguous && amb.candidates.length === 2, JSON.stringify(amb));
            const q = await ws.read('other.js#add');
            assert.ok(q.ok && /return x;/.test(q.body));
            // Needs `node` on the TARGET; a non-interactive ssh shell may not
            // have it on PATH, which is an honest failure, not a crash.
            const chk = await ws.syntaxCheck('lib/factory.js');
            if (fac.name === 'localFs') assert.equal(chk.ok, true, JSON.stringify(chk));
            else assert.ok(chk.ok === true || /node: command not found/.test(chk.out), JSON.stringify(chk));
        });

        it('packages: the declared dependency surface, from the installed version', async () => {
            const { ws } = await setup();
            const deps = await ws.packages();
            assert.ok(Array.isArray(deps) && deps.length === 1 && deps[0].name === 'widget', JSON.stringify(deps));
            const widget = deps[0];
            assert.equal(widget.version, '2.3.1');
            assert.ok(widget.surface.includes('build') && widget.surface.includes('Widget'));
            assert.ok(widget.members.includes('Widget.render') && widget.members.includes('Extras.polish'));
            assert.ok(!widget.members.some((m) => /\.(width|height)$/.test(m)));
            const one = await ws.package('widget');
            assert.equal(one.version, '2.3.1');
            assert.ok((await ws.package('nope')).error);
        });
    });
}
