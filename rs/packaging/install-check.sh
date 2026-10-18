#!/usr/bin/env bash
# The install check (release plan C6): a bare consumer in a clean Debian bookworm container (glibc 2.36, the brain image)
# npm-installs okdb's tarball + okcode's and exercises every entry point: library mode (own store), a host-passed okdb v3 db,
# `npx okcode` (bin/okcode.js), and the bundled standalone binary. Local only: nothing is published.
#
#   rs/packaging/install-check.sh <kedem-okdb-3.x.tgz> <kedem-okcode-1.x.tgz>
set -euo pipefail
OKDB="${1:?okdb tarball or npm spec}"; OKCODE=$(readlink -f "${2:?okcode tarball}")
# a file is copied in; anything else (e.g. @kedem/okdb@3.0.0) is installed from the registry
if [ -f "$OKDB" ]; then OKDB=$(readlink -f "$OKDB"); OKDB_ARG="./$(basename "$OKDB")"; else OKDB_ARG="$OKDB"; fi
IMAGE="${TEST_IMAGE:-node:24-bookworm-slim}"
W="${WORK:-$HOME/.cache/okcode-rs/install-check}"
rm -rf "$W"; mkdir -p "$W/proj/lib" "$W/npm-cache"
# npm's cache seeded from the host's (prefer-offline: okdb's few JS deps come from it when present)
[ -d "$HOME/.npm/_cacache" ] && cp -al "$HOME/.npm/_cacache" "$W/npm-cache/_cacache" 2>/dev/null || true
printf 'export function retryWithBackoff(fn, n) {\n  return fn();\n}\nexport const TAX = 0.17;\n' > "$W/proj/lib/a.js"
echo '{"name":"consumer","version":"1.0.0","private":true}' > "$W/package.json"
cat > "$W/check.js" <<'JS'
const path = require('path');
const okcode = require('@kedem/okcode');
const OKDB = require('@kedem/okdb');
(async () => {
    const root = path.join(__dirname, 'proj');
    const oc = await okcode.open({ path: path.join(__dirname, 'store-own') });
    const ws = await oc.addWorkspace('app', { access: okcode.access.localFs(root) });
    const hits = await ws.find('retryWithBackoff');
    if (!hits.length || hits[0].file !== 'lib/a.js') throw new Error('own store: find failed');
    await oc.close();
    const db = new OKDB(path.join(__dirname, 'store-host'));
    await db.open();
    const oc2 = await okcode.open({ db });
    const ws2 = await oc2.addWorkspace('app', { access: okcode.access.localFs(root) });
    const r = await ws2.read('retryWithBackoff');
    if (!r.ok) throw new Error('host db: read failed');
    if ((await ws2.grep('TAX')).total.lines !== 1) throw new Error('host db: grep failed');
    await oc2.close();
    await db.close();
    console.log('CHECK library: own store + host db OK');
})().catch((e) => { console.error('CHECK FAIL', e.code, e.message); process.exit(1); });
JS
[ -f "$OKDB" ] && cp "$OKDB" "$W/"; cp "$OKCODE" "$W/"
docker run --rm --user "$(id -u):$(id -g)" -v "$W:/w" -w /w -e HOME=/w -e npm_config_cache=/w/npm-cache "$IMAGE" sh -c '
    set -e
    ldd --version | head -1; node --version
    npm install --no-audit --no-fund --prefer-offline '"$OKDB_ARG"' ./'"$(basename "$OKCODE")"' >npm.log 2>&1 || { tail -20 npm.log; exit 1; }
    node -p "\"CHECK okdb from npm: \" + require(\"@kedem/okdb/package.json\").version" 2>/dev/null
    node check.js 2>&1 | grep CHECK
    npx --no-install okcode find TAX --root proj --store store-cli-js 2>/dev/null | grep -q "lib/a.js" && echo "CHECK npx okcode OK"
    node_modules/@kedem/okcode/native/linux-x64-gnu/okcode find TAX --json --root proj --store store-cli-rs 2>/dev/null | grep -q "\"file\": \"lib/a.js\"" && echo "CHECK standalone okcode binary OK"
'
echo "install-check: done ($IMAGE)"
