#!/usr/bin/env node
'use strict';
// npm run build — the one build that ships: a clean dist/ from HEAD.
//   1. the native addon + standalone CLI, built in Docker against glibc 2.28 (rs/packaging/build-linux-x64.sh, dist profile)
//   2. dist/package/: exactly the shipped files (release-lib SHIP) + a package.json with the published fields only
//   3. npm pack -> dist/kedem-okcode-<version>.tgz, and dist/BUILD.json (version, HEAD, sha256, file list)
// Refuses a dirty tree (what ships must be a commit); OKCODE_BUILD_DIRTY=1 builds anyway for a look, and check refuses it.
const fs = require('node:fs');
const path = require('node:path');
const L = require('./release-lib');
const { REPO, KEY, VERSION, D, SCRATCH, SHIP, MANIFEST_FIELDS, ROOT_PKG, die, step, run, sha256, mb, head, dirty } = L;

const isDirty = dirty();
if (isDirty && process.env.OKCODE_BUILD_DIRTY !== '1') die(`working tree is not clean (commit first, or OKCODE_BUILD_DIRTY=1 for a look):\n${isDirty}`);
const versionProblems = L.versionProblems();
if (versionProblems.length) die(versionProblems.join('\n'));

step('1. native binaries (glibc 2.28, dist profile)');
const native = path.join(SCRATCH, 'native', KEY);
fs.rmSync(native, { recursive: true, force: true });
fs.mkdirSync(native, { recursive: true });
run('bash', [path.join(REPO, 'rs/packaging/build-linux-x64.sh')], { env: { ...process.env, PROFILE: 'dist', OUT: native } });

step('2. dist/package');
fs.rmSync(D.dir, { recursive: true, force: true });
fs.mkdirSync(D.pkg, { recursive: true });
for (const rel of SHIP) {
    if (rel === 'native') continue;
    const src = path.join(REPO, rel);
    if (!fs.existsSync(src)) die(`shipped path ${rel} is missing`);
    fs.cpSync(src, path.join(D.pkg, rel), { recursive: true });
}
fs.mkdirSync(path.join(D.pkg, 'native', KEY), { recursive: true });
for (const f of ['okcode.node', 'okcode']) {
    fs.copyFileSync(path.join(native, f), path.join(D.pkg, 'native', KEY, f));
    fs.chmodSync(path.join(D.pkg, 'native', KEY, f), 0o755);
}
const manifest = {};
for (const k of MANIFEST_FIELDS) if (k in ROOT_PKG) manifest[k] = ROOT_PKG[k];
manifest.files = SHIP.filter((f) => !['README.md', 'LICENSE'].includes(f));
fs.writeFileSync(path.join(D.pkg, 'package.json'), JSON.stringify(manifest, null, 4) + '\n');

step('3. npm pack');
run('npm', ['pack', '--ignore-scripts', '--pack-destination', D.dir], { cwd: D.pkg, env: { ...process.env, TMPDIR: process.env.TMPDIR || SCRATCH } });
if (!fs.existsSync(D.tgz)) die(`npm pack did not write ${D.tgz}`);
const files = [];
const walk = (d, pre = '') => {
    for (const e of fs.readdirSync(d, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
        const rel = pre ? `${pre}/${e.name}` : e.name;
        if (e.isDirectory()) walk(path.join(d, e.name), rel);
        else files.push(rel);
    }
};
walk(D.pkg);
L.writeState(D.build, { version: VERSION, head: head(), dirty: !!isDirty, sha256: sha256(D.tgz), size: fs.statSync(D.tgz).size, files, built: new Date().toISOString() });
console.log(`\nbuild: ${path.relative(REPO, D.tgz)}  ${mb(fs.statSync(D.tgz).size)}  sha256 ${sha256(D.tgz)}  (${files.length} files)`);
