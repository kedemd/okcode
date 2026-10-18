'use strict';
// Shared by the release scripts (root package.json):
//   npm run build      scripts/release/build.js     clean glibc-2.28 Docker build -> dist/ (package dir + tarball + BUILD.json)
//   npm run check      scripts/release/check.js     every guard on dist/ -> dist/CHECKED
//   npm run test:dist  scripts/release/test-dist.js bookworm install check against the published @kedem/okdb ^3 + the suite -> dist/TESTED
//   npm run publish    scripts/release/publish.js   ships dist/ (refuses unless built from HEAD, CHECKED and TESTED); typed confirmation
//   npm run release    scripts/release/release.js   bump + CHANGELOG + commit + tag, then build -> check -> test:dist -> publish
// dist/ is gitignored and holds exactly one build: what `npm run publish` ships is what check + test:dist saw, byte for byte.
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const crypto = require('node:crypto');
const { execFileSync, spawnSync } = require('node:child_process');

const REPO = path.resolve(__dirname, '../..');
const KEY = 'linux-x64-gnu'; // the one bundled target in 1.x
const NPM_NAME = '@kedem/okcode';
const GH_REPO = 'kedemd/okcode';
const RELEASE_BRANCH = process.env.OKCODE_RELEASE_BRANCH || 'master';
const readJson = (f) => JSON.parse(fs.readFileSync(f, 'utf8'));
const ROOT_PKG = readJson(path.join(REPO, 'package.json'));
const VERSION = ROOT_PKG.version;
const TAG = `v${VERSION}`;
const DIST = path.join(REPO, 'dist');
const TGZ_NAME = `kedem-okcode-${VERSION}.tgz`;
const D = {
    dir: DIST,
    pkg: path.join(DIST, 'package'), // the package directory npm packs (exactly the tarball's contents)
    tgz: path.join(DIST, TGZ_NAME),
    build: path.join(DIST, 'BUILD.json'),
    checked: path.join(DIST, 'CHECKED'),
    tested: path.join(DIST, 'TESTED'),
    published: path.join(DIST, 'PUBLISHED'),
};
const CACHE = path.join(os.homedir(), '.cache/okcode-release');
const SCRATCH = path.join(CACHE, VERSION);

// What the package ships, and nothing else (paths relative to the package root).
const SHIP = ['bin', 'lib', 'native', 'docs/ACCESS.md', 'docs/DESIGN.md', 'docs/UPGRADING.md', 'docs/NATIVE.md', 'README.md', 'CHANGELOG.md', 'LICENSE'];
// The shipped package.json keeps these fields only (dev scripts, devDependencies, repo-internal keys are dropped).
const MANIFEST_FIELDS = ['name', 'version', 'description', 'keywords', 'homepage', 'bugs', 'repository', 'license', 'author', 'main', 'bin', 'files', 'engines', 'dependencies', 'publishConfig'];
// The scan patterns are built from split fragments (j(...)) so that these sources never match their own scans.
const j = (...p) => p.join('');
const rx = (parts, flags = '') => new RegExp(parts.join('|'), flags);
// Words that must not appear in shipped text: internal process, internal names, machine paths. (Crate names are public.)
const INTERNAL_TERMS = {
    test(line) {
        return INTERNAL_CS.test(line) || INTERNAL_CI.test(line.replace(/\.slice\(/g, ''));
    },
};
const INTERNAL_CS = rx([
    j('HAND', 'OFF'), j('PORT', 'ING'), j('SHARED', ' FILE'), j('OWN', 'ER:'), j('TEMPO', 'RARY'), j('\\bTO', 'DO\\b'), j('\\bFIX', 'ME\\b'), j('\\bX', 'XX\\b'),
    j('\\bC', '[1-7]\\b'), j('PARITY', '\\.md'),
]);
const INTERNAL_CI = rx(
    [
        j('okdb', '-src'), j('stor', 'ita'), j('\\bbr', 'ain\\b'), j('\\/ho', 'me\\/[a-z]'), j('\\bsli', 'ce\\b'), j('\\bwa', 've [0-9]'), j('\\bjud', 'ge'),
        j('rust', '-port'), j('archi', 'tect'), j('release', ' plan'), j('\\bcla', 'ude\\b'), j('preload', '-native'), j('okcode', '-rs'), j('\\bgold', 'en\\b'),
        j('(^|[^a-z])rs', '\\/'), j('integrate', '\\.sh'), j('okdb', '-napi'), j('okdb', '-dispatch'),
        j('kedem', '@'),
    ],
    'i',
);
// What must never reach the public repo (any pushed commit): internal notes, machine paths, credentials.
const PUSH_TERMS = rx([
    j('\\/ho', 'me\\/[a-z]'), j('\\.con', 'fig\\/br', 'ain'),
    j('stor', 'ita'), j('okdb', '-src'), j('HAND', 'OFF'), j('PORT', 'ING\\.md'), j('-----BEGIN [A-Z ]*PRIVATE ', 'KEY-----'),
    j('npm_[A-Za-z0-9]', '{36}'), j('ghp_[A-Za-z0-9]', '{36}'),
]);
const PUSH_FILES = rx([j('(^|\\/)\\.en', 'v($|\\.)'), j('\\.pe', 'm$'), j('\\.ke', 'y$'), j('\\.lic', 'ense$'), j('\\.p1', '2$'), j('id_r', 'sa'), j('(^|\\/)HAND', 'OFF\\.md$'), j('(^|\\/)PORT', 'ING\\.md$')]);
// Nothing public credits an AI assistant: the user is the sole author and committer.
const CREDIT = rx([j('co-author', 'ed-by:.*(cla', 'ude|anthro', 'pic)'), j('noreply@', 'anthro', 'pic\\.com'), j('generated with \\[?cla', 'ude'), j('cla', 'ude\\.com\\/cla', 'ude-code'), j('\\bcla', 'ude co', 'de\\b')], 'i');
const AI_IDENTITY = rx([j('cla', 'ude'), j('anthro', 'pic')], 'i');

function die(msg) {
    console.error(`\n${path.basename(process.argv[1], '.js')}: FAIL — ${msg}`);
    process.exit(1);
}
function step(s) {
    console.log(`\n\x1b[1m=== ${path.basename(process.argv[1], '.js')} ${VERSION}: ${s}\x1b[0m`);
}
function sh(cmd, args, o = {}) {
    return execFileSync(cmd, args, { cwd: REPO, encoding: 'utf8', ...o }).trim();
}
function run(cmd, args, o = {}) {
    console.log(`$ ${[cmd, ...args].join(' ')}`);
    const r = spawnSync(cmd, args, { cwd: REPO, stdio: 'inherit', ...o });
    if (r.status !== 0) throw new Error(`${cmd} ${args.join(' ')} exited ${r.status}${r.signal ? ' (' + r.signal + ')' : ''}`);
}
const sha256 = (f) => crypto.createHash('sha256').update(fs.readFileSync(f)).digest('hex');
const mb = (n) => `${(n / 1048576).toFixed(1)} MB`;
const head = () => sh('git', ['rev-parse', 'HEAD']);
const dirty = () => sh('git', ['status', '--porcelain', '--untracked-files=normal']);
function tagCommit(tag = TAG) {
    try {
        return sh('git', ['rev-parse', '--verify', '--quiet', `refs/tags/${tag}^{commit}`], { stdio: ['ignore', 'pipe', 'ignore'] });
    } catch {
        return '';
    }
}
const readState = (f) => (fs.existsSync(f) ? readJson(f) : null);
const writeState = (f, o) => fs.writeFileSync(f, JSON.stringify(o, null, 2) + '\n');

// "## [x.y.z] - YYYY-MM-DD" section of CHANGELOG.md
function changelogSection(version = VERSION) {
    const lines = fs.readFileSync(path.join(REPO, 'CHANGELOG.md'), 'utf8').split('\n');
    const start = lines.findIndex((l) => l.startsWith(`## [${version}]`));
    if (start < 0) return null;
    let end = lines.findIndex((l, i) => i > start && /^## \[/.test(l));
    if (end < 0) end = lines.length;
    return { heading: lines[start], body: lines.slice(start + 1, end).join('\n').trim() };
}
function versionProblems() {
    const problems = [];
    if (!/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/.test(VERSION)) problems.push(`package.json version ${VERSION} is not a semver`);
    const notes = changelogSection();
    if (!notes) problems.push(`CHANGELOG.md has no "## [${VERSION}] - <date>" section`);
    else if (!/\d{4}-\d{2}-\d{2}/.test(notes.heading)) problems.push(`CHANGELOG.md heading is not dated: "${notes.heading}"`);
    return problems;
}
// The build's identity: the inputs whose bytes end up in dist/. A commit that changes only release tooling keeps it valid.
const PACKAGING_ONLY = [/^scripts\/release\//];
function changedSince(commit) {
    if (!commit) return ['(no build commit)'];
    const out = sh('git', ['diff', '--name-only', commit, 'HEAD']);
    return out ? out.split('\n').filter((f) => !PACKAGING_ONLY.some((re) => re.test(f))) : [];
}
// dist/ belongs to HEAD when the build commit equals HEAD or differs only in release tooling
function distProblems() {
    const p = [];
    const b = readState(D.build);
    if (!b) return ['dist/ has no BUILD.json (npm run build)'];
    if (b.version !== VERSION) p.push(`dist/ is ${b.version}, package.json is ${VERSION}`);
    if (!fs.existsSync(D.tgz)) p.push(`dist/${TGZ_NAME} is missing`);
    else if (sha256(D.tgz) !== b.sha256) p.push('dist/ tarball changed after the build');
    const changed = changedSince(b.head);
    if (changed.length) p.push(`HEAD changed shipped inputs since the build (${changed.slice(0, 5).join(', ')}${changed.length > 5 ? ', …' : ''}): rebuild`);
    return p;
}

module.exports = {
    REPO, KEY, NPM_NAME, GH_REPO, RELEASE_BRANCH, ROOT_PKG, VERSION, TAG, DIST, TGZ_NAME, D, CACHE, SCRATCH, SHIP, MANIFEST_FIELDS,
    INTERNAL_TERMS, PUSH_TERMS, PUSH_FILES, CREDIT, AI_IDENTITY, die, step, sh, run, sha256, mb, head, dirty, tagCommit, readState, writeState, changelogSection, versionProblems,
    distProblems, readJson,
};
