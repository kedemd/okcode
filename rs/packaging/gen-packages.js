#!/usr/bin/env node
'use strict';
// Release plan C6: lay out the per-platform npm packages from built binaries. Writes nothing outside --out.
//
//   node rs/packaging/gen-packages.js --out DIR --version X.Y.Z --key linux-x64 --addon <libokcode_node.so|.dylib|.dll> --cli <okcode[.exe]>
//   node rs/packaging/gen-packages.js --out DIR --version X.Y.Z --main      # the main package's optionalDependencies, printed
//
// Each package: @kedem/okcode-<key>/{package.json, okcode.node, bin/okcode[.exe], README.md}, with os/cpu/libc so npm installs
// only the matching one. Nothing is published here.
const fs = require('fs');
const path = require('path');

const a = {};
for (let i = 2; i < process.argv.length; i++) if (process.argv[i].startsWith('--')) a[process.argv[i].slice(2)] = process.argv[i + 1]?.startsWith('--') ? true : process.argv[i + 1] ?? true;
const T = JSON.parse(fs.readFileSync(path.join(__dirname, 'targets.json'), 'utf8'));
const need = (k) => {
    if (!a[k] || a[k] === true) throw new Error(`gen-packages: --${k} is required`);
    return a[k];
};
const name = (t) => `${T.scope}/${T.prefix}-${t.key}`;

if (a.main) {
    const version = need('version');
    console.log(JSON.stringify({ optionalDependencies: Object.fromEntries(T.targets.map((t) => [name(t), version])) }, null, 4));
    process.exit(0);
}

const out = need('out');
const version = need('version');
const t = T.targets.find((x) => x.key === need('key'));
if (!t) throw new Error(`gen-packages: unknown --key ${a.key} (targets.json)`);
const dir = path.join(out, `${T.prefix}-${t.key}`);
fs.mkdirSync(path.join(dir, 'bin'), { recursive: true });
fs.copyFileSync(need('addon'), path.join(dir, 'okcode.node'));
const exe = t.os === 'win32' ? 'okcode.exe' : 'okcode';
fs.copyFileSync(need('cli'), path.join(dir, 'bin', exe));
if (t.os !== 'win32') fs.chmodSync(path.join(dir, 'bin', exe), 0o755);
const pkg = {
    name: name(t),
    version,
    description: `okcode native binaries for ${t.key} (the addon and the standalone CLI). Installed automatically by @kedem/okcode.`,
    license: 'MIT',
    os: [t.os],
    cpu: [t.cpu],
    ...(t.libc ? { libc: [t.libc] } : {}),
    files: ['okcode.node', `bin/${exe}`, 'README.md'],
    bin: { okcode: `bin/${exe}` },
    main: 'okcode.node',
};
fs.writeFileSync(path.join(dir, 'package.json'), JSON.stringify(pkg, null, 4) + '\n');
fs.writeFileSync(path.join(dir, 'README.md'), `# ${pkg.name}\n\n${pkg.description}\n`);
console.log(dir);
