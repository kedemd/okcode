'use strict';
// The shell facade over the native shell facade. `run(script, stdin) → Promise<stdout>` is
// the host's transport: native worker threads post each call here and wait for the answer.
const native = require('../native/native');
const { pathChecker } = require('./common');
const { methods } = require('./_facade');
const { pure } = require('../native/pure');

// Only the names/dialect pairs; the scripts themselves live in src/access/scripts and are
// embedded in the native binary.
const DIALECTS = { bash: { pathDialect: 'posix' }, powershell: { pathDialect: 'windows' } };

const quote = pure('access/shell')('quote');

function localRunner(dialect = 'bash') {
    if (!DIALECTS[dialect]) throw new Error(`unknown shell dialect: ${dialect}`);
    return (script, stdin) =>
        native.facadeLocalRun(dialect, String(script), stdin == null ? '' : String(stdin)).catch((e) => {
            throw new Error(e && e.message !== undefined ? e.message : String(e));
        });
}

function shell({ root, run, dialect = 'bash' } = {}) {
    const d = DIALECTS[dialect];
    if (!d) throw new Error(`unknown shell dialect: ${dialect}`);
    if (typeof root !== 'string' || !root) throw new Error('shell({ root }): root is required');
    if (typeof run !== 'function') throw new Error('shell({ run }): run(script, stdin) is required');
    const checkPath = pathChecker(d.pathDialect);
    const dispatch = (id, kind, a) => {
        Promise.resolve()
            .then(() => run(a.script, a.stdin))
            .then(
                (out) => {
                    try {
                        native.facadeReply(id, null, String(out), null);
                    } catch (err) {
                        native.facadeReply(id, String((err && err.message) || err), null, null);
                    }
                },
                (e) => native.facadeReply(id, String((e && e.message) || e), null, null),
            );
    };
    const nf = native.facadeShell({ root, dialect }, dispatch);
    const m = methods(nf, checkPath);
    async function filesContaining(paths, text, { ignoreCase = false } = {}) {
        const names = await m.call('filesContaining', { paths, text: String(text ?? ''), ignoreCase: !!ignoreCase });
        return names ? new Set(names) : null;
    }
    return {
        kind: 'shell',
        dialect,
        root,
        checkPath,
        list: m.list,
        stat: m.stat,
        read: m.read,
        commit: m.commit,
        lock: m.lock,
        remove: m.remove,
        exec: m.exec,
        filesContaining,
    };
}

shell.quote = quote;
shell.localRunner = localRunner;
shell.DIALECTS = DIALECTS;

module.exports = shell;
module.exports.shell = shell;
module.exports.quote = quote;
module.exports.localRunner = localRunner;
