'use strict';
// grep and glob: rg over a workspace — every matching line with its text,
// case/regex/glob/path filters, context, caps — through each facade (localFs,
// and the shell facade over a local bash, which also exercises the target-side
// pre-filter), with a real okdb store behind it, including a workspace whose
// index is not built yet: the index is an accelerator, never a gate.

const { describe, it, after } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const OKDB = require('@kedem/okdb');
const { openWorkspace } = require('../src/workspace');
const { openStore } = require('../src/store');
const { localFs, shell } = require('../src/access');
const { createTools } = require('../src/tools');
const { globToRegExp, pathMatcher, compileMatcher, matchText, indexTerms } = require('../src/grep');
const { writeFixture, tmpRoot } = require('./fixtures/code-fixture');

// Files on top of the shared fixture: several matches per file, a test file,
// prose, a secret, a CRLF file, and a pruned build directory.
const APP = [
    "'use strict';", //                                      1
    '// Hello from the app.', //                             2
    'function hello(name) {', //                             3
    "    return 'hello ' + name;", //                        4
    '}', //                                                  5
    '', //                                                   6
    'function HELLO_ALL(names) {', //                        7
    '    return names.map(hello);', //                       8
    '}', //                                                  9
    '', //                                                   10
    'module.exports = { hello, HELLO_ALL };', //             11
    '',
].join('\n');
function writeExtra(root) {
    const w = (rel, text) => {
        fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
        fs.writeFileSync(path.join(root, rel), text);
    };
    w('src/app.js', APP);
    w('src/app.test.js', "const { hello } = require('./app');\nhello('test');\n");
    w('docs/notes.md', '# Notes\n\nSay Hello World to everyone.\n');
    w('src/crlf.js', 'const a = 1;\r\nconst helloCr = 2;\r\n');
    w('.env', 'HELLO_TOKEN=secret-hello\n');
    w('dist/bundle.js', 'hello from the bundle\n');
}

const FACADES = [
    { name: 'localFs', make: (root) => localFs(root) },
    { name: 'shell/bash', make: (root) => shell({ root, run: shell.localRunner('bash') }) },
];

describe('grep helpers', () => {
    it('globs: name at any depth, anchored paths, **, braces, classes, directories', () => {
        const t = (g, p) => pathMatcher({ glob: g })(p);
        assert.ok(t('*.js', 'a.js') && t('*.js', 'src/deep/a.js') && !t('*.js', 'a.mjs'));
        assert.ok(t('src/*.js', 'src/a.js') && !t('src/*.js', 'src/x/a.js') && !t('src/*.js', 'lib/src/a.js'));
        assert.ok(t('src/**/*.js', 'src/a.js') && t('src/**/*.js', 'src/x/y/a.js'));
        assert.ok(t('**/test/*.js', 'test/a.js') && t('**/test/*.js', 'pkg/test/a.js'));
        assert.ok(t('*.{js,ts}', 'a.ts') && !t('*.{js,ts}', 'a.css'));
        assert.ok(t('?.js', 'a.js') && !t('?.js', 'ab.js'));
        assert.ok(t('[ab].js', 'b.js') && !t('[!ab].js', 'b.js'));
        // A glob matching a DIRECTORY takes its subtree.
        assert.ok(t('lib', 'lib/math.js') && t('lib', 'x/lib/y.js') && !t('lib', 'libs/a.js'));
        assert.ok(t('./src/', 'src/a.js'));
        // Excludes, alone or with includes.
        const m = pathMatcher({ glob: ['src/**', '!*.test.js'] });
        assert.ok(m('src/app.js') && !m('src/app.test.js') && !m('lib/a.js'));
        assert.ok(pathMatcher({ glob: '!dist' })('a.js') && !pathMatcher({ glob: '!dist' })('dist/a.js'));
        // paths: directories or files, not string prefixes.
        const p = pathMatcher({ paths: ['lib', 'src/app.js'] });
        assert.ok(p('lib/math.js') && p('src/app.js') && !p('libs/x.js') && !p('src/app.test.js'));
        assert.equal(pathMatcher({}), null);
        assert.equal(pathMatcher({ paths: ['.'] }), null);
        assert.ok(globToRegExp('a.b').test('a.b') && !globToRegExp('a.b').test('axb'));
    });

    it('matcher: literal, case modes, regex, bad patterns', () => {
        assert.equal(compileMatcher('Foo').test('a foo'), 2);
        assert.equal(compileMatcher('Foo', { caseSensitive: true }).test('a foo'), -1);
        assert.equal(compileMatcher('foo', { caseSensitive: 'smart' }).test('FOO'), 0);
        assert.equal(compileMatcher('Foo', { caseSensitive: 'smart' }).test('foo'), -1);
        assert.equal(compileMatcher('a.b').test('axb'), -1, 'a literal dot is a dot');
        assert.equal(compileMatcher('a.b', { regex: true }).test('axb'), 0);
        assert.equal(compileMatcher('ab', { regex: true }).literal, 'ab', 'a meta-free regex is a literal');
        assert.equal(compileMatcher('é', {}).test('É'), 0);
        for (const bad of ['', 'a\nb']) assert.throws(() => compileMatcher(bad), { code: 'GREP_BAD_PATTERN' });
        assert.throws(() => compileMatcher('(', { regex: true }), { code: 'GREP_BAD_PATTERN' });
        const r = matchText('x\nfoo\ny\nfoo\n', compileMatcher('foo'), { context: 1, maxPerFile: 1 });
        assert.equal(r.count, 2);
        assert.deepEqual(r.matches, [{ line: 2, col: 1, text: 'foo', before: ['x'], after: ['y'] }]);
        const last = matchText('a\nfoo\n', compileMatcher('foo'), { after: 3 });
        assert.deepEqual(last.matches[0].after, [], 'no phantom line after a trailing newline');
    });

    it('index terms: only what every containing file must hold', () => {
        const sw = ['the', 'then', 'there', 'this'];
        assert.deepEqual(indexTerms('twice', { stopwords: sw }), [], 'one open word: an infix, no term');
        assert.deepEqual(indexTerms('TAX_RATE = 0.17', { stopwords: sw }), ['17']);
        assert.deepEqual(indexTerms("toggleSection('claude')", { stopwords: sw }), ['claude']);
        assert.deepEqual(indexTerms('x.th', { stopwords: sw }), [], 'a stopword starts with it: unindexed');
        assert.deepEqual(indexTerms('x.the', { stopwords: sw }), [], 'a stopword itself');
        assert.deepEqual(indexTerms('x.a', { stopwords: sw }), [], 'too short to be indexed');
        assert.deepEqual(indexTerms('a.café.b', { stopwords: sw }), [], 'non-ASCII runs are not trusted');
    });
});

for (const fac of FACADES) {
    describe(`grep and glob over ${fac.name}`, () => {
        const cleanups = [];
        let n = 0;
        after(async () => {
            for (const c of cleanups) await c().catch(() => {});
        });

        async function setup({ store = true, open = true } = {}) {
            const { base, root } = tmpRoot('grep');
            writeFixture(root);
            writeExtra(root);
            const access = fac.make(root);
            const id = `grep-${++n}`;
            let db = null;
            if (store) {
                db = new OKDB(path.join(base, 'okdb'), { auth: { open: true } });
                await db.open();
            }
            const st = store ? await openStore({ db, id, access }) : null;
            const ws = await openWorkspace({ id, access, store: st });
            if (open) {
                await ws.refresh();
                await ws.flush();
            }
            cleanups.push(async () => {
                await ws.close();
                if (db) await db.close();
                fs.rmSync(base, { recursive: true, force: true });
            });
            const write = (rel, text) => fs.writeFileSync(path.join(root, rel), text);
            return { ws, root, write, access };
        }

        it('every matching line per file, with text and column, in path order', async () => {
            const { ws } = await setup();
            const r = await ws.grep('hello');
            assert.equal(r.ok, true);
            assert.equal(r.ignoreCase, true, 'case-insensitive by default');
            const where = r.matches.map((m) => `${m.file}:${m.line}`);
            assert.deepEqual(where, [
                'docs/notes.md:3',
                'src/app.js:2',
                'src/app.js:3',
                'src/app.js:4',
                'src/app.js:7',
                'src/app.js:8',
                'src/app.js:11',
                'src/app.test.js:1',
                'src/app.test.js:2',
                'src/crlf.js:2',
            ]);
            const m = r.matches.find((x) => x.file === 'src/app.js' && x.line === 4);
            assert.equal(m.text, "    return 'hello ' + name;");
            assert.equal(m.col, 13);
            assert.equal(r.matches.find((x) => x.file === 'src/crlf.js').text, 'const helloCr = 2;', 'CR dropped');
            assert.deepEqual(
                r.files.map((f) => [f.file, f.count]),
                [
                    ['docs/notes.md', 1],
                    ['src/app.js', 6],
                    ['src/app.test.js', 2],
                    ['src/crlf.js', 1],
                ],
            );
            assert.match(r.files[1].at, /^[0-9A-F]{40}$/);
            assert.equal(r.truncated, false);
            // Never: a secret, a pruned directory, a dot-directory, gitignored state.
            for (const f of ['.env', 'dist/bundle.js', '.cache/blob.js', 'session-state/key-1.json'])
                assert.ok(!r.files.some((x) => x.file === f), f);
            if (fac.name === 'shell/bash') assert.equal(r.via, 'target', 'the target pre-filtered the literal');
        });

        it('case: sensitive and smart', async () => {
            const { ws } = await setup();
            const exact = await ws.grep('HELLO', { caseSensitive: true });
            assert.deepEqual(
                exact.matches.map((m) => `${m.file}:${m.line}`),
                ['src/app.js:7', 'src/app.js:11'],
            );
            const smart = await ws.grep('Hello', { caseSensitive: 'smart' });
            assert.deepEqual(
                smart.matches.map((m) => `${m.file}:${m.line}`),
                ['docs/notes.md:3', 'src/app.js:2'],
            );
            const smartLower = await ws.grep('hello world', { caseSensitive: 'smart' });
            assert.equal(smartLower.matches.length, 1);
        });

        it('regex: JS syntax, anchors per line; a bad one throws GREP_BAD_PATTERN', async () => {
            const { ws } = await setup();
            const r = await ws.grep('^function \\w+\\(', { regex: true });
            assert.equal(r.regex, true);
            assert.deepEqual(
                r.matches.map((m) => `${m.file}:${m.line}`),
                ['lib/factory.js:2', 'lib/math.js:5', 'src/app.js:3', 'src/app.js:7'],
            );
            assert.notEqual(r.via, 'target', 'a regex is never decided by the target grep');
            const alt = await ws.grep('TAX_RATE|makeCounter\\(', { regex: true, caseSensitive: true });
            assert.equal(alt.files.length, 2);
            await assert.rejects(() => ws.grep('(unclosed', { regex: true }), { code: 'GREP_BAD_PATTERN' });
        });

        it('glob and paths filters', async () => {
            const { ws } = await setup();
            const md = await ws.grep('hello', { glob: '*.md' });
            assert.deepEqual(
                md.files.map((f) => f.file),
                ['docs/notes.md'],
            );
            const noTests = await ws.grep('hello', { glob: ['src/**', '!*.test.js'] });
            assert.deepEqual(
                noTests.files.map((f) => f.file),
                ['src/app.js', 'src/crlf.js'],
            );
            const under = await ws.grep('module.exports', { paths: ['lib'] });
            assert.deepEqual(
                under.files.map((f) => f.file),
                ['lib/factory.js', 'lib/math.js', 'lib/unicode.js'],
            );
            assert.equal(under.searched, 4, 'only lib/ was searched');
            const file = await ws.grep('hello', { paths: ['src/app.test.js'] });
            assert.deepEqual(
                file.files.map((f) => f.file),
                ['src/app.test.js'],
            );
        });

        it('context lines: before/after, clipped at file edges', async () => {
            const { ws } = await setup();
            const r = await ws.grep('HELLO_ALL(names)', { context: 2 });
            assert.equal(r.matches.length, 1);
            const m = r.matches[0];
            assert.equal(m.line, 7);
            assert.deepEqual(m.before, ['}', '']);
            assert.deepEqual(m.after, ['    return names.map(hello);', '}']);
            const first = (await ws.grep("'use strict'", { paths: ['src/app.js'], before: 3, after: 1 })).matches[0];
            assert.deepEqual(first.before, []);
            assert.deepEqual(first.after, ['// Hello from the app.']);
            const lastLine = (await ws.grep('module.exports = { hello', { after: 5 })).matches[0];
            assert.deepEqual(lastLine.after, []);
        });

        it('caps: per file, total, files — and the answer says so', async () => {
            const { ws } = await setup();
            const per = await ws.grep('hello', { maxPerFile: 2 });
            const app = per.files.find((f) => f.file === 'src/app.js');
            assert.deepEqual([app.count, app.shown], [6, 2]);
            assert.equal(per.matches.filter((m) => m.file === 'src/app.js').length, 2);
            assert.equal(per.truncated, true);
            const total = await ws.grep('hello', { maxMatches: 3 });
            assert.equal(total.matches.length, 3);
            assert.equal(total.truncated, true);
            assert.ok(total.unsearched >= 1, JSON.stringify(total));
            const files = await ws.grep('hello', { maxFiles: 1 });
            assert.deepEqual(
                files.files.map((f) => f.file),
                ['docs/notes.md'],
            );
            assert.equal(files.truncated, true);
        });

        it('literal pre-filters stay complete: a file edited outside the workspace is found', async () => {
            const { ws, write } = await setup();
            const before = await ws.grep('TAX_RATE = 0.17');
            assert.deepEqual(
                before.matches.map((m) => `${m.file}:${m.line}`),
                ['lib/math.js:9'],
            );
            if (fac.name === 'localFs') {
                assert.equal(before.via, 'index', 'the full-text index narrowed the literal');
                assert.equal(before.candidates, 1);
            }
            // Changed on disk since the index saw it: no sync, no own write.
            write('lib/factory.js', "'use strict';\n// was TAX_RATE = 0.17 once\nmodule.exports = {};\n");
            const now = await ws.grep('TAX_RATE = 0.17');
            assert.deepEqual(
                now.matches.map((m) => `${m.file}:${m.line}`),
                ['lib/factory.js:2', 'lib/math.js:9'],
            );
            // An open word has no sound index term: every file is read.
            const open = await ws.grep('twice');
            assert.ok(['scan', 'target'].includes(open.via), open.via);
            assert.deepEqual(
                open.matches.map((m) => `${m.file}:${m.line}`),
                ['lib/math.js:13', 'lib/math.js:14', 'lib/math.js:16'],
            );
        });

        it('without a store: the same answers from the working set', async () => {
            const { ws } = await setup({ store: false });
            const r = await ws.grep('hello', { glob: '*.md' });
            assert.deepEqual(
                r.matches.map((m) => `${m.file}:${m.line}`),
                ['docs/notes.md:3'],
            );
        });

        it('a workspace whose index is not built yet answers directly from the facade', async () => {
            const { ws } = await setup({ open: false });
            const r = await ws.grep('hello', { glob: 'src/**' });
            assert.equal(r.indexed, false, 'answered before the index was ready');
            assert.deepEqual(
                r.files.map((f) => [f.file, f.count]),
                [
                    ['src/app.js', 6],
                    ['src/app.test.js', 2],
                    ['src/crlf.js', 1],
                ],
            );
            assert.match(r.files[0].at, /^[0-9A-F]{40}$/);
            const g = await ws.glob('*.md');
            assert.deepEqual(
                g.files.map((f) => f.file),
                ['docs/notes.md'],
            );
            // Meanwhile the walk it skipped was started; once it lands the
            // index answers, identically.
            await ws.refresh();
            await ws.flush();
            const again = await ws.grep('hello', { glob: 'src/**' });
            assert.equal(again.indexed, true);
            assert.deepEqual(again.matches, r.matches);
            assert.equal((await ws.glob('*.md')).via, 'index');
        });

        it('a grep during the first (cold) walk does not wait for it', async () => {
            const { ws } = await setup({ open: false });
            const walking = ws.refresh();
            const r = await ws.grep('makeCounter', { caseSensitive: true });
            assert.equal(r.indexed, false);
            assert.deepEqual(
                r.matches.map((m) => `${m.file}:${m.line}`),
                ['lib/factory.js:2', 'lib/factory.js:12'],
            );
            await walking;
        });

        it('glob: paths over the index, excludes, directories, limit', async () => {
            const { ws } = await setup();
            const js = await ws.glob('*.js');
            assert.deepEqual(
                js.files.map((f) => f.file),
                ['lib/factory.js', 'lib/math.js', 'lib/unicode.js', 'src/app.js', 'src/app.test.js', 'src/crlf.js'],
            );
            assert.equal(js.via, 'index');
            assert.equal(js.files[1].lines, 17);
            const lib = await ws.glob(['lib', '!*.mjs']);
            assert.deepEqual(
                lib.files.map((f) => f.file),
                ['lib/factory.js', 'lib/math.js', 'lib/unicode.js'],
            );
            const lim = await ws.glob('**/*.js', { limit: 2 });
            assert.equal(lim.total, 6);
            assert.equal(lim.files.length, 2);
            assert.equal(lim.truncated, true);
            assert.equal((await ws.glob('')).ok, false);
        });

        it('tools: code_grep renders rg-style with context; code_glob lists; old args still work', async () => {
            const { ws } = await setup();
            const tools = createTools({ workspace: async () => ws, workspaces: () => [{ id: ws.id }] });
            const out = await tools.render('code_grep', { pattern: 'hello', glob: ['src/app.js'], context: 1 });
            assert.match(out, /^code_grep "hello" in grep-\d+ — 6 matching lines in 1 file/);
            assert.match(out, /── src\/app\.js \(6 matching lines; at=[0-9A-F]{40}\)/);
            assert.match(out, /\nsrc\/app\.js:4: {5}return 'hello ' \+ name;/);
            assert.match(out, /\nsrc\/app\.js-5- \}/);
            assert.match(out, /\nsrc\/app\.js-10- \n/);
            assert.doesNotMatch(out, /\n--\n/, 'one contiguous stretch');
            const gaps = await tools.render('code_grep', { pattern: 'add', paths: 'lib/math.js', context: 1 });
            assert.match(gaps, /\nlib\/math\.js-6- {5}return a \+ b;\n--\nlib\/math\.js-12- \n/, 'separated stretches');
            const flat = await tools.render('code_grep', { pattern: 'add', paths: 'lib/math.js' });
            assert.doesNotMatch(flat, /\n--\n/, 'no separators without context');
            // The historical form: { text, limit (files) }.
            const old = await tools.render('code_grep', { text: 'TAX_RATE = 0.17', limit: 5 });
            assert.match(old, /lib\/math\.js:9: const TAX_RATE = 0\.17;/);
            const regex = await tools.render('code_grep', { pattern: 'a +b', regex: true });
            assert.match(regex, /no line matches that expression/);
            assert.match(await tools.render('code_grep', { pattern: '(', regex: true }), /invalid regular expression/);
            const capped = await tools.render('code_grep', { pattern: 'hello', max_per_file: 1 });
            assert.match(capped, /6 matching lines, showing 1/);
            const globbed = await tools.render('code_glob', { pattern: 'lib/*.js,!lib/unicode.js' });
            assert.match(globbed, /— 2 files:\n {2}lib\/factory\.js {2}\(13 lines\)\n {2}lib\/math\.js {2}\(17 lines\)/);
            assert.match(await tools.render('code_glob', { pattern: 'nothing/**' }), /no file in grep-\d+ matches/);
            assert.ok(tools.schemas().some((s) => s.name === 'code_glob'));
        });
    });
}
