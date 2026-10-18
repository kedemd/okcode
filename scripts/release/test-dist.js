#!/usr/bin/env node
'use strict';
// npm run test:dist — dist/ as a user gets it. Writes dist/TESTED (version, HEAD, sha256) when both pass.
//   1. install check: a bare consumer in node:24-bookworm-slim (glibc 2.36) npm-installs the PUBLISHED @kedem/okdb ^3 and dist/'s
//      tarball, then exercises library mode (own store, host-passed okdb db), `npx okcode` and the bundled standalone binary
//   2. the suite: every test file (but the live-Ollama one) against dist/'s own binaries, with @kedem/okdb ^3 from the registry,
//      then cli.test.js again against the standalone binary. No failure is accepted.
const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const L = require('./release-lib');
const { REPO, VERSION, D, SCRATCH, die, step, sh, sha256, head } = L;

const problems = L.distProblems();
if (!L.readState(D.checked)) problems.push('dist/ is not CHECKED (npm run check)');
if (problems.length) die(problems.join('\n'));
const OKDB = `@kedem/okdb@${L.readJson(path.join(D.pkg, 'package.json')).dependencies['@kedem/okdb']}`;

step(`1. install check (bookworm, ${OKDB} from the registry)`);
const ic = spawnSync('bash', [path.join(REPO, 'scripts/release/install-check.sh'), OKDB, D.tgz], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'] });
process.stdout.write(ic.stdout);
const want = ['CHECK library: own store + host db OK', 'CHECK npx okcode OK', 'CHECK standalone okcode binary OK'];
const missing = want.filter((w) => !ic.stdout.includes(w));
if (ic.status !== 0 || missing.length) die(`install check failed${missing.length ? ': missing ' + missing.join('; ') : ''}`);

step(`2. suite (the shipped binaries, ${OKDB} from the registry)`);
// a tree of HEAD's tests + lib/ + bin/ (+ the access scripts the tests compare), with dist/'s native/ (byte for byte what ships) and okdb from the registry
const T = path.join(SCRATCH, 'suite');
const TMP = path.join(SCRATCH, 'tmp');
fs.rmSync(T, { recursive: true, force: true });
fs.rmSync(TMP, { recursive: true, force: true });
fs.mkdirSync(T, { recursive: true });
fs.mkdirSync(TMP, { recursive: true });
sh('sh', ['-c', `git archive HEAD bin lib test crates/okcode-access/scripts package.json package-lock.json | tar -x -C '${T}'`]);
fs.cpSync(path.join(D.pkg, 'native'), path.join(T, 'native'), { recursive: true });
// the repo's node_modules hard-linked (cheap; devDependencies such as @kedem/okjs), okdb ^3 installed over it from the registry
sh('cp', ['-al', path.join(REPO, 'node_modules'), path.join(T, 'node_modules')]);
sh('npm', ['install', '--no-save', '--no-audit', '--no-fund', '--prefer-online', '--ignore-scripts', OKDB], { cwd: T, stdio: ['ignore', 'pipe', 'inherit'] });
const okdbVersion = L.readJson(path.join(T, 'node_modules/@kedem/okdb/package.json')).version;
console.log(`okdb ${okdbVersion}`);
const env = { ...process.env, TMPDIR: TMP, OKCODE_PARSE_THREADS: '2' };
delete env.OKCODE_NATIVE_BINARY; // the shipped loader path: native/<key>/okcode.node
delete env.OKDB_NATIVE_BINARY;
delete env.NODE_OPTIONS;
const tests = fs.readdirSync(path.join(T, 'test')).filter((f) => f.endsWith('.test.js') && f !== 'e2e-ollama.test.js').sort().map((f) => `test/${f}`);
const tap = (args, extra = {}) => {
    const r = spawnSync(process.execPath, ['--test', '--test-force-exit', '--test-reporter=tap', ...args], { cwd: T, env: { ...env, ...extra }, encoding: 'utf8', maxBuffer: 1 << 28, timeout: 3600e3 });
    const n = (k) => Number((new RegExp(`^# ${k} (\\d+)`, 'm').exec(r.stdout) || [])[1] || 0);
    const failed = r.stdout.split('\n').filter((l) => /^\s*not ok /.test(l) && !/# SKIP/.test(l));
    return { status: r.status, pass: n('pass'), fail: n('fail'), cancelled: n('cancelled'), tests: n('tests'), failed, stderr: r.stderr };
};
const suite = tap([`--test-concurrency=${process.env.TEST_JOBS || 3}`, ...tests]);
console.log(`suite: ${tests.length} files, pass ${suite.pass}/${suite.tests} fail ${suite.fail} cancelled ${suite.cancelled}`);
if (suite.status !== 0 || suite.fail || suite.cancelled || !suite.pass) die(`the suite failed:\n${suite.failed.slice(0, 20).join('\n')}\n${suite.stderr.slice(-2000)}`);
// the standalone binary through the same CLI cases (cli.test.js above ran bin/okcode.js)
const cli = tap(['test/cli.test.js'], { OKCODE_CLI_BIN: path.join(T, 'native', L.KEY, 'okcode') });
console.log(`standalone CLI (cli.test.js): pass ${cli.pass}/${cli.tests} fail ${cli.fail}`);
if (cli.status !== 0 || cli.fail || cli.cancelled || !cli.pass) die(`the standalone CLI failed:\n${cli.failed.join('\n')}\n${cli.stderr.slice(-2000)}`);
fs.rmSync(TMP, { recursive: true, force: true });

L.writeState(D.tested, { version: VERSION, head: head(), sha256: sha256(D.tgz), okdb: `${OKDB} (${okdbVersion})`, suite: { files: tests.length, pass: suite.pass, tests: suite.tests }, standaloneCli: `${cli.pass}/${cli.tests}`, tested: new Date().toISOString() });
console.log(`\ntest:dist: OK -> ${path.relative(REPO, D.tested)}`);
