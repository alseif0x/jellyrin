#!/usr/bin/env node

const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const { spawn } = require('node:child_process');

const repoRoot = path.resolve(__dirname, '..');
const binary = path.join(repoRoot, 'target', 'debug', 'jellyrin-migrate');
const wrapper = path.join(repoRoot, 'ops', 'audit-runtime-hygiene.sh');
const canaries = ['user-canary', 'password-canary', 'token-canary', 'provider.invalid'];

async function main() {
  const build = await run('cargo', ['+1.94.0', 'build', '--locked', '-p', 'jellyrin-migrate']);
  requireCode(build, 0, 'build');
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'jellyrin-runtime-hygiene-'));
  try {
    const cleanLog = path.join(root, 'clean.log');
    const dirtyLog = path.join(root, 'dirty.log');
    const cleanArgv = path.join(root, 'clean.cmdline');
    await fs.writeFile(cleanLog, 'selected internal relay\n');
    await fs.writeFile(
      dirtyLog,
      'upstream https://user-canary:password-canary@provider.invalid/live/user-canary/password-canary/42?token=token-canary\n',
    );
    await fs.writeFile(
      cleanArgv,
      Buffer.from('/usr/local/bin/ffmpeg\0-i\0http://127.0.0.1:8096/.jellyrin/internal/remote-media/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\0'),
    );

    const cleanReport = path.join(root, 'clean.json');
    const clean = await run(binary, [
      'audit-runtime-hygiene', '--log', cleanLog, '--argv', cleanArgv,
      '--relay-port', '8096', '--report', cleanReport,
    ]);
    requireCode(clean, 0, 'clean scanner');
    assertSafe(clean, await fs.readFile(cleanReport, 'utf8'));

    const dirtyReport = path.join(root, 'dirty.json');
    const dirty = await run(binary, [
      'audit-runtime-hygiene', '--log', dirtyLog, '--report', dirtyReport,
    ]);
    requireCode(dirty, 2, 'finding scanner');
    assertSafe(dirty, await fs.readFile(dirtyReport, 'utf8'));

    const symlink = path.join(root, 'linked.log');
    await fs.symlink(cleanLog, symlink);
    const incomplete = await run(binary, ['audit-runtime-hygiene', '--log', symlink]);
    requireCode(incomplete, 3, 'symlink scanner');
    assertSafe(incomplete, '');

    const fakeJournal = path.join(root, 'journalctl');
    const fakeSystemctl = path.join(root, 'systemctl');
    await executable(fakeJournal, '#!/bin/sh\nprintf "journal snapshot clean\\n"\n');
    await executable(fakeSystemctl, '#!/bin/sh\nprintf "/test.slice\\n"\n');
    await fs.mkdir(path.join(root, 'cgroup', 'test.slice'), { recursive: true });
    await fs.writeFile(path.join(root, 'cgroup', 'test.slice', 'cgroup.procs'), '123\n');
    await fs.mkdir(path.join(root, 'proc', '123'), { recursive: true });
    await fs.writeFile(path.join(root, 'proc', '123', 'cmdline'), Buffer.from('/usr/local/bin/jellyrin-server\0'));

    const wrapperReport = path.join(root, 'wrapper.json');
    const wrapperResult = await run(wrapper, [
      '--since', '2026-08-09T00:00:00Z', '--relay-port', '8096',
      '--report', wrapperReport, '--no-default-logs', '--log', cleanLog,
    ], wrapperEnv(root, fakeJournal, fakeSystemctl));
    requireCode(wrapperResult, 0, 'wrapper');
    assertSafe(wrapperResult, await fs.readFile(wrapperReport, 'utf8'));

    const failedJournal = path.join(root, 'journalctl-failed');
    await executable(failedJournal, '#!/bin/sh\nexit 1\n');
    const failedReport = path.join(root, 'wrapper-incomplete.json');
    const failedWrapper = await run(wrapper, [
      '--since', '2026-08-09T00:00:00Z', '--relay-port', '8096',
      '--report', failedReport, '--no-default-logs', '--log', cleanLog,
    ], wrapperEnv(root, failedJournal, fakeSystemctl));
    requireCode(failedWrapper, 3, 'failed journal wrapper');
    assertSafe(failedWrapper, await fs.readFile(failedReport, 'utf8'));

    process.stdout.write('runtime hygiene smoke passed\n');
  } finally {
    await fs.rm(root, { recursive: true, force: true });
  }
}

function wrapperEnv(root, journal, systemctl) {
  return {
    ...process.env,
    JELLYRIN_MIGRATE_BIN: binary,
    JELLYRIN_JOURNALCTL_BIN: journal,
    JELLYRIN_SYSTEMCTL_BIN: systemctl,
    JELLYRIN_PROC_ROOT: path.join(root, 'proc'),
    JELLYRIN_CGROUP_ROOT: path.join(root, 'cgroup'),
    TMPDIR: root,
  };
}

async function executable(file, content) {
  await fs.writeFile(file, content, { mode: 0o700 });
}

function assertSafe(result, report) {
  const output = `${result.stdout}\n${result.stderr}\n${report}`;
  for (const canary of canaries) {
    if (output.includes(canary)) throw new Error(`runtime audit leaked canary category ${canaries.indexOf(canary)}`);
  }
}

function requireCode(result, expected, label) {
  if (result.code !== expected) {
    throw new Error(`${label} exited ${result.code}; stdout=${result.stdout}; stderr=${result.stderr}`);
  }
}

function run(command, args, env = process.env) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { cwd: repoRoot, env, stdio: ['ignore', 'pipe', 'pipe'] });
    const stdout = [];
    const stderr = [];
    child.stdout.on('data', (chunk) => stdout.push(chunk));
    child.stderr.on('data', (chunk) => stderr.push(chunk));
    child.on('error', reject);
    child.on('close', (code) => resolve({
      code,
      stdout: Buffer.concat(stdout).toString('utf8'),
      stderr: Buffer.concat(stderr).toString('utf8'),
    }));
  });
}

main().catch((error) => {
  console.error(error.message);
  process.exitCode = 1;
});
