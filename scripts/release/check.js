#!/usr/bin/env node
'use strict';
// npm run check — every guard on dist/, on the tarball's own contents. Writes dist/CHECKED (version, HEAD, sha256) when all pass.
//   identity   dist/ was built from HEAD (or HEAD changed release tooling only), clean tree, tarball == dist/package
//   allowlist  only the shipped paths; no tests, source maps, .env, keys or licenses; no Rust source
//   manifest   published fields only (no scripts, devDependencies, …); version == the root package.json; @kedem/okdb ^3
//   js         every .js parses (node --check); no sourceMappingURL
//   binaries   ELF, stripped, glibc <= 2.28, no C++ ABI (check-glibc.sh)
//   text       no internal term (release-lib INTERNAL_TERMS) anywhere in shipped text, comments included
//   version    CHANGELOG has a dated "## [<version>]" section
const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const L = require('./release-lib');
const { REPO, VERSION, D, SCRATCH, SHIP, MANIFEST_FIELDS, INTERNAL_TERMS, ROOT_PKG, die, step, sh, sha256, head } = L;

const problems = [];
const fail = (m) => problems.push(m);

step('identity');
problems.push(...L.distProblems());
const build = L.readState(D.build) || {};
if (build.dirty) fail('dist/ was built from a dirty tree (OKCODE_BUILD_DIRTY): rebuild from a commit');
if (L.dirty()) fail(`working tree is not clean:\n${L.dirty()}`);
problems.push(...L.versionProblems());
if (!fs.existsSync(D.tgz)) die(problems.join('\n'));
const X = path.join(SCRATCH, 'check');
fs.rmSync(X, { recursive: true, force: true });
fs.mkdirSync(X, { recursive: true });
sh('tar', ['xzf', D.tgz, '-C', X]);
const P = path.join(X, 'package');
const files = [];
const walk = (d, pre = '') => {
    for (const e of fs.readdirSync(d, { withFileTypes: true })) {
        const rel = pre ? `${pre}/${e.name}` : e.name;
        if (e.isDirectory()) walk(path.join(d, e.name), rel);
        else files.push(rel);
    }
};
walk(P);
files.sort();
const same = files.length === (build.files || []).length && files.every((f) => fs.existsSync(path.join(D.pkg, f)) && Buffer.compare(fs.readFileSync(path.join(P, f)), fs.readFileSync(path.join(D.pkg, f))) === 0);
if (!same) fail('the tarball differs from dist/package');
console.log(`${files.length} files, sha256 ${sha256(D.tgz)}`);

step('allowlist');
const allowed = (f) => f === 'package.json' || SHIP.some((s) => f === s || f.startsWith(s + '/'));
for (const f of files) {
    if (!allowed(f)) fail(`not on the allowlist: ${f}`);
    if (/(^|\/)(test|tests|__tests__)\/|\.test\.js$|\.spec\.js$/.test(f)) fail(`test file shipped: ${f}`);
    if (/\.map$/.test(f)) fail(`source map shipped: ${f}`);
    if (/(^|\/)\.env|\.pem$|\.key$|\.license$|\.p12$|id_rsa/.test(f)) fail(`secret-looking file shipped: ${f}`);
    if (/^src\/|(^|\/)Cargo\.(toml|lock)$|\.rs$/.test(f)) fail(`Rust source shipped: ${f}`);
}
for (const s of SHIP) if (!files.some((f) => f === s || f.startsWith(s + '/'))) fail(`expected shipped path missing: ${s}`);

step('manifest');
const m = JSON.parse(fs.readFileSync(path.join(P, 'package.json'), 'utf8'));
for (const k of Object.keys(m)) if (!MANIFEST_FIELDS.includes(k)) fail(`package.json carries a dev/internal field: ${k}`);
if (m.version !== VERSION || m.version !== ROOT_PKG.version) fail(`package.json version ${m.version} != ${VERSION}`);
if (m.name !== L.NPM_NAME) fail(`package.json name ${m.name}`);
const deps = Object.keys(m.dependencies || {});
if (deps.join() !== '@kedem/okdb' || !/^\^3\./.test(m.dependencies['@kedem/okdb'])) fail(`dependencies must be exactly @kedem/okdb ^3: ${JSON.stringify(m.dependencies)}`);

step('js');
for (const f of files.filter((f) => f.endsWith('.js'))) {
    const r = spawnSync(process.execPath, ['--check', path.join(P, f)], { encoding: 'utf8' });
    if (r.status !== 0) fail(`${f} does not parse: ${r.stderr.split('\n')[0]}`);
    if (/sourceMappingURL/.test(fs.readFileSync(path.join(P, f), 'utf8'))) fail(`${f} references a source map`);
}

step('binaries');
const bins = files.filter((f) => f.startsWith('native/'));
for (const f of bins) {
    const p = path.join(P, f);
    const desc = sh('file', ['-b', p]);
    if (!/ELF/.test(desc)) fail(`${f} is not an ELF binary (${desc})`);
    if (!/stripped/.test(desc) || /not stripped/.test(desc)) fail(`${f} is not stripped`);
    const sections = sh('readelf', ['-S', p]);
    if (/\.symtab|\.debug_/.test(sections)) fail(`${f} carries symbol/debug sections`);
}
const g = spawnSync('bash', [path.join(REPO, 'scripts/release/check-glibc.sh'), ...bins.map((f) => path.join(P, f))], { encoding: 'utf8' });
process.stdout.write(g.stdout);
if (g.status !== 0) fail('glibc floor / C++ ABI check failed');

step('text (internal terms, comments included)');
const text = files.filter((f) => /\.(js|md|json|sh|ps1|txt)$/.test(f) || f === 'LICENSE');
let hits = 0;
for (const f of text) {
    fs.readFileSync(path.join(P, f), 'utf8').split('\n').forEach((line, i) => {
        if (INTERNAL_TERMS.test(line)) {
            hits++;
            if (hits <= 30) console.log(`  ${f}:${i + 1}: ${line.trim().slice(0, 140)}`);
        }
    });
}
if (hits) fail(`${hits} line(s) with internal terms in shipped text`);

if (problems.length) die(`\n- ${problems.join('\n- ')}`);
L.writeState(D.checked, { version: VERSION, head: head(), sha256: sha256(D.tgz), files: files.length, checked: new Date().toISOString() });
console.log(`\ncheck: OK -> ${path.relative(REPO, D.checked)}  (sha256 ${sha256(D.tgz)})`);
