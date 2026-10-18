#!/usr/bin/env node
'use strict';
// npm run test:dist — dist/ as a user gets it. Writes dist/TESTED (version, HEAD, sha256) when both pass.
//   1. install check: a bare consumer in node:24-bookworm-slim (glibc 2.36) npm-installs the PUBLISHED @kedem/okdb ^3 and dist/'s
//      tarball, then exercises library mode (own store, host-passed okdb db), `npx okcode` and the bundled standalone binary
//   2. the suite: every test file against the native build, with @kedem/okdb ^3 from the registry (rs/tools/bridge-run.sh)
const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const L = require('./release-lib');
const { REPO, VERSION, D, die, step, sha256, head } = L;

const problems = L.distProblems();
if (!L.readState(D.checked)) problems.push('dist/ is not CHECKED (npm run check)');
if (problems.length) die(problems.join('\n'));
const OKDB = `@kedem/okdb@${L.readJson(path.join(D.pkg, 'package.json')).dependencies['@kedem/okdb']}`;

step(`1. install check (bookworm, ${OKDB} from the registry)`);
const ic = spawnSync('bash', [path.join(REPO, 'rs/packaging/install-check.sh'), OKDB, D.tgz], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'] });
process.stdout.write(ic.stdout);
const want = ['CHECK library: own store + host db OK', 'CHECK npx okcode OK', 'CHECK standalone okcode binary OK'];
const missing = want.filter((w) => !ic.stdout.includes(w));
if (ic.status !== 0 || missing.length) die(`install check failed${missing.length ? ': missing ' + missing.join('; ') : ''}`);

step(`2. suite (${OKDB})`);
const s = spawnSync('bash', [path.join(REPO, 'rs/tools/bridge-run.sh'), OKDB, '--no-bench'], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'] });
process.stdout.write(s.stdout);
const m = /tests: pass (\d+) fail (\d+)/.exec(s.stdout);
const cli = /cli\.test\.js: # pass (\d+) # fail (\d+)/.exec(s.stdout);
const diff = /cli-diff: (\d+)\/(\d+)/.exec(s.stdout);
// the one accepted failure: analysis-treesitter's injected-loader test (no JS grammar loader exists in the native build)
const failing = s.stdout.split('\n').filter((l) => /^\s+[\w.-]+: pass \d+\/\d+ fail [1-9]/.test(l));
const accepted = failing.length === 1 && /analysis-treesitter: pass \d+\/\d+ fail 1 /.test(failing[0]);
if (s.status !== 0 || !m || (Number(m[2]) > 1) || (Number(m[2]) === 1 && !accepted)) die('the suite failed');
if (!cli || cli[2] !== '0' || !diff || diff[1] !== diff[2]) die('the CLI checks failed');

L.writeState(D.tested, { version: VERSION, head: head(), sha256: sha256(D.tgz), okdb: OKDB, suite: { pass: Number(m[1]), acceptedFail: Number(m[2]) }, cli: `${cli[1]}/${cli[1]}`, cliDiff: `${diff[1]}/${diff[2]}`, tested: new Date().toISOString() });
console.log(`\ntest:dist: OK -> ${path.relative(REPO, D.tested)}`);
