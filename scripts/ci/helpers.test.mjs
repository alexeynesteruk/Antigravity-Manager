import assert from 'node:assert/strict';
import { test } from 'node:test';
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { execFileSync, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { tmpdir } from 'node:os';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { classify } from './changes.mjs';
import { extractNotes, parseTag, publish, validateRelease, windowsDraft } from './release.mjs';

const helper = fileURLToPath(new URL('./release.mjs', import.meta.url));
const bump = fileURLToPath(new URL('../bump-version.mjs', import.meta.url));
const tag = 'v1.2.3-beta.1';
const commit = 'a'.repeat(40);
const targets = ['darwin-aarch64', 'darwin-x86_64', 'darwin-universal', 'linux-x86_64', 'linux-aarch64', 'windows-x86_64'];
const sha = (value) => createHash('sha256').update(value).digest('hex');
const write = (dir, name, value) => {
  const path = join(dir, name);
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, typeof value === 'string' ? value : JSON.stringify(value, null, 2));
};

function fixture(t) {
  const parent = resolve(tmpdir());
  const dir = mkdtempSync(join(parent, 'antigravity-ci-'));
  t.after(() => {
    // Verify the absolute deletion target stays inside this test's temp root.
    const child = relative(parent, resolve(dir));
    assert.ok(child.startsWith('antigravity-ci-') && !child.includes('..'));
    rmSync(dir, { recursive: true, force: true });
  });
  return dir;
}

test('path selection covers deletions, shared inputs, frontend assets and docs-only changes', () => {
  assert.deepEqual(classify(['docs/README.md']), { frontend: false, backend: false });
  assert.deepEqual(classify(['src-tauri/src/proxy/server.rs']), { frontend: false, backend: true });
  assert.deepEqual(classify(['src/pages/Settings.tsx']), { frontend: true, backend: false });
  for (const path of ['package-lock.json', 'vite.config.ts', 'tsconfig.node.json', 'public/icon.png']) assert.equal(classify([path]).frontend, true);
  for (const path of ['scripts/ci/changes.mjs', '.github/workflows/ci.yml', '.cargo/config.toml', 'src-tauri/icons/icon.png']) assert.deepEqual(classify([path]), { frontend: true, backend: true });
  assert.deepEqual(classify(Array.from({ length: 350 }, (_, i) => `docs/${i}.md`).concat('src/App.tsx')), { frontend: true, backend: false });
});

test('unknown push bases and manual dispatch select both validators', (t) => {
  const dir = fixture(t);
  write(dir, 'event.json', { before: '0'.repeat(40) });
  for (const event of ['push', 'workflow_dispatch']) {
    const output = join(dir, `${event}.txt`);
    execFileSync(process.execPath, [fileURLToPath(new URL('./changes.mjs', import.meta.url))], {
      cwd: dir, env: { ...process.env, GITHUB_EVENT_NAME: event, GITHUB_EVENT_PATH: join(dir, 'event.json'), GITHUB_SHA: commit, GITHUB_OUTPUT: output },
    });
    assert.equal(readFileSync(output, 'utf8'), 'frontend=true\nbackend=true\n');
  }
});

test('release gate enforces exact SemVer, checkout, channel, versions and opt-in dependencies', () => {
  const context = { tag, commit, head: commit, ref: 'refs/heads/beta', versions: Array(6).fill('1.2.3-beta.1'), onChannel: true };
  assert.equal(validateRelease(context).branch, 'beta');
  assert.equal(parseTag('v1.2.3').prerelease, false);
  for (const invalid of ['v01.2.3', 'v1.2.3-beta.01', 'v1.2.3garbage', 'v1.2.3-', 'v1.2.3-beta..1', '1.2.3']) assert.throws(() => parseTag(invalid));
  for (const delta of [{ head: 'b'.repeat(40) }, { ref: 'refs/heads/main' }, { onChannel: false }, { versions: ['1.2.3'] }, { publishRelease: true }, { publishDocker: true }]) assert.throws(() => validateRelease({ ...context, ...delta }));
  assert.equal(validateRelease({ ...context, buildNative: true, publishRelease: true }).prerelease, true);
  assert.equal(validateRelease({ ...context, buildDocker: true, publishDocker: true }).prerelease, true);
});

test('changelog extraction matches an entire tag and stops before the next version', () => {
  const text = '*   **Version History**:\r\n    *   **v1.2.3-beta.10 (date)**:\r\n        - Later\r\n    *   **v1.2.3-beta.1 (date)**:\r\n        - Correct\r\n    *   **v1.2.2 (date)**:\r\n        - Older\r\n';
  assert.equal(extractNotes(text, tag), '- Correct');
  assert.equal(extractNotes(text, 'v1.2.3-beta'), '');
});

test('cached bundle cleanup is restricted to a known target and preserves other cached files', (t) => {
  const dir = fixture(t);
  const selected = 'src-tauri/target/x86_64-pc-windows-msvc/release/bundle/old.exe';
  const other = 'src-tauri/target/aarch64-apple-darwin/release/bundle/other.dmg';
  const dependency = 'src-tauri/target/x86_64-pc-windows-msvc/release/deps/keep.rlib';
  for (const path of [selected, other, dependency]) write(dir, path, 'cached');
  const clean = (target) => spawnSync(process.execPath, [helper, 'clean-bundle'], { cwd: dir, encoding: 'utf8', env: { ...process.env, RUST_TARGET: target } });
  assert.notEqual(clean('../../../outside').status, 0);
  assert.equal(existsSync(join(dir, selected)), true);
  assert.equal(clean('x86_64-pc-windows-msvc').status, 0);
  assert.equal(existsSync(join(dir, selected)), false);
  assert.equal(existsSync(join(dir, other)), true);
  assert.equal(existsSync(join(dir, dependency)), true);
});

function releaseFixture(t) {
  const dir = fixture(t);
  write(dir, 'CHANGELOG.md', `*   **Version History**:\n    *   **${tag} (date)**:\n        - English notes\n`);
  write(dir, 'CHANGELOG_ZH.md', `*   **版本演进**:\n    *   **${tag} (date)**:\n        - 中文更新\n`);
  for (const target of targets) {
    const primary = `${target}${target.startsWith('darwin-') ? '.app.tar.gz' : target.startsWith('windows-') ? '-setup.exe' : '.AppImage'}`;
    const files = { [primary]: `installer:${target}`, [`${primary}.sig`]: `signature:${target}` };
    for (const [name, value] of Object.entries(files)) write(dir, `all-artifacts/native-${target}/${name}`, value);
    write(dir, `all-artifacts/native-${target}/${target}.json`, { target, tag, commit, primary, assets: Object.fromEntries(Object.entries(files).map(([name, value]) => [name, sha(value)])) });
  }
  return dir;
}

const prepare = (dir) => spawnSync(process.execPath, [helper, 'prepare'], { cwd: dir, encoding: 'utf8', env: { ...process.env, RELEASE_TAG: tag, RELEASE_COMMIT: commit, GITHUB_REPOSITORY: 'owner/repo' } });

test('complete signed artifacts produce five accurate updater URLs without a universal collision', (t) => {
  const dir = releaseFixture(t);
  const result = prepare(dir);
  assert.equal(result.status, 0, result.stderr);
  const manifest = JSON.parse(readFileSync(join(dir, 'updater.json'), 'utf8'));
  assert.equal(manifest.version, tag.slice(1));
  assert.equal(Object.keys(manifest.platforms).length, 5);
  assert.equal(manifest.platforms['darwin-universal'], undefined);
  assert.ok(manifest.notes.includes('English notes') && manifest.notes.includes('中文更新'));
  for (const [target, platform] of Object.entries(manifest.platforms)) {
    const name = decodeURIComponent(platform.url.split('/').at(-1));
    assert.ok(existsSync(join(dir, 'release-files', name)));
    assert.equal(platform.signature, `signature:${target}`);
    assert.ok(platform.url.includes(`/releases/download/${tag}/`));
  }
});

test('publication preparation rejects missing targets, tampering, wrong provenance and absent signatures', (t) => {
  const cases = [
    (dir) => write(dir, 'all-artifacts/native-linux-aarch64/linux-aarch64.json', { target: 'unexpected' }),
    (dir) => write(dir, 'all-artifacts/native-linux-x86_64/linux-x86_64.AppImage', 'tampered'),
    (dir) => {
      const path = 'all-artifacts/native-windows-x86_64/windows-x86_64.json';
      write(dir, path, { ...JSON.parse(readFileSync(join(dir, path), 'utf8')), commit: 'b'.repeat(40) });
    },
    (dir) => {
      const path = 'all-artifacts/native-darwin-aarch64/darwin-aarch64.json';
      const metadata = JSON.parse(readFileSync(join(dir, path), 'utf8'));
      delete metadata.assets[`${metadata.primary}.sig`];
      write(dir, path, metadata);
    },
  ];
  for (const alter of cases) {
    const dir = releaseFixture(t);
    alter(dir);
    const result = prepare(dir);
    assert.notEqual(result.status, 0);
    assert.equal(existsSync(join(dir, 'updater.json')), false);
  }
});

test('bump generates both changelogs for current and older upstream layouts in temporary fixtures', (t) => {
  for (const current of [true, false]) {
    const dir = fixture(t);
    mkdirSync(join(dir, 'scripts'));
    copyFileSync(bump, join(dir, 'scripts/bump-version.mjs'));
    write(dir, 'package.json', { name: 'fixture', version: '1.2.2' });
    const english = current ? 'CHANGELOG.md' : 'CHANGELOG_EN.md';
    const chinese = current ? 'CHANGELOG_ZH.md' : 'CHANGELOG.md';
    write(dir, english, '*   **Version History**:\n');
    write(dir, chinese, '*   **版本演进**:\n');
    write(dir, 'README.md', 'Fixture (v1.2.2)');
    const result = spawnSync(process.execPath, ['scripts/bump-version.mjs', '1.2.3-beta.1'], { cwd: dir, encoding: 'utf8', env: { ...process.env, npm_config_commit: 'false', npm_config_dry_run: 'false' } });
    assert.equal(result.status, 0, result.stderr);
    assert.ok(extractNotes(readFileSync(join(dir, english), 'utf8'), tag));
    assert.ok(extractNotes(readFileSync(join(dir, chinese), 'utf8'), tag));
    assert.equal(readFileSync(join(dir, 'README.md'), 'utf8'), 'Fixture (v1.2.2)');
    assert.equal(JSON.parse(readFileSync(join(dir, 'package.json'), 'utf8')).version, '1.2.3-beta.1');
  }
});

function publicationFixture(t, releaseTag, action) {
  const dir = fixture(t);
  write(dir, 'release-files/installer.exe', 'prepared installer');
  write(dir, 'updater.json', '{}');
  write(dir, 'release_notes.md', 'Verified current notes');
  write(dir, 'windows-artifacts/installer.exe', 'Windows installer');
  write(dir, 'CHANGELOG.md', `*   **Version History**:\n    *   **${releaseTag} (date)**:\n        - Current notes\n`);
  const previous = { cwd: process.cwd(), tag: process.env.RELEASE_TAG, commit: process.env.RELEASE_COMMIT };
  try {
    process.chdir(dir);
    process.env.RELEASE_TAG = releaseTag;
    process.env.RELEASE_COMMIT = commit;
    action();
  } finally {
    process.chdir(previous.cwd);
    if (previous.tag === undefined) delete process.env.RELEASE_TAG; else process.env.RELEASE_TAG = previous.tag;
    if (previous.commit === undefined) delete process.env.RELEASE_COMMIT; else process.env.RELEASE_COMMIT = previous.commit;
  }
}

function mockGh({ published = false, failUpload = false, existingPreview = false } = {}) {
  const calls = [];
  const run = (...args) => {
    calls.push(args);
    if (args[0] === 'view') {
      if (published) return JSON.stringify({ isDraft: false });
      if (args[1] === 'preview' && existingPreview) return JSON.stringify({ tagName: 'preview' });
      throw new Error('Release not found');
    }
    if (failUpload && args[0] === 'upload') throw new Error('Upload failed');
    return '';
  };
  return { calls, run };
}

test('public release and Windows draft paths refuse already published releases before mutations', (t) => {
  publicationFixture(t, tag, () => {
    for (const operation of [publish, windowsDraft]) {
      const mock = mockGh({ published: true });
      assert.throws(() => operation(mock.run), /already published/);
      assert.deepEqual(mock.calls.map((call) => call[0]), ['view']);
    }
  });
});

test('a failed installer upload leaves the created release in draft and never advances channels', (t) => {
  publicationFixture(t, tag, () => {
    const mock = mockGh({ failUpload: true });
    assert.throws(() => publish(mock.run), /Upload failed/);
    assert.deepEqual(mock.calls.map((call) => call[0]), ['view', 'create', 'upload']);
    assert.ok(mock.calls[1].includes('--draft') && mock.calls[1].includes('--latest=false'));
    assert.equal(mock.calls.some((call) => call.includes('--draft=false') || call[1] === 'preview'), false);
  });
});

test('beta publishes only after upload, never becomes latest, then advances preview metadata alone', (t) => {
  publicationFixture(t, tag, () => {
    const mock = mockGh({ existingPreview: true });
    publish(mock.run);
    const publicIndex = mock.calls.findIndex((call) => call.includes('--draft=false'));
    const uploadIndex = mock.calls.findIndex((call) => call[0] === 'upload' && call[1] === tag);
    const previewIndex = mock.calls.findIndex((call) => call[1] === 'preview');
    assert.ok(uploadIndex < publicIndex && publicIndex < previewIndex);
    assert.ok(mock.calls[publicIndex].includes('--prerelease=true') && mock.calls[publicIndex].includes('--latest=false'));
    assert.equal(mock.calls.some((call) => call.includes('--latest=true')), false);
    assert.deepEqual(mock.calls.at(-1), ['upload', 'preview', 'updater.json', '--clobber']);
  });
});

test('stable advances latest only in the final publication call and never changes preview', (t) => {
  publicationFixture(t, 'v1.2.3', () => {
    const mock = mockGh();
    publish(mock.run);
    assert.deepEqual(mock.calls.at(-1), ['edit', 'v1.2.3', '--draft=false', '--prerelease=false', '--latest=true']);
    assert.equal(mock.calls.slice(0, -1).some((call) => call.includes('--latest=true')), false);
    assert.equal(mock.calls.some((call) => call[1] === 'preview'), false);
  });
});

test('Windows upload creates an unpublished draft with current notes and no updater changes', (t) => {
  publicationFixture(t, tag, () => {
    const mock = mockGh();
    windowsDraft(mock.run);
    assert.deepEqual(mock.calls.map((call) => call[0]), ['view', 'create', 'upload']);
    assert.ok(mock.calls[1].includes('--draft') && mock.calls[1].includes('--latest=false'));
    assert.equal(mock.calls.some((call) => call.includes('--draft=false') || call.includes('updater.json') || call[1] === 'preview'), false);
    assert.equal(readFileSync('release_notes.md', 'utf8'), '- Current notes');
  });
});
