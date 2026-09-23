'use strict';
// A small project on disk for the workspace/store suites, plus the facades
// every suite runs against: localFs, and the shell facade over a REAL ssh
// connection to a throwaway sshd (skipped when /usr/sbin/sshd is absent). The
// same behaviour must hold remotely — that is the point of the package.

const fs = require('fs');
const os = require('os');
const path = require('path');
const { localFs, shell } = require('../../src/access');
const { startSshd, sshdAvailable } = require('../helpers/sshd');

const MATH = [
    "'use strict';",
    '',
    '// Adds two numbers. The doc line above a symbol is indexed with it,',
    '// because prose is how a codebase says what a thing is for.',
    'function add(a, b) {',
    '    return a + b;',
    '}',
    '',
    'const TAX_RATE = 0.17;',
    '',
    'const scale = (v, by) => v * by;',
    '',
    // A ONE-LINE symbol whose prose mentions another symbol by name: full-text
    // returns both, and the exact match must still win.
    '// A shorthand that calls add twice.',
    'const twice = (v) => add(v, v);',
    '',
    'module.exports = { add, scale, TAX_RATE, twice };',
    '',
].join('\n');

function writeFixture(root) {
    fs.rmSync(root, { recursive: true, force: true });
    fs.mkdirSync(path.join(root, 'lib'), { recursive: true });
    const w = (rel, text) => {
        fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
        fs.writeFileSync(path.join(root, rel), text);
    };
    w('lib/math.js', MATH);
    // An .mjs file: proves the sourceType fallback.
    w(
        'lib/esm.mjs',
        ["export const GREETING = 'hi';", 'export function greet(name) { return `${GREETING} ${name}`; }', ''].join(
            '\n',
        ),
    );
    // A factory: the shape where all the interesting code hides inside a
    // closure.
    w(
        'lib/factory.js',
        [
            "'use strict';",
            'function makeCounter(start) {',
            '    let n = start;',
            '    // Bumps the counter by a step and reports the new total.',
            '    function bump(by) { n += by; return n; }',
            '    return {',
            '        value() { return n; },',
            '        plus(by) { return bump(by); },',
            '    };',
            '}',
            '',
            'module.exports = { makeCounter };',
            '',
        ].join('\n'),
    );
    // A dependency, surface only.
    w(
        'package.json',
        JSON.stringify(
            {
                name: 'fixture',
                version: '1.0.0',
                dependencies: { widget: '^2.0.0' },
                devDependencies: { missing: '^1.0.0' },
            },
            null,
            2,
        ),
    );
    w(
        'node_modules/widget/package.json',
        JSON.stringify(
            {
                name: 'widget',
                version: '2.3.1',
                description: 'A widget.',
                main: 'index.js',
                types: './types/index.d.ts',
            },
            null,
            2,
        ),
    );
    w(
        'node_modules/widget/types/index.d.ts',
        [
            "export type * from './extras';",
            'export declare function build(spec: string): Widget;',
            'export declare class Widget {',
            '    render(target: string): void;',
            '    resize(',
            '        width: number,',
            '        height: number,',
            '    ): void;',
            '}',
            '',
        ].join('\n'),
    );
    w(
        'node_modules/widget/types/extras.d.ts',
        ['export declare class Extras {', '    polish(times: number): void;', '}', ''].join('\n'),
    );
    // A UTF-8 em-dash, ellipsis and accent: the wire must be byte-exact.
    w(
        'lib/unicode.js',
        [
            "'use strict';",
            '// A comment — with an em-dash, ellipsis … and an accent é.',
            'const MARKER = "—…é";',
            'module.exports = { MARKER };',
            '',
        ].join('\n'),
    );
    // Working state the project's .gitignore excludes (no leading dot, so the
    // dot-directory rule never sees it).
    w('.gitignore', 'session-state/\n*.log\n');
    w('session-state/key-1.json', '{"secret":"session"}\n');
    w('debug.log', 'noise\n');
    // A dot-directory below the root: working state, never listed.
    w('.cache/blob.js', 'const hidden = 1;\n');
}

// The files writeFixture makes that the index should see.
const FIXTURE_FILES = ['.gitignore', 'lib/esm.mjs', 'lib/factory.js', 'lib/math.js', 'lib/unicode.js', 'package.json'];

let seq = 0;
function tmpRoot(tag = 'ws') {
    const base = fs.mkdtempSync(path.join(os.tmpdir(), `okcode-${tag}-`));
    return { base, root: path.join(base, `root${++seq}`) };
}

// [{ name, make(root) → facade, before?, after? }] — the ssh facade needs a
// daemon started once per suite.
function facades() {
    const out = [{ name: 'localFs', make: (root) => localFs(root) }];
    const ssh = { name: 'shell/ssh', skip: sshdAvailable() ? false : 'no /usr/sbin/sshd', daemon: null };
    ssh.before = async () => {
        ssh.daemon = await startSshd();
    };
    ssh.after = async () => {
        if (ssh.daemon) await ssh.daemon.stop();
    };
    ssh.make = (root) => shell({ root, run: ssh.daemon.run });
    out.push(ssh);
    return out;
}

// A facade wrapper that counts calls per method, delegating to the real one.
function counted(access) {
    const counts = {};
    const wrapped = { ...access };
    for (const m of ['list', 'stat', 'read', 'commit', 'lock', 'exec', 'remove']) {
        if (typeof access[m] !== 'function') continue;
        counts[m] = 0;
        wrapped[m] = (...args) => {
            counts[m]++;
            return access[m](...args);
        };
    }
    return { access: wrapped, counts, reset: () => Object.keys(counts).forEach((k) => (counts[k] = 0)) };
}

module.exports = { writeFixture, FIXTURE_FILES, tmpRoot, facades, counted, MATH };
