// Minimal WHATWG-ish URL for QuickJS (no URL global there). okjs's import resolution
// only needs `new URL(spec, base).href` and `new URL(abs)` validity for hierarchical URLs.
(function () {
    const SPECIAL = { 'http:': 80, 'https:': 443, 'ftp:': 21, 'ws:': 80, 'wss:': 443, 'file:': null };
    const enc = (s) => s.replace(/[^\x21-\x7e]|["<>`{}]/g, (c) => encodeURIComponent(c));
    function parseAbs(str) {
        const m = /^([a-zA-Z][a-zA-Z\d+.-]*:)(.*)$/s.exec(str);
        if (!m) return null;
        const scheme = m[1].toLowerCase();
        let rest = m[2], hash = '', query = '';
        let i = rest.indexOf('#');
        if (i >= 0) { hash = rest.slice(i); rest = rest.slice(0, i); }
        i = rest.indexOf('?');
        if (i >= 0) { query = rest.slice(i); rest = rest.slice(0, i); }
        const special = scheme in SPECIAL;
        if (special) rest = rest.replace(/\\/g, '/');
        let host = '', path = rest, hasAuth = false;
        if (/^\/\//.test(rest) || (special && scheme !== 'file:')) {
            hasAuth = true;
            rest = special && scheme !== 'file:' ? rest.replace(/^\/*/, '') : rest.replace(/^\/\//, '');
            const j = rest.indexOf('/');
            host = j < 0 ? rest : rest.slice(0, j);
            path = j < 0 ? '' : rest.slice(j);
            if (special && scheme !== 'file:' && !host) throw new TypeError('Invalid URL');
            host = host.toLowerCase();
        } else if (scheme === 'file:') { hasAuth = true; }
        if (!hasAuth) return { scheme, opaque: true, path: rest, query, hash, host: '' };
        if (special && !path) path = '/';
        return { scheme, opaque: false, host, path, query, hash, special };
    }
    function normPath(path) {
        const out = [];
        const segs = path.split('/');
        for (let k = 1; k < segs.length; k++) {
            const s = segs[k], last = k === segs.length - 1;
            if (s === '..' || /^%2e%2e$/i.test(s)) { out.pop(); if (last) out.push(''); }
            else if (s === '.' || /^%2e$/i.test(s)) { if (last) out.push(''); }
            else out.push(s);
        }
        return '/' + out.join('/');
    }
    function build(p) {
        if (p.opaque) return p.scheme + p.path + p.query + p.hash;
        const path = p.special || p.path.startsWith('/') ? normPath(p.path) : p.path;
        return p.scheme + '//' + p.host + enc(path) + enc(p.query) + enc(p.hash);
    }
    class URL {
        constructor(input, base) {
            input = String(input).replace(/^[\u0000- ]+|[\u0000- ]+$/g, '').replace(/[\t\n\r]/g, '');
            let p = parseAbs(input);
            if (!p) {
                if (base === undefined) throw new TypeError('Invalid URL');
                const b = base instanceof URL ? base._p : parseAbs(String(base));
                if (!b || b.opaque) throw new TypeError('Invalid URL');
                let rest = input, hash = '', query = '';
                if (b.special) rest = rest.replace(/\\/g, '/');
                let i = rest.indexOf('#');
                if (i >= 0) { hash = rest.slice(i); rest = rest.slice(0, i); }
                i = rest.indexOf('?');
                if (i >= 0) { query = rest.slice(i); rest = rest.slice(0, i); }
                if (rest.startsWith('//')) {
                    p = parseAbs(b.scheme + input);
                } else if (rest.startsWith('/')) {
                    p = { ...b, path: rest, query, hash };
                } else if (rest === '') {
                    p = { ...b, query: i >= 0 && input.indexOf('?') >= 0 ? query : b.query, hash };
                } else {
                    const dir = b.path.slice(0, b.path.lastIndexOf('/') + 1);
                    p = { ...b, path: dir + rest, query, hash };
                }
            }
            this._p = p;
            this.href = build(p);
        }
        toString() { return this.href; }
    }
    globalThis.URL = URL;
})();
