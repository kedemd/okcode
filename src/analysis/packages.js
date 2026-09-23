'use strict';
// The dependency SURFACE — what the things a project depends on expose.
//
// Full depth for the workspace, surface only for dependencies. This project is
// 247 files against 5,136 in node_modules / 159 MB, and indexing that to
// workspace depth is a research project that never pays for itself. What DOES
// pay for itself is the one question dependencies actually raise, and the one
// that has cost this project real cycles: *does okdb expose createPipeline*.
// Answering it means reading a package.json and a type declaration — two small
// files — not walking a tree.
//
// So: the declared dependencies are read from the root package.json (that is
// the set anyone asks about), and any other package can still be looked up by
// name on demand, because a transitive dependency is a fair question even if
// it is not worth indexing 5,000 files to anticipate.

const { extract } = require('./parse');

// A declaration file is the best answer when there is one: it is the package's
// own statement of its surface, rather than an inference from its code.
const DTS_EXPORT =
    /^\s*export\s+(?:declare\s+)?(?:async\s+)?(?:function|class|const|let|var|type|interface|enum|namespace)\s+([A-Za-z_$][\w$]*)/gm;
const DTS_LIST = /^\s*export\s*\{([^}]*)\}/gm;

function namesFromDts(text) {
    const out = new Set();
    for (const m of text.matchAll(DTS_EXPORT)) out.add(m[1]);
    for (const m of text.matchAll(DTS_LIST)) {
        for (const part of m[1].split(',')) {
            const name = part
                .trim()
                .split(/\s+as\s+/)
                .pop()
                .trim();
            if (name && /^[A-Za-z_$][\w$]*$/.test(name)) out.add(name);
        }
    }
    if (/^\s*export\s+default\b/m.test(text)) out.add('default');
    return [...out];
}

// The MEMBERS of a declared class or interface, as `Type.member`.
//
// Top-level exports alone do not answer the question this exists for. okdb's
// index.d.ts exports exactly two names — `OKDB` and `default` — and the thing
// anyone actually asks is whether `createPipeline` is callable, which is a
// member of an interface in another file. A list of two names would have
// looked like a confident no.
function membersFromDts(text) {
    const count = (line, ch) => (line.match(ch) || []).length;
    const out = [];
    let owner = null;
    let depth = 0; // braces: inside the declaration body
    let open = 0; // parens: inside a parameter list
    for (const line of text.split('\n')) {
        if (!owner) {
            const decl =
                /^\s*(?:export\s+)?(?:declare\s+)?(?:abstract\s+)?(?:class|interface)\s+([A-Za-z_$][\w$]*)/.exec(line);
            if (decl) {
                owner = decl[1];
                depth = 0;
                open = 0;
            }
        }
        if (!owner) continue;
        depth += count(line, /\{/g) - count(line, /\}/g);
        // A member is an indented identifier followed by a call signature, a
        // type annotation or a generic. A PARAMETER on a wrapped signature
        // looks exactly the same — so a line is only a member when it does not
        // start inside an open parameter list. Without this, `addPipeline(`
        // followed by `name: string,` put `name` in the package's surface as
        // though it were a method. (Judging by the line's last character
        // instead is style-dependent: it cost acorn 289 of its 293 members,
        // because that package writes its members without semicolons.)
        const member = /^\s+(?:readonly\s+|static\s+|abstract\s+|async\s+|public\s+)*([A-Za-z_$][\w$]*)\s*[(<?:]/.exec(
            line,
        );
        if (member && depth > 0 && open === 0) out.push(`${owner}.${member[1]}`);
        open += count(line, /\(/g) - count(line, /\)/g);
        if (open < 0) open = 0;
        if (depth <= 0 && line.includes('}')) owner = null;
    }
    return out;
}

// Where a declaration file hands the question on: `export * from './x'`,
// `export type { A } from './features/y'`. One level of following is what
// turns a two-name surface into the real one; more than that is a type
// checker, which is not what this is.
function reExports(text) {
    const out = new Set();
    for (const m of text.matchAll(/\bfrom\s+'(\.[^']+)'/g)) out.add(m[1]);
    for (const m of text.matchAll(/\bfrom\s+"(\.[^"]+)"/g)) out.add(m[1]);
    return [...out];
}

// The declared dependencies, and which list each came from. Read from the
// root package.json rather than by listing node_modules: the declared set is
// what a caller means by "our dependencies", and listing 5,136 entries to
// derive it would cost more than the answer is worth.
function declared(manifest) {
    const out = new Map();
    for (const [field, source] of [
        ['dependencies', 'dependency'],
        ['devDependencies', 'dev'],
        ['optionalDependencies', 'optional'],
        ['peerDependencies', 'peer'],
    ]) {
        for (const name of Object.keys((manifest && manifest[field]) || {})) {
            if (!out.has(name)) out.set(name, source);
        }
    }
    return out;
}

// One package.json, parsed into the fields that answer "how do I use this".
function fromManifest(name, manifest, source) {
    const exportKeys = manifest.exports && typeof manifest.exports === 'object' ? Object.keys(manifest.exports) : [];
    return {
        name,
        version: manifest.version || null,
        description: String(manifest.description || '').slice(0, 200) || null,
        main: manifest.main || null,
        module: manifest.module || null,
        types: manifest.types || manifest.typings || null,
        exportKeys,
        bin: manifest.bin ? (typeof manifest.bin === 'string' ? [name] : Object.keys(manifest.bin)) : [],
        source,
        surface: [],
        surfaceFrom: null,
    };
}

// A workspace-relative path in the facade's canonical form: `/`-separated,
// no `.` segments, no leading `/`, `..` resolved. null when `..` would climb
// above the workspace root — the facade rejects those, so they are never sent.
function normalise(path) {
    const out = [];
    for (const seg of String(path).replace(/\\/g, '/').split('/')) {
        if (!seg || seg === '.') continue;
        if (seg === '..') {
            if (!out.length) return null;
            out.pop();
            continue;
        }
        out.push(seg);
    }
    return out.join('/');
}

const uniq = (list) => [...new Set(list)];

// Where a package states its surface, in the order worth trying. The .d.ts is
// the package's own account of itself; the entry point is an inference from
// code; neither existing is a fact worth recording rather than a failure.
function surfaceCandidates(pkg, dir) {
    // Manifest fields are written './types/index.d.ts' as often as not, and a
    // path with /./ in it is one more thing for a remote shell to disagree
    // about. Normalise once, here.
    // The facade also refuses any path with a `..` segment, so one that
    // climbs out of the package is dropped here rather than sent.
    const at = (rel) => normalise(`${dir}/${String(rel)}`);
    const out = [];
    const push = (path, from) => {
        if (path != null) out.push({ path, from });
    };
    if (pkg.types) push(at(pkg.types), 'types');
    push(at('index.d.ts'), 'types');
    if (pkg.main) push(at(pkg.main), 'main');
    push(at('index.js'), 'main');
    return out;
}

// Three facade round trips for the whole dependency set, whatever its size:
// one for every package.json, one for every surface file, one for the files
// those re-export. Per-package reads would be 300+ round trips for an answer
// nobody is waiting on.
//
// Paths are WORKSPACE-RELATIVE — the facade owns the root — so a package lives
// at `node_modules/<name>`.
async function readPackages(access, names, sourceOf = () => 'transitive') {
    const dirOf = (name) => `node_modules/${name}`;
    const manifests = await access.read([...names].map((n) => `${dirOf(n)}/package.json`));

    const packages = new Map();
    const wanted = [];
    for (const name of names) {
        const buf = manifests.get(`${dirOf(name)}/package.json`);
        if (!buf) continue;
        let manifest;
        try {
            manifest = JSON.parse(buf.toString('utf8'));
        } catch {
            continue;
        }
        const pkg = fromManifest(name, manifest, sourceOf(name));
        packages.set(name, pkg);
        for (const c of surfaceCandidates(pkg, dirOf(name))) wanted.push({ name, ...c });
    }

    // One batch for every candidate; the ones that do not exist simply come
    // back missing, which is cheaper than asking whether each exists first.
    const bodies = wanted.length ? await access.read(uniq(wanted.map((w) => w.path))) : new Map();
    const follow = [];
    for (const w of wanted) {
        const pkg = packages.get(w.name);
        if (!pkg || pkg.surface.length) continue;
        const buf = bodies.get(w.path);
        if (!buf) continue;
        const text = buf.toString('utf8');
        const names_ = w.from === 'types' ? namesFromDts(text) : extract(w.path, text).exports || [];
        if (!names_.length) continue;
        pkg.surface = names_;
        pkg.members = w.from === 'types' ? membersFromDts(text) : [];
        pkg.surfaceFrom = w.from;
        pkg.surfaceFile = w.path.slice(dirOf(w.name).length + 1);
        if (w.from !== 'types') continue;
        const dir = w.path.slice(0, w.path.lastIndexOf('/'));
        for (const spec of reExports(text)) {
            const at = normalise(`${dir}/${spec}`);
            // A specifier that climbs out of the workspace (`../../x`) is not
            // something the facade will read; skip it rather than hand it an
            // escaping path.
            if (at == null) continue;
            // A specifier is written without its extension; both shapes are
            // asked for and the missing one is simply skipped.
            follow.push({ name: w.name, path: /\.d\.ts$/.test(at) ? at : `${at}.d.ts` });
            follow.push({ name: w.name, path: `${at}/index.d.ts` });
        }
    }

    // One more batch, one level deep: the files the entry point delegates to.
    if (follow.length) {
        const deeper = await access.read(uniq(follow.map((f) => f.path)));
        for (const f of follow) {
            const pkg = packages.get(f.name);
            const buf = deeper.get(f.path);
            if (!pkg || !buf) continue;
            const text = buf.toString('utf8');
            pkg.surface.push(...namesFromDts(text));
            pkg.members.push(...membersFromDts(text));
        }
    }
    for (const pkg of packages.values()) {
        pkg.surface = [...new Set(pkg.surface)].slice(0, 500);
        // Deduped BEFORE the cap, and generous: a package's whole surface is
        // a few hundred short strings, and truncating it is how a lookup
        // answers "no" about something that is there.
        pkg.members = [...new Set(pkg.members || [])].slice(0, 1500);
    }
    return packages;
}

// The declared dependency set of a workspace, with each one's surface.
async function scanPackages(access) {
    const manifests = await access.read(['package.json']);
    const buf = manifests.get('package.json');
    if (!buf) return { packages: new Map(), manifest: null, reason: 'no package.json at the workspace root' };
    let manifest;
    try {
        manifest = JSON.parse(buf.toString('utf8'));
    } catch (err) {
        return { packages: new Map(), manifest: null, reason: `package.json does not parse: ${err.message}` };
    }
    const sources = declared(manifest);
    return {
        manifest,
        packages: await readPackages(access, [...sources.keys()], (n) => sources.get(n) || 'transitive'),
    };
}

module.exports = { scanPackages, readPackages, namesFromDts, membersFromDts, reExports, declared };
