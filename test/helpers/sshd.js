'use strict';
// A throwaway OpenSSH server for the remote-facade tests: runs as the current
// user on a free 127.0.0.1 port, with its OWN host key, client key and
// authorized_keys in a temp dir. Nothing under ~/.ssh is read or written — the
// client runs with `-F /dev/null`, an explicit identity and no known_hosts.

const fs = require('fs');
const os = require('os');
const path = require('path');
const net = require('net');
const { spawn, execFileSync } = require('child_process');
const { quote } = require('../../src/access/shell');

const SSHD = '/usr/sbin/sshd';

function sshdAvailable() {
    try {
        fs.accessSync(SSHD, fs.constants.X_OK);
        execFileSync('sh', ['-c', 'command -v ssh && command -v ssh-keygen'], { stdio: 'ignore' });
        return true;
    } catch {
        return false;
    }
}

function freePort() {
    return new Promise((resolve, reject) => {
        const srv = net.createServer();
        srv.once('error', reject);
        srv.listen(0, '127.0.0.1', () => {
            const { port } = srv.address();
            srv.close(() => resolve(port));
        });
    });
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function startSshd() {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'okcode-sshd-'));
    const hostKey = path.join(dir, 'host_ed25519');
    const clientKey = path.join(dir, 'client_ed25519');
    const authorized = path.join(dir, 'authorized_keys');
    const cfg = path.join(dir, 'sshd_config');
    const log = path.join(dir, 'sshd.log');
    const pidFile = path.join(dir, 'sshd.pid');

    execFileSync('ssh-keygen', ['-q', '-t', 'ed25519', '-N', '', '-f', hostKey]);
    execFileSync('ssh-keygen', ['-q', '-t', 'ed25519', '-N', '', '-f', clientKey]);
    fs.copyFileSync(`${clientKey}.pub`, authorized);
    fs.chmodSync(authorized, 0o600);

    const port = await freePort();
    fs.writeFileSync(
        cfg,
        [
            `Port ${port}`,
            'ListenAddress 127.0.0.1',
            `HostKey ${hostKey}`,
            `AuthorizedKeysFile ${authorized}`,
            'PasswordAuthentication no',
            'KbdInteractiveAuthentication no',
            'UsePAM no',
            `PidFile ${pidFile}`,
            'StrictModes no',
            '',
        ].join('\n'),
    );

    // -D: stay in the foreground so stop() is a plain kill of this child.
    const daemon = spawn(SSHD, ['-D', '-f', cfg, '-E', log], { stdio: 'ignore' });
    let exited = false;
    daemon.on('exit', () => (exited = true));

    const sshArgs = [
        '-F',
        '/dev/null',
        '-p',
        String(port),
        '-i',
        clientKey,
        '-o',
        'IdentitiesOnly=yes',
        '-o',
        'BatchMode=yes',
        '-o',
        'StrictHostKeyChecking=no',
        '-o',
        'UserKnownHostsFile=/dev/null',
        '-o',
        'LogLevel=ERROR',
        '127.0.0.1',
    ];

    // ssh joins its arguments into one remote command line, so the script is
    // quoted for the remote shell — the only quoting the transport needs.
    const run = (script, stdin) =>
        new Promise((resolve, reject) => {
            const p = spawn('ssh', [...sshArgs, `bash --noprofile --norc -c ${quote(script)}`]);
            const out = [];
            let err = '';
            p.stdout.on('data', (d) => out.push(d));
            p.stderr.on('data', (d) => (err += d));
            p.on('error', reject);
            p.on('close', (code) => {
                if (code === 0) resolve(Buffer.concat(out).toString('utf8'));
                else reject(new Error(err.trim() || `ssh exit ${code}`));
            });
            p.stdin.on('error', () => {});
            p.stdin.end(stdin ?? '');
        });

    const stop = async () => {
        if (!exited) {
            daemon.kill('SIGTERM');
            for (let i = 0; i < 50 && !exited; i++) await sleep(20);
        }
        fs.rmSync(dir, { recursive: true, force: true });
    };

    // Ready when a real command round-trips.
    let lastErr = null;
    for (let i = 0; i < 100; i++) {
        if (exited) break;
        try {
            if ((await run('echo ready', '')).trim() === 'ready') return { run, stop, port };
        } catch (err) {
            lastErr = err;
        }
        await sleep(50);
    }
    let logText = '';
    try {
        logText = fs.readFileSync(log, 'utf8');
    } catch {}
    await stop();
    throw new Error(`sshd did not come up: ${lastErr && lastErr.message}\n${logText}`);
}

module.exports = { startSshd, sshdAvailable, SSHD };
