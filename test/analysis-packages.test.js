'use strict';
// The dependency SURFACE, read through the async access facade — "does this
// library have X", asked of the INSTALLED version, without indexing a
// dependency tree. The facade here is an in-memory fixture that records every
// batch, so the round-trip budget is asserted too.

const test = require('node:test');
const assert = require('node:assert/strict');
const {
    scanPackages,
    readPackages,
    namesFromDts,
    membersFromDts,
    reExports,
    declared,
} = require('../src/analysis/packages');

function memoryAccess(files) {
    const calls = [];
    return {
        calls,
        async read(paths) {
            calls.push([...paths]);
            const out = new Map();
            for (const p of paths) {
                if (p.startsWith('/') || p.split('/').includes('..') || p.split('/').includes('.'))
                    throw new Error(`facade rejects non-canonical path ${p}`);
                if (Object.prototype.hasOwnProperty.call(files, p)) out.set(p, Buffer.from(files[p], 'utf8'));
            }
            return out;
        },
    };
}

const json = (o) => JSON.stringify(o, null, 2);

const FIXTURE = {
    'package.json': json({
        name: 'fixture',
        version: '1.0.0',
        dependencies: { widget: '^2.0.0', plain: '^1.0.0' },
        devDependencies: { missing: '^1.0.0' },
        peerDependencies: { widget: '*' },
    }),
    'node_modules/widget/package.json': json({
        name: 'widget',
        version: '2.3.1',
        description: 'A widget.',
        main: 'index.js',
        types: './types/index.d.ts',
        exports: { '.': './index.js', './extra': './extra.js' },
        bin: { widgetize: 'bin.js' },
    }),
    'node_modules/widget/types/index.d.ts': [
        "export type * from './extras';",
        'export declare function build(spec: string): Widget;',
        'export declare class Widget {',
        '    render(target: string): void;',
        // A wrapped signature: its parameters must not be mistaken for members.
        '    resize(',
        '        width: number,',
        '        height: number,',
        '    ): void;',
        '}',
        '',
    ].join('\n'),
    // One re-export away — where the answer usually is.
    'node_modules/widget/types/extras.d.ts': [
        'export declare class Extras {',
        '    polish(times: number): void;',
        '}',
        '',
    ].join('\n'),
    // No types: the surface is inferred from the entry point's code.
    'node_modules/plain/package.json': json({
        name: 'plain',
        version: '1.0.0',
        main: './lib/./main.js',
        bin: 'cli.js',
    }),
    'node_modules/plain/lib/main.js': "'use strict';\nfunction go() {}\nmodule.exports = { go, stop: 1 };\n",
    // A transitive dependency, asked for by name.
    'node_modules/@scope/deep/package.json': json({ name: '@scope/deep', version: '0.1.0' }),
    'node_modules/@scope/deep/index.d.ts': 'export { a, b as c };\nexport default x;\n',
};

test('scanPackages: the declared dependencies, each with its installed surface', async () => {
    const access = memoryAccess(FIXTURE);
    const { manifest, packages, reason } = await scanPackages(access);
    assert.equal(reason, undefined);
    assert.equal(manifest.name, 'fixture');
    assert.deepEqual([...packages.keys()].sort(), ['plain', 'widget'], 'a declared-but-missing package is absent');

    const widget = packages.get('widget');
    assert.equal(widget.version, '2.3.1', 'the installed version, not the declared range');
    assert.equal(widget.source, 'dependency', 'first list wins');
    assert.equal(widget.description, 'A widget.');
    assert.equal(widget.types, './types/index.d.ts');
    assert.deepEqual(widget.exportKeys, ['.', './extra']);
    assert.deepEqual(widget.bin, ['widgetize']);
    assert.equal(widget.surfaceFrom, 'types');
    assert.equal(widget.surfaceFile, 'types/index.d.ts');
    assert.ok(widget.surface.includes('build') && widget.surface.includes('Widget'), JSON.stringify(widget.surface));
    assert.ok(widget.members.includes('Widget.render'));
    assert.ok(widget.members.includes('Widget.resize'));
    assert.ok(widget.members.includes('Extras.polish'), 'a re-export one level away is followed');
    assert.ok(!widget.members.some((m) => /\.(width|height)$/.test(m)), 'wrapped parameters are not members');

    const plain = packages.get('plain');
    assert.equal(plain.surfaceFrom, 'main');
    assert.equal(plain.surfaceFile, 'lib/main.js', '/./ in a manifest path is normalised');
    assert.deepEqual(plain.surface, ['go', 'stop']);
    assert.deepEqual(plain.members, []);
    assert.deepEqual(plain.bin, ['plain']);

    // One batch for the root manifest, one for package manifests, one for
    // surface candidates, one for re-exports — whatever the dependency count.
    assert.equal(access.calls.length, 4, JSON.stringify(access.calls));
    assert.deepEqual(access.calls[0], ['package.json']);
    for (const batch of access.calls) assert.equal(new Set(batch).size, batch.length, 'no duplicate paths in a batch');
});

test('readPackages: any package by name, sourced as transitive by default', async () => {
    const access = memoryAccess(FIXTURE);
    const packages = await readPackages(access, ['@scope/deep', 'nope']);
    assert.deepEqual([...packages.keys()], ['@scope/deep']);
    const deep = packages.get('@scope/deep');
    assert.equal(deep.source, 'transitive');
    assert.deepEqual(deep.surface.sort(), ['a', 'c', 'default']);
    assert.equal(deep.surfaceFile, 'index.d.ts');
    assert.ok(
        access.calls.every((b) => b.every((p) => p.startsWith('node_modules/'))),
        'workspace-relative paths',
    );
});

test('a package with no readable surface still reports its manifest', async () => {
    const access = memoryAccess({
        'node_modules/bare/package.json': json({ name: 'bare', version: '3.0.0', types: '../../../etc/x.d.ts' }),
        'node_modules/broken/package.json': '{ not json',
    });
    const packages = await readPackages(access, ['bare', 'broken']);
    assert.deepEqual([...packages.keys()], ['bare'], 'an unparseable manifest is skipped');
    const bare = packages.get('bare');
    assert.deepEqual(bare.surface, []);
    assert.deepEqual(bare.members, []);
    assert.equal(bare.surfaceFrom, null);
    // A types path that climbs out of the workspace is never sent to the facade.
    assert.ok(access.calls.flat().every((p) => !p.includes('..')));
});

test('scanPackages: no manifest, or one that does not parse, is a reason — not a throw', async () => {
    const none = await scanPackages(memoryAccess({}));
    assert.equal(none.manifest, null);
    assert.equal(none.packages.size, 0);
    assert.match(none.reason, /no package\.json/);
    const bad = await scanPackages(memoryAccess({ 'package.json': '{' }));
    assert.match(bad.reason, /does not parse/);
});

test('declaration-file helpers', () => {
    assert.deepEqual(
        namesFromDts('export declare function f(): void;\nexport interface I {}\nexport { a, b as c, 1bad };\n').sort(),
        ['I', 'a', 'c', 'f'],
    );
    assert.deepEqual(membersFromDts('interface Opts {\n    readonly size: number;\n    go<T>(x: T): T;\n}\n'), [
        'Opts.size',
        'Opts.go',
    ]);
    assert.deepEqual(reExports(`export * from './a';\nexport { x } from "./b";\nexport * from 'pkg';\n`), [
        './a',
        './b',
    ]);
    assert.deepEqual(
        [
            ...declared({
                dependencies: { a: '1' },
                devDependencies: { b: '1', a: '2' },
                optionalDependencies: { c: '1' },
            }),
        ],
        [
            ['a', 'dependency'],
            ['b', 'dev'],
            ['c', 'optional'],
        ],
    );
});
