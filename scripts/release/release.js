#!/usr/bin/env node
'use strict';
// npm run release -- patch|minor|major|x.y.z
//   bump package.json, turn CHANGELOG.md's "## [Unreleased]" into "## [x.y.z] - <today>", commit "release vx.y.z", tag vx.y.z,
//   then npm run build -> check -> test:dist -> publish (each a separate script; on a failure fix it and run the remaining ones).
// Needs a clean tree on the release branch and a non-empty "## [Unreleased]" section.
const fs = require('node:fs');
const path = require('node:path');
const L = require('./release-lib');
const { REPO, die, sh, run } = L;

const arg = process.argv[2];
if (!arg) die('usage: npm run release -- patch|minor|major|x.y.z');
if (L.dirty()) die(`working tree is not clean:\n${L.dirty()}`);
const branch = sh('git', ['rev-parse', '--abbrev-ref', 'HEAD']);
if (branch !== L.RELEASE_BRANCH) die(`on branch ${branch}, the release branch is ${L.RELEASE_BRANCH}`);
const cur = L.VERSION.split('-')[0].split('.').map(Number);
const next =
    arg === 'patch' ? `${cur[0]}.${cur[1]}.${cur[2] + 1}` : arg === 'minor' ? `${cur[0]}.${cur[1] + 1}.0` : arg === 'major' ? `${cur[0] + 1}.0.0` : arg;
if (!/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/.test(next)) die(`not a version: ${next}`);
if (L.tagCommit(`v${next}`)) die(`tag v${next} already exists`);

const clPath = path.join(REPO, 'CHANGELOG.md');
const cl = fs.readFileSync(clPath, 'utf8');
const m = /^## \[Unreleased\][^\n]*\n([\s\S]*?)(?=^## \[|(?![\s\S]))/m.exec(cl);
if (!m || !m[1].trim()) die('CHANGELOG.md needs a non-empty "## [Unreleased]" section with the release notes');
const today = new Date().toISOString().slice(0, 10);
fs.writeFileSync(clPath, cl.replace(/^## \[Unreleased\][^\n]*\n/m, `## [Unreleased]\n\n## [${next}] - ${today}\n`));
const pkgPath = path.join(REPO, 'package.json');
const pkg = JSON.parse(fs.readFileSync(pkgPath, 'utf8'));
pkg.version = next;
fs.writeFileSync(pkgPath, JSON.stringify(pkg, null, 4) + '\n');
run('git', ['commit', '-qam', `release v${next}`]);
run('git', ['tag', `v${next}`]);
for (const s of ['build', 'check', 'test:dist', 'publish']) run('npm', ['run', s]);
