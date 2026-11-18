#!/usr/bin/env node
/**
 * Issue #4 stage A: isolated CLI end-to-end checks, no real Agent home writes.
 * Run from the issue-4 worktree, after building the CLI in this worktree:
 *   node scripts/validation/issue-4-stage-a.mjs [--keep]
 * Evidence is sanitized and written to validation/issue-4-stage-a-evidence.json.
 * All disposable fixtures remain under the worktree's ignored .npm-cache/.
 */
import assert from 'node:assert/strict';
import { spawnSync, execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const worktree = fs.realpathSync(process.cwd());
assert(fs.existsSync(path.join(worktree, 'src-tauri/Cargo.toml')), 'run from repository root');
assert(fs.existsSync(path.join(worktree, '.git')), 'run from checkout/worktree root');
const currentBranch = execFileSync('git', ['branch', '--show-current'], { cwd: worktree, encoding: 'utf8' }).trim();
assert.equal(currentBranch, 'test/issue-4-baseline-validation', 'run only in the dedicated Issue #4 validation branch');
const gitDir = fs.realpathSync(path.resolve(worktree, execFileSync('git', ['rev-parse', '--git-dir'], { cwd: worktree, encoding: 'utf8' }).trim()));
const commonDir = fs.realpathSync(path.resolve(worktree, execFileSync('git', ['rev-parse', '--git-common-dir'], { cwd: worktree, encoding: 'utf8' }).trim()));
assert.notEqual(gitDir, commonDir, 'refusing to test in the primary worktree');
const cli = path.join(worktree, 'src-tauri/target/debug/agent-toolchain-manager-cli');
assert(fs.existsSync(cli), `build the worktree-local CLI first: ${cli}`);
const cache = path.join(worktree, '.npm-cache');
fs.mkdirSync(cache, { recursive: true });
const fixture = fs.mkdtempSync(path.join(cache, 'issue4-stage-a-'));
const home = path.join(fixture, 'home');
const source = path.join(fixture, 'source', 'issue4-fixture');
const projectA = path.join(fixture, 'project-a');
const projectB = path.join(fixture, 'project-b');
for (const p of [home, source, projectA, projectB, path.join(home, '.codex/skills/user-fixture'), path.join(home, '.claude/skills/user-fixture'), path.join(home, '.agents/skills/user-fixture'), path.join(fixture, 'tmp'), path.join(fixture, 'xdg-config'), path.join(fixture, 'xdg-data'), path.join(fixture, 'xdg-cache')]) fs.mkdirSync(p, { recursive: true });
fs.writeFileSync(path.join(source, 'SKILL.md'), '---\nname: issue4-fixture\ndescription: Isolated validation fixture\n---\n# Issue 4 fixture\nOriginal source content.\n');
fs.writeFileSync(path.join(source, 'note.txt'), 'source fixture data\n');
for (const agent of ['.codex', '.claude', '.agents']) fs.writeFileSync(path.join(home, agent, 'skills/user-fixture/marker.txt'), `keep-${agent}\n`);

const env = {
  PATH: process.env.PATH || '/usr/bin:/bin',
  HOME: home,
  XDG_CONFIG_HOME: path.join(fixture, 'xdg-config'),
  XDG_DATA_HOME: path.join(fixture, 'xdg-data'),
  XDG_CACHE_HOME: path.join(fixture, 'xdg-cache'),
  TMPDIR: path.join(fixture, 'tmp'),
  LANG: 'C',
  NO_COLOR: '1',
};
const sanitize = (s) => String(s).replaceAll(fixture, '<fixture>').replaceAll(worktree, '<worktree>');
const sha256 = (b) => createHash('sha256').update(b).digest('hex');
const digest = (p) => sha256(fs.readFileSync(p));
function snapshot(root) {
  const items = {};
  function visit(p, rel = '.') {
    if (!fs.existsSync(p) && !fs.existsSync(path.dirname(p))) return;
    const stat = fs.lstatSync(p);
    if (stat.isSymbolicLink()) items[rel] = { kind: 'symlink', target: sanitize(fs.readlinkSync(p)) };
    else if (stat.isFile()) items[rel] = { kind: 'file', sha256: digest(p) };
    else if (stat.isDirectory()) {
      items[rel] = { kind: 'directory' };
      for (const name of fs.readdirSync(p).sort()) visit(path.join(p, name), path.join(rel, name));
    }
  }
  if (fs.existsSync(root)) visit(root);
  return items;
}
const agentsBefore = Object.fromEntries(['.codex', '.claude', '.agents'].map(name => [name, snapshot(path.join(home, name))]));
const commands = [], results = [];
function run(...args) {
  const p = spawnSync(cli, ['--json', ...args], { cwd: worktree, env, encoding: 'utf8', timeout: 30000, maxBuffer: 2 * 1024 * 1024 });
  commands.push({ args: args.map(sanitize), code: p.status, stdout: sanitize(p.stdout || ''), stderr: sanitize(p.stderr || ''), error: p.error?.message });
  if (p.error || p.status !== 0) throw new Error(`${args.join(' ')} returned ${p.status}: ${p.error?.message || p.stderr || p.stdout}`);
  try { return JSON.parse(p.stdout); }
  catch (err) { throw new Error(`Invalid CLI JSON for ${args.join(' ')}: ${err.message}: ${p.stdout}`); }
}
function check(id, cb) {
  try {
    const { status = 'PASS', detail, evidence = {} } = cb();
    results.push({ id, status, detail, evidence });
    console.log(`${id}: ${status} - ${detail}`);
  } catch (err) {
    results.push({ id, status: 'FAIL', detail: sanitize(err.stack || err.message) });
    console.error(`${id}: FAIL - ${sanitize(err.message)}`);
  }
}
const state = {};
const commit = execFileSync('git', ['rev-parse', 'HEAD'], { cwd: worktree, encoding: 'utf8' }).trim();
const baselineGitStatus = execFileSync('git', ['status', '--porcelain=v1'], { cwd: worktree, encoding: 'utf8' });

check('SM-01', () => {
  const repo = run('repo', 'status');
  for (const location of [repo.base_dir, repo.skills_dir, repo.db_path]) {
    assert(path.resolve(location).startsWith(`${home}${path.sep}`), `CLI escaped fixture HOME: ${location}`);
  }
  const install = run('skills', 'install', source, '--local');
  assert.equal(install.ok, true);
  assert.equal(install.synced, false, 'import unexpectedly synced to an Agent');
  assert.equal(install.source_type, 'local');
  assert(install.central_path.startsWith(`${home}${path.sep}`));
  assert(fs.existsSync(path.join(install.central_path, 'SKILL.md')));
  assert.deepEqual(snapshot(projectA), { '.': { kind: 'directory' } });
  assert.deepEqual(snapshot(projectB), { '.': { kind: 'directory' } });
  state.central = install.central_path;
  state.skill = install.name;
  state.skillId = install.skill_id;
  return { detail: 'local import created central asset without deployment', evidence: { store: sanitize(repo.base_dir), central: sanitize(install.central_path), originalSha256: digest(path.join(install.central_path, 'SKILL.md')), synced: install.synced } };
});

if (!state.central) {
  for (const id of ['SM-02', 'SM-03', 'SM-04', 'SM-05', 'SM-10']) results.push({ id, status: 'NOT TESTED', detail: 'SM-01 prerequisite failed; aborted further actions for safety' });
} else {
  check('SM-02', () => {
    const regA = run('projects', 'add', projectA);
    const regB = run('projects', 'add', projectB);
    assert.equal(regA.ok, true); assert.equal(regB.ok, true);
    assert.equal(fs.readdirSync(projectA).length, 0, 'project registration created files');
    assert.equal(fs.readdirSync(projectB).length, 0, 'project registration created files');
    const beforeB = snapshot(projectB);
    const dry = run('projects', 'deploy', projectA, state.skill, '--agent', 'codex', '--agent', 'claude_code', '--dry-run');
    assert.equal(dry.dry_run, true);
    assert.equal(fs.readdirSync(projectA).length, 0, 'dry run created directories');
    const deployed = run('projects', 'deploy', projectA, state.skill, '--agent', 'codex', '--agent', 'claude_code');
    assert.equal(deployed.ok, true);
    assert.equal(deployed.changed_pairs, 2);
    assert.deepEqual(snapshot(projectB), beforeB, 'project B modified by A deployment');
    state.targets = deployed.targets;
    return { detail: 'A-only dry-run and two-agent deploy left project B untouched', evidence: { projectA: sanitize(projectA), projectB: sanitize(projectB), changedPairs: deployed.changed_pairs, projectADirs: Object.keys(snapshot(projectA)), projectBDirs: Object.keys(beforeB) } };
  });
  if (state.targets) {
    check('SM-03', () => {
      // Official, project-scoped discovery paths verified on 2026-11-18.
      // This is a documentation-conformance check, not an Agent subprocess test.
      const documented = [
        { agent: 'codex', relativeDir: '.agents/skills', doc: 'https://learn.chatgpt.com/docs/build-skills#where-to-save-skills' },
        { agent: 'claude_code', relativeDir: '.claude/skills', doc: 'https://code.claude.com/docs/en/skills#choose-where-skills-load' },
      ];
      const observed = state.targets.map(t => ({
        agent: t.agent,
        relativeDir: path.relative(projectA, path.dirname(t.path)),
        path: sanitize(t.path),
      }));
      const comparisons = documented.map(expected => {
        const match = observed.find(actual => actual.agent === expected.agent);
        return { ...expected, actualRelativeDir: match?.relativeDir ?? null, matches: match?.relativeDir === expected.relativeDir };
      });
      const report = run('projects', 'show', projectA);
      assert(report.skills?.length >= 2, 'CLI failed to discover deployed project skills');
      for (const t of state.targets) assert(fs.lstatSync(t.path).isSymbolicLink(), `missing project skill link: ${t.path}`);
      const bad = comparisons.filter(c => !c.matches);
      return {
        status: bad.length ? 'FAIL' : 'PASS',
        detail: bad.length
          ? `documented project directory mismatch: ${bad.map(c => `${c.agent} actual=${c.actualRelativeDir} expected=${c.relativeDir}`).join('; ')}`
          : 'Codex and Claude project directories conform to their official Skill documentation',
        evidence: { criterion: 'official product documentation plus actual CLI deployment paths; no Agent subprocess required', documentationCheckedAt: '2026-11-18', comparisons, observed, detectedByManager: report.skills.length },
      };
    });
    check('SM-04', () => {
      const links = state.targets.map(t => t.path);
      assert.equal(links.length, 2);
      for (const link of links) {
        assert(fs.lstatSync(link).isSymbolicLink());
        assert.equal(fs.realpathSync(link), fs.realpathSync(state.central));
      }
      return { detail: 'both project symlinks resolve to the installed central Skill', evidence: { links: links.map(p => ({ path: sanitize(p), target: sanitize(fs.realpathSync(p)) })) } };
    });
    check('SM-05', () => {
      const centralSkill = path.join(state.central, 'SKILL.md');
      const before = fs.readFileSync(centralSkill);
      const originalHash = sha256(before);
      const linkSkill = path.join(projectA, '.codex/skills', path.basename(state.central), 'SKILL.md');
      try {
        fs.appendFileSync(linkSkill, '\nIssue-4 linked write proof (transient).\n');
        assert.notEqual(digest(centralSkill), originalHash);
        assert.equal(digest(linkSkill), digest(centralSkill));
        return { status: 'PARTIAL', detail: 'project symlink write changed central asset; no GUI warning confirmation in this CLI-only run', evidence: { originalSha256: originalHash, changedSha256: digest(centralSkill), sharedWithClaude: digest(path.join(projectA, '.claude/skills', path.basename(state.central), 'SKILL.md')) === digest(centralSkill) } };
      } finally {
        fs.writeFileSync(centralSkill, before);
        assert.equal(digest(centralSkill), originalHash);
      }
    });
  } else {
    for (const id of ['SM-03', 'SM-04', 'SM-05']) results.push({ id, status: 'NOT TESTED', detail: 'SM-02 deployment prerequisite failed' });
  }
  check('SM-10', () => {
    const after = Object.fromEntries(['.codex', '.claude', '.agents'].map(name => [name, snapshot(path.join(home, name))]));
    assert.deepEqual(after, agentsBefore, 'user-level Agent fixture was modified');
    assert(!fs.existsSync(path.join(projectB, '.codex')));
    assert(!fs.existsSync(path.join(projectB, '.claude')));
    return { detail: 'project workflow did not modify preexisting user-level Agent skill directories in the fake HOME', evidence: { agentRoots: Object.keys(agentsBefore), unchanged: true, projectBEmpty: true } };
  });
}
for (const id of ['SM-06', 'SM-07', 'SM-08', 'SM-09']) results.push({ id, status: 'NOT TESTED', detail: 'Issue #4 stage B: reserved for Issue #11 Git Commit/Tree and ownership implementation' });
const payload = {
  title: 'Issue #4 stage A isolated CLI validation',
  date: new Date().toISOString(),
  commit,
  worktree: '<worktree>',
  platform: `${os.platform()} ${os.arch()}`,
  node: process.version,
  binary: '<worktree>/src-tauri/target/debug/agent-toolchain-manager-cli',
  fixture: '<fixture> (ignored .npm-cache subdirectory)',
  baselineGitStatus,
  results,
  commands,
  projectASnapshot: snapshot(projectA),
  projectBSnapshot: snapshot(projectB),
  globalAgentSnapshots: Object.fromEntries(['.codex', '.claude', '.agents'].map(name => [name, snapshot(path.join(home, name))])),
};
const evidencePath = path.join(worktree, 'validation/issue-4-stage-a-evidence.json');
fs.writeFileSync(evidencePath, `${JSON.stringify(payload, null, 2)}\n`);
console.log(`Evidence: ${path.relative(worktree, evidencePath)}`);
if (!process.argv.includes('--keep')) fs.rmSync(fixture, { recursive: true, force: true });
else console.log(`Fixture retained: ${fixture}`);
const failures = results.filter(r => r.status === 'FAIL');
if (failures.length) process.exitCode = 1;
