import { appendFileSync, readFileSync } from 'node:fs';
import { execFileSync } from 'node:child_process';

// No API pagination or GitHub's 300-file path-filter limit: inspect the checked-out
// merge result on PRs and the entire before..after range on pushes. Unknown bases
// deliberately run both validators rather than silently skipping verification.
export function classify(paths) {
  const shared = paths.some((p) => /^(?:\.github\/workflows\/ci\.yml|scripts\/ci\/|\.cargo\/|rust-toolchain(?:\.toml)?$)/.test(p));
  return {
    frontend: shared || paths.some((p) => /^(?:src\/|public\/|package(?:-lock)?\.json$|index\.html$|(?:tsconfig[^/]*|vite\.config[^/]*|tailwind\.config[^/]*|postcss\.config[^/]*)$|src-tauri\/icons\/)/.test(p)),
    backend: shared || paths.some((p) => p.startsWith('src-tauri/')),
  };
}

function git(...args) {
  return execFileSync('git', args, { encoding: 'utf8', maxBuffer: 32 * 1024 * 1024 });
}

function resolveChanges() {
  if (process.env.GITHUB_EVENT_NAME === 'workflow_dispatch') return { frontend: true, backend: true };
  let event;
  try { event = JSON.parse(readFileSync(process.env.GITHUB_EVENT_PATH, 'utf8')); }
  catch (error) {
    console.warn(`Cannot read changed-path event; running all validators: ${error.message}`);
    return { frontend: true, backend: true };
  }
  const base = event.pull_request?.base.sha ?? event.before;
  const head = process.env.GITHUB_SHA;
  if (!/^[0-9a-f]{40}$/.test(base ?? '') || /^0+$/.test(base) || !/^[0-9a-f]{40}$/.test(head ?? '')) {
    return { frontend: true, backend: true };
  }
  try {
    try { git('cat-file', '-e', `${base}^{commit}`); }
    catch { git('fetch', '--no-tags', '--depth=1', 'origin', base); }
    return classify(git('diff', '--name-only', '--diff-filter=ACDMRTUXB', '-z', base, head).split('\0').filter(Boolean));
  } catch (error) {
    console.warn(`Cannot establish changed paths; running all validators: ${error.message}`);
    return { frontend: true, backend: true };
  }
}

if (process.env.GITHUB_OUTPUT && process.argv[1]?.endsWith('changes.mjs')) {
  const result = resolveChanges();
  appendFileSync(process.env.GITHUB_OUTPUT, Object.entries(result).map(([key, value]) => `${key}=${value}\n`).join(''));
  console.log(`Validation scope: frontend=${result.frontend}, backend=${result.backend}`);
}
