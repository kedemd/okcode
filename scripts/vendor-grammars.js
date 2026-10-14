'use strict';
// Copies each tree-sitter grammar's `.wasm`, its upstream `queries/tags.scm`
// and its license into grammars/<lang>/, and records the versions vendored
// into grammars/manifest.json. See docs/WORKPLAN.md Phase F.
//
// Why vendored rather than loaded from node_modules at runtime: the grammar
// NPM packages (tree-sitter-typescript etc.) run node-gyp-build on install —
// they build NATIVE bindings this project never uses, since web-tree-sitter
// only wants the .wasm they ship alongside. Keeping them as devDependencies
// and committing the handful of files we actually need means a consumer of
// @kedem/okcode installs neither the native toolchain nor the grammar
// packages themselves — only the optional `web-tree-sitter` runtime, if it
// wants symbols at all.
//
// Run with `npm run vendor-grammars` after bumping a grammar's devDependency
// version; re-run adds nothing new unless the source files actually moved
// (the committed diff IS the review).

const fs = require('fs');
const path = require('path');

const ROOT = path.join(__dirname, '..');
const OUT = path.join(ROOT, 'grammars');

// A language = where its npm package's files live, and which files this
// project actually ships. `tagsLocal` is kept alongside (not generated) —
// this script never overwrites it.
const GRAMMARS = {
    typescript: {
        pkg: 'tree-sitter-typescript',
        files: {
            'tree-sitter-typescript.wasm': 'tree-sitter-typescript.wasm',
            'tree-sitter-tsx.wasm': 'tree-sitter-tsx.wasm',
            'queries/tags.scm': 'tags.scm',
            LICENSE: 'LICENSE',
        },
    },
    python: {
        pkg: 'tree-sitter-python',
        files: {
            'tree-sitter-python.wasm': 'tree-sitter-python.wasm',
            'queries/tags.scm': 'tags.scm',
            LICENSE: 'LICENSE',
        },
    },
    go: {
        pkg: 'tree-sitter-go',
        files: {
            'tree-sitter-go.wasm': 'tree-sitter-go.wasm',
            'queries/tags.scm': 'tags.scm',
            LICENSE: 'LICENSE',
        },
    },
    rust: {
        pkg: 'tree-sitter-rust',
        files: {
            'tree-sitter-rust.wasm': 'tree-sitter-rust.wasm',
            'queries/tags.scm': 'tags.scm',
            LICENSE: 'LICENSE',
        },
    },
    // tree-sitter-php ships TWO grammars: `php` (PHP with embedded HTML,
    // parsed the way an actual file on disk reads — `<?php ... ?>` tags
    // and all) and `php_only` (expects the `<?php` stripped already, for
    // an eval()'d snippet). A real .php/.phtml file can carry inline HTML
    // at any point, so `php` is the one that will not choke on an
    // ordinary template; `queries/tags.scm` is written against it.
    php: {
        pkg: 'tree-sitter-php',
        files: {
            'tree-sitter-php.wasm': 'tree-sitter-php.wasm',
            'queries/tags.scm': 'tags.scm',
            LICENSE: 'LICENSE',
        },
    },
};

function main() {
    const manifest = {};
    for (const [lang, cfg] of Object.entries(GRAMMARS)) {
        const pkgDir = path.join(ROOT, 'node_modules', cfg.pkg);
        const pkgJson = JSON.parse(fs.readFileSync(path.join(pkgDir, 'package.json'), 'utf8'));
        const destDir = path.join(OUT, lang);
        fs.mkdirSync(destDir, { recursive: true });
        for (const [from, to] of Object.entries(cfg.files)) {
            const src = path.join(pkgDir, from);
            if (!fs.existsSync(src)) throw new Error(`${cfg.pkg}: missing ${from} — grammar package shape changed?`);
            fs.copyFileSync(src, path.join(destDir, to));
        }
        manifest[lang] = { package: cfg.pkg, version: pkgJson.version };
        console.log(`vendored ${lang} <- ${cfg.pkg}@${pkgJson.version}`);
    }
    fs.writeFileSync(path.join(OUT, 'manifest.json'), `${JSON.stringify(manifest, null, 4)}\n`);
}

main();
