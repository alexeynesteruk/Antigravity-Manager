import { appendFileSync, copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const targets = ['darwin-aarch64', 'darwin-x86_64', 'darwin-universal', 'linux-x86_64', 'linux-aarch64', 'windows-x86_64'];
const git = (...args) => execFileSync('git', args, { encoding: 'utf8' }).trim();
const gh = (...args) => execFileSync('gh', ['release', ...args], { encoding: 'utf8' }).trim();
const json = (path) => JSON.parse(readFileSync(path, 'utf8'));
const digest = (path) => createHash('sha256').update(readFileSync(path)).digest('hex');
const enabled = (name) => process.env[name] === 'true';

export function parseTag(tag) {
  const match = /^v((?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?)$/.exec(tag);
  if (!match || match[2]?.split('.').some((p) => /^0\d+$/.test(p))) throw new Error(`Invalid release tag: ${tag}`);
  return { version: match[1], prerelease: Boolean(match[2]), branch: match[2] ? 'beta' : 'main' };
}

export function extractNotes(text, tag) {
  const lines = text.split(/\r?\n/);
  const heading = (line) => /^(?:\s*[*-]\s+\*\*|#{1,6}\s+)\s*(v\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)(?=[\s(*:]|$)/.exec(line)?.[1];
  const start = lines.findIndex((line) => heading(line) === tag);
  if (start < 0) return '';
  const next = lines.findIndex((line, i) => i > start && heading(line));
  return lines.slice(start + 1, next < 0 ? undefined : next).map((line) => line.replace(/^ {8}/, '')).join('\n').trim();
}

function releaseNotes(tag) {
  // This fork uses English CHANGELOG.md and Chinese CHANGELOG_ZH.md; older
  // upstream layouts used CHANGELOG_EN.md and Chinese CHANGELOG.md.
  const files = existsSync('CHANGELOG_EN.md') ? ['CHANGELOG_EN.md', 'CHANGELOG.md'] : ['CHANGELOG.md', 'CHANGELOG_ZH.md'];
  const sections = files.filter((file) => existsSync(file)).map((file) => {
    const notes = extractNotes(readFileSync(file, 'utf8'), tag);
    if (!notes) throw new Error(`${file} has no nonempty exact section for ${tag}`);
    return notes;
  });
  if (!sections.length) throw new Error('No release changelog found');
  return sections.join('\n\n---\n\n');
}

export function validateRelease({ tag, commit, head, ref, versions, onChannel, buildNative, publishRelease, buildDocker, publishDocker }) {
  if (publishRelease && !buildNative) throw new Error('Publishing requires all native packages');
  if (publishDocker && !buildDocker) throw new Error('Docker publication requires Docker builds');
  const release = parseTag(tag);
  if (commit !== head) throw new Error('Checkout must be the exact existing release tag');
  if (ref?.startsWith('refs/heads/') && ref !== `refs/heads/${release.branch}`) {
    throw new Error(`${tag} must be dispatched from ${release.branch}, or its existing tag`);
  }
  if (!onChannel) throw new Error(`${tag} must belong to ${release.branch}`);
  if (versions.length !== 6 || versions.some((version) => version !== release.version)) throw new Error('Release tag and all manifest/lockfile versions must match');
  return release;
}

function gate() {
  const tag = process.env.RELEASE_TAG || (process.env.GITHUB_REF?.startsWith('refs/tags/') ? process.env.GITHUB_REF.slice(10) : `v${json('package.json').version}`);
  const release = parseTag(tag);
  const commit = git('rev-parse', `refs/tags/${tag}^{commit}`);
  let onChannel = false;
  try { git('merge-base', '--is-ancestor', commit, `refs/remotes/origin/${release.branch}`); onChannel = true; } catch { /* Report the channel failure below. */ }
  const lock = json('package-lock.json');
  const cargo = readFileSync('src-tauri/Cargo.toml', 'utf8').match(/\[package\][\s\S]*?^version\s*=\s*"([^"]+)"/m)?.[1];
  const cargoLock = readFileSync('src-tauri/Cargo.lock', 'utf8').match(/\[\[package\]\]\s*name = "antigravity-tools"\s*version = "([^"]+)"/)?.[1];
  const versions = [json('package.json').version, lock.version, lock.packages?.['']?.version, cargo, cargoLock, json('src-tauri/tauri.conf.json').version];
  validateRelease({ tag, commit, head: git('rev-parse', 'HEAD'), ref: process.env.GITHUB_REF, versions, onChannel,
    buildNative: enabled('BUILD_NATIVE'), publishRelease: enabled('PUBLISH_RELEASE'), buildDocker: enabled('BUILD_DOCKER'), publishDocker: enabled('PUBLISH_DOCKER') });
  releaseNotes(tag); // Fail before building instead of publishing placeholder notes.
  appendFileSync(process.env.GITHUB_OUTPUT, `release_tag=${tag}\ncommit=${commit}\nis_prerelease=${release.prerelease}\n`);
  appendFileSync(process.env.GITHUB_STEP_SUMMARY, `### Release gate: ${tag}\nVerified existing tag, ${release.branch} ancestry, manifest versions and changelogs.\n\nNative build: ${enabled('BUILD_NATIVE')}; publish: ${enabled('PUBLISH_RELEASE')}. Docker build: ${enabled('BUILD_DOCKER')}; push: ${enabled('PUBLISH_DOCKER')}.\n`);
}

function filesBelow(dir) {
  if (!existsSync(dir)) return [];
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => entry.isDirectory() ? filesBelow(join(dir, entry.name)) : entry.isFile() ? [join(dir, entry.name)] : []);
}

function primaryAsset(target, names) {
  const suffix = target.startsWith('darwin-') ? '.app.tar.gz' : target.startsWith('windows-') ? '-setup.exe' : '.AppImage';
  const matches = names.filter((name) => name.endsWith(suffix));
  if (matches.length !== 1) throw new Error(`Expected exactly one updater asset for ${target}, found ${matches.length}`);
  return matches[0];
}

function cleanBundle() {
  const rustTarget = process.env.RUST_TARGET;
  if (!['aarch64-apple-darwin', 'x86_64-apple-darwin', 'universal-apple-darwin', 'x86_64-unknown-linux-gnu', 'aarch64-unknown-linux-gnu', 'x86_64-pc-windows-msvc'].includes(rustTarget)) throw new Error('Unknown packaging target');
  const root = resolve('src-tauri/target');
  const bundle = resolve(root, rustTarget, 'release/bundle');
  if (!bundle.startsWith(`${root}/`) && !bundle.startsWith(`${root}\\`)) throw new Error('Bundle directory escapes target root');
  // Rust caches can retain previous installer versions. Only remove this exact
  // target's generated bundle directory; keep compiled dependencies cached.
  rmSync(bundle, { recursive: true, force: true });
}

function stage() {
  const target = process.env.ARTIFACT_TARGET;
  if (!targets.includes(target)) throw new Error(`Unknown target ${target}`);
  const roots = [join('src-tauri/target', process.env.RUST_TARGET || '', 'release/bundle')];
  let files = roots.flatMap(filesBelow).filter((path) => /(?:\.dmg|\.deb|\.rpm|\.AppImage|\.msi|\.exe|\.sig|\.app\.tar\.gz)$/.test(path));
  if (target.startsWith('darwin-') && !files.some((path) => path.endsWith('.app.tar.gz'))) {
    const macDir = join(roots[0], 'macos');
    const apps = readdirSync(macDir, { withFileTypes: true }).filter((entry) => entry.isDirectory() && entry.name.endsWith('.app'));
    if (apps.length !== 1) throw new Error('Expected one macOS app bundle');
    const archive = join(macDir, `${apps[0].name}.tar.gz`);
    execFileSync('tar', ['-czf', archive, '-C', macDir, apps[0].name]);
    files.push(archive); // Unsigned, artifact-only builds still retain a downloadable app.
  }
  mkdirSync('release-assets', { recursive: true });
  const assets = {};
  for (const path of files) {
    let name = basename(path).replaceAll(' ', '.');
    if (target.startsWith('darwin-') && /\.app\.tar\.gz(?:\.sig)?$/.test(name)) {
      const arch = target === 'darwin-x86_64' ? 'x64' : target.slice(7);
      name = name.replace(/\.app\.tar\.gz/, `_${arch}.app.tar.gz`);
    }
    if (assets[name]) throw new Error(`Duplicate asset ${name}`);
    copyFileSync(path, join('release-assets', name));
    assets[name] = digest(path); // Never modify signed installer/archive bytes.
  }
  const primary = primaryAsset(target, Object.keys(assets));
  writeFileSync(`release-assets/${target}.json`, JSON.stringify({ target, commit: process.env.RELEASE_COMMIT, tag: process.env.RELEASE_TAG, primary, assets }, null, 2));
}

function prepare() {
  const tag = process.env.RELEASE_TAG;
  const release = parseTag(tag);
  const metadata = filesBelow('all-artifacts').filter((path) => targets.some((target) => basename(path) === `${target}.json`)).map((path) => ({ ...json(path), path }));
  if (metadata.length !== targets.length || targets.some((target) => metadata.filter((item) => item.target === target).length !== 1)) throw new Error('All six native targets are required before publication');
  mkdirSync('release-files', { recursive: true });
  const seen = new Set();
  const platforms = {};
  for (const item of metadata) {
    if (item.tag !== tag || item.commit !== process.env.RELEASE_COMMIT) throw new Error('Artifact provenance mismatch');
    for (const [name, sha] of Object.entries(item.assets)) {
      if (basename(name) !== name || name.includes('\\') || seen.has(name.toLowerCase())) throw new Error(`Unsafe or duplicate asset ${name}`);
      seen.add(name.toLowerCase());
      const source = join(dirname(item.path), name);
      if (digest(source) !== sha) throw new Error(`Artifact checksum mismatch: ${name}`);
      copyFileSync(source, join('release-files', name));
    }
    if (primaryAsset(item.target, Object.keys(item.assets)) !== item.primary || !item.assets[`${item.primary}.sig`]) throw new Error(`Missing updater signature: ${item.target}`);
    const signature = readFileSync(join('release-files', `${item.primary}.sig`), 'utf8').trim();
    if (!signature) throw new Error(`Empty updater signature: ${item.target}`);
    if (item.target !== 'darwin-universal') platforms[item.target] = { signature, url: `https://github.com/${process.env.GITHUB_REPOSITORY}/releases/download/${tag}/${encodeURIComponent(item.primary)}` };
  }
  const notes = releaseNotes(tag);
  writeFileSync('release_notes.md', notes);
  writeFileSync('updater.json', JSON.stringify({ version: release.version, notes, pub_date: new Date().toISOString(), platforms }, null, 2));
}

export function publish(runGh = gh) {
  const tag = process.env.RELEASE_TAG;
  const release = parseTag(tag);
  const assets = filesBelow('release-files');
  if (!assets.length || !existsSync('updater.json')) throw new Error('Validated release assets and updater manifest are required');
  let existing;
  try { existing = JSON.parse(runGh('view', tag, '--json', 'isDraft')); } catch { /* An absent release will be created as a draft. */ }
  if (existing && !existing.isDraft) throw new Error('Refusing to overwrite an already published release');
  const options = ['--title', `Antigravity Tools ${tag}`, '--notes-file', 'release_notes.md', `--prerelease=${release.prerelease}`, '--latest=false'];
  if (existing) runGh('edit', tag, ...options);
  else runGh('create', tag, '--verify-tag', '--draft', ...options);
  runGh('upload', tag, ...assets, 'updater.json', '--clobber');
  runGh('edit', tag, '--draft=false', `--prerelease=${release.prerelease}`, `--latest=${!release.prerelease}`);
  // The application's preview endpoint consumes updater.json only. Keep it
  // isolated from the stable latest endpoint; archive URLs point at the real tag.
  if (release.prerelease) {
    let previewExists = false;
    try { runGh('view', 'preview', '--json', 'tagName'); previewExists = true; } catch { /* Create the preview channel below. */ }
    if (previewExists) {
      runGh('edit', 'preview', '--prerelease', '--latest=false');
      runGh('upload', 'preview', 'updater.json', '--clobber');
    } else {
      runGh('create', 'preview', 'updater.json', '--target', process.env.RELEASE_COMMIT, '--prerelease', '--latest=false', '--title', 'Preview update channel', '--notes', 'Preview updater metadata; installers are attached to their versioned prereleases.');
    }
  }
}

export function windowsDraft(runGh = gh) {
  const tag = process.env.RELEASE_TAG;
  const release = parseTag(tag);
  const notes = releaseNotes(tag);
  writeFileSync('release_notes.md', notes);
  let existing;
  try { existing = JSON.parse(runGh('view', tag, '--json', 'isDraft')); } catch { /* Draft created below. */ }
  if (existing && !existing.isDraft) throw new Error('Refusing to modify an already published release');
  const assets = filesBelow('windows-artifacts').filter((path) => /\.(exe|sig)$/.test(path));
  if (!assets.some((path) => path.endsWith('.exe'))) throw new Error('Missing Windows installer');
  const options = ['--title', `Antigravity Tools ${tag} (Windows draft)`, '--notes-file', 'release_notes.md', `--prerelease=${release.prerelease}`, '--latest=false'];
  if (existing) runGh('edit', tag, ...options);
  else runGh('create', tag, '--verify-tag', '--draft', ...options);
  runGh('upload', tag, ...assets, '--clobber');
  // Never publish a partial platform set or alter either automatic update channel.
}

if (resolve(process.argv[1] || '') === fileURLToPath(import.meta.url)) {
  const commands = { gate, stage, prepare, publish, 'windows-draft': windowsDraft, 'clean-bundle': cleanBundle };
  const command = commands[process.argv[2]];
  if (!command) throw new Error('Expected gate, clean-bundle, stage, prepare, publish, or windows-draft');
  command();
}
