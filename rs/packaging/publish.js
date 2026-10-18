#!/usr/bin/env node
'use strict';
// npm run publish — ships dist/, exactly as built, checked and tested. Run it in your own terminal: it asks you to type the version,
// and npm itself asks for the OTP.
//
// Refuses unless: clean tree on the release branch (OKCODE_RELEASE_BRANCH, default master), tag v<version> on HEAD, dist/CHECKED and
// dist/TESTED for this build (same sha256 as the tarball), and the PUSH SCAN finds nothing: every commit that the push would add to
// the public repo (origin/<branch>..HEAD) — its message and every file it adds or changes — is scanned for internal notes, machine
// paths, license or key material, .env files, and any credit to an AI assistant (Co-Authored-By trailers, "Generated with …",
// anthropic addresses); every commit's author and committer must be the repo's git user.
// Then it shows what each step will do (from the live remote state), asks for the typed version, and runs:
//   1. git push origin <branch>         fast-forward only, never forced           (OKCODE_PUBLISH_GIT=source, the default)
//   2. git push origin v<version>       that one tag, never --tags                 (OKCODE_PUBLISH_GIT=source)
//   3. npm publish dist/<tarball>       --access public (a prerelease: --tag next); npm prompts for the OTP
//   4. wait until the registry serves <version> (polls out the registry lag)
// OKCODE_PUBLISH_GIT=none skips 1–2 (npm only). Every step first checks whether it is already done and skips it, so after a failure:
// fix the cause, re-run npm run publish.
const fs = require('node:fs');
const path = require('node:path');
const readline = require('node:readline');
const { spawnSync } = require('node:child_process');
const L = require('./release-lib');
const { REPO, VERSION, TAG, NPM_NAME, D, die, step, sh, run, sha256, mb } = L;

const BRANCH = process.env.OKCODE_RELEASE_BRANCH || 'master';
const GIT = process.env.OKCODE_PUBLISH_GIT || 'source';
if (!['source', 'none'].includes(GIT)) die(`OKCODE_PUBLISH_GIT=${GIT}: use source (push the branch and tag) or none (npm only)`);
const PRE = VERSION.includes('-');

const { PUSH_TERMS, PUSH_FILES, CREDIT, AI_IDENTITY } = L;

// the one identity allowed as author and committer of a pushed commit (the repo's own git user)
const IDENTITY = (() => {
    try {
        return `${sh('git', ['config', 'user.name'])} <${sh('git', ['config', 'user.email'])}>`;
    } catch {
        return '';
    }
})();

function pushScan(base) {
    const range = base ? `${base}..HEAD` : 'HEAD';
    const commits = sh('git', ['rev-list', '--reverse', range]).split('\n').filter(Boolean);
    const hits = [];
    for (const c of commits) {
        const msg = sh('git', ['log', '-1', '--format=%B', c]);
        msg.split('\n').forEach((l) => (PUSH_TERMS.test(l) || CREDIT.test(l)) && hits.push(`${c.slice(0, 9)} message: ${l.trim().slice(0, 120)}`));
        const who = sh('git', ['log', '-1', '--format=%an <%ae>|%cn <%ce>', c]);
        for (const id of who.split('|')) {
            if (AI_IDENTITY.test(id) || (IDENTITY && id !== IDENTITY)) hits.push(`${c.slice(0, 9)} author/committer: ${id} (expected ${IDENTITY || 'a human'})`);
        }
        const changed = sh('git', ['diff-tree', '--root', '--no-commit-id', '-r', '--diff-filter=AMR', '--name-only', c]).split('\n').filter(Boolean);
        for (const f of changed) {
            if (PUSH_FILES.test(f)) hits.push(`${c.slice(0, 9)} file: ${f}`);
            const blob = spawnSync('git', ['show', `${c}:${f}`], { cwd: REPO, maxBuffer: 1 << 30 });
            if (blob.status !== 0 || blob.stdout.includes(0)) continue; // binary
            blob.stdout.toString('utf8').split('\n').forEach((l, i) => (PUSH_TERMS.test(l) || CREDIT.test(l)) && hits.push(`${c.slice(0, 9)} ${f}:${i + 1}: ${l.trim().slice(0, 120)}`));
        }
    }
    return { commits: commits.length, hits };
}

const lsRemote = (ref) => {
    const r = spawnSync('git', ['ls-remote', 'origin', ref], { cwd: REPO, encoding: 'utf8' });
    return r.status === 0 ? r.stdout.split(/\s+/)[0] || '' : null;
};
const npmHas = () => spawnSync('npm', ['view', `${NPM_NAME}@${VERSION}`, 'version'], { encoding: 'utf8' }).stdout.trim() === VERSION;

step('1. preflight');
const problems = L.versionProblems();
if (L.dirty()) problems.push(`working tree is not clean:\n${L.dirty()}`);
const branch = sh('git', ['rev-parse', '--abbrev-ref', 'HEAD']);
if (branch !== BRANCH) problems.push(`on branch ${branch}, the release branch is ${BRANCH} (OKCODE_RELEASE_BRANCH)`);
const HEAD = L.head();
if (L.tagCommit() !== HEAD) problems.push(`tag ${TAG} is not on HEAD (git tag -f ${TAG})`);
problems.push(...L.distProblems());
const sha = fs.existsSync(D.tgz) ? sha256(D.tgz) : '';
for (const [name, f] of [['CHECKED', D.checked], ['TESTED', D.tested]]) {
    const s = L.readState(f);
    if (!s) problems.push(`dist/${name} is missing (npm run ${name === 'CHECKED' ? 'check' : 'test:dist'})`);
    else if (s.sha256 !== sha) problems.push(`dist/${name} is for another tarball (${s.sha256.slice(0, 12)})`);
}
if (problems.length) die(`\n- ${problems.join('\n- ')}`);

let remoteBranch = null;
let remoteTag = null;
if (GIT === 'source') {
    step('2. push scan (everything the push adds to the public repo)');
    remoteBranch = lsRemote(`refs/heads/${BRANCH}`);
    if (remoteBranch === null) die('cannot reach origin');
    if (remoteBranch && spawnSync('git', ['merge-base', '--is-ancestor', remoteBranch, HEAD], { cwd: REPO }).status !== 0) {
        if (spawnSync('git', ['cat-file', '-e', remoteBranch], { cwd: REPO }).status !== 0) die(`origin/${BRANCH} (${remoteBranch.slice(0, 9)}) is not in this repo: git fetch origin`);
        die(`origin/${BRANCH} is not an ancestor of HEAD: the push would not be a fast-forward`);
    }
    const scan = pushScan(remoteBranch);
    console.log(`${scan.commits} commit(s) to push`);
    if (scan.hits.length) die(`the push scan found ${scan.hits.length} hit(s):\n  ${scan.hits.slice(0, 40).join('\n  ')}`);
    console.log('push scan: clean');
    remoteTag = lsRemote(`refs/tags/${TAG}`);
}

step('3. plan');
console.log(`  package   ${NPM_NAME}@${VERSION}  ${path.basename(D.tgz)}  ${mb(fs.statSync(D.tgz).size)}  sha256 ${sha}`);
if (GIT === 'source') {
    console.log(`  branch    origin/${BRANCH}: ${remoteBranch ? remoteBranch.slice(0, 9) : '(new)'} -> ${HEAD.slice(0, 9)}${remoteBranch === HEAD ? '  (already there: skip)' : ''}`);
    console.log(`  tag       ${TAG} -> ${HEAD.slice(0, 9)}${remoteTag === HEAD ? '  (already there: skip)' : remoteTag ? `  (origin has it at ${remoteTag.slice(0, 9)}: REFUSED)` : ''}`);
    if (remoteTag && remoteTag !== HEAD) die(`origin already has ${TAG} on another commit`);
} else console.log('  git       not pushed (OKCODE_PUBLISH_GIT=none)');
const already = npmHas();
console.log(`  npm       ${already ? 'already published: skip' : `npm publish --access public${PRE ? ' --tag next' : ''} (npm asks for the OTP)`}`);

(async () => {
    const rl = readline.createInterface({ input: process.stdin, output: process.stdout });
    const typed = await new Promise((res) => rl.question(`\nType the version (${VERSION}) to publish: `, res));
    rl.close();
    if (typed.trim() !== VERSION) die('not confirmed');

    if (GIT === 'source') {
        step('4. git push');
        if (remoteBranch !== HEAD) run('git', ['push', 'origin', `HEAD:refs/heads/${BRANCH}`]);
        if (remoteTag !== HEAD) run('git', ['push', 'origin', `refs/tags/${TAG}`]);
    }
    step('5. npm publish');
    if (!npmHas()) run('npm', ['publish', D.tgz, '--access', 'public', ...(PRE ? ['--tag', 'next'] : [])], { cwd: D.dir });
    step('6. registry');
    for (let i = 0; i < 60 && !npmHas(); i++) await new Promise((r) => setTimeout(r, 5000));
    if (!npmHas()) die(`the registry does not serve ${VERSION} yet: re-run npm run publish to finish`);
    L.writeState(D.published, { version: VERSION, head: HEAD, sha256: sha, git: GIT, published: new Date().toISOString() });
    console.log(`\npublish: ${NPM_NAME}@${VERSION} is live${GIT === 'source' ? `; ${BRANCH} and ${TAG} pushed` : ''}`);
})().catch((e) => die(e.message));
