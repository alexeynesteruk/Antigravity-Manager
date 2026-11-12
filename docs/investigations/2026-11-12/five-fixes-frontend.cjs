// Execute the actual settings controls and save/reload closures with local mocks.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const root = path.resolve(__dirname, '../../..');
const ts = require(path.join(root, 'node_modules/typescript'));
const source = name => fs.readFileSync(path.join(root, name), 'utf8');
function execute(text, modules = {}, globals = {}) {
  const exports = {};
  const code = ts.transpileModule(text, { compilerOptions: {
    module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2020, jsx: ts.JsxEmit.ReactJSX,
  } }).outputText;
  vm.runInNewContext(code, { exports, require(name) {
    assert.ok(Object.hasOwn(modules, name), name); return modules[name];
  }, ...globals });
  return exports;
}
const json = value => JSON.parse(JSON.stringify(value));
const deferred = () => { let resolve, reject; const promise = new Promise((a, b) => { resolve = a; reject = b; }); return { promise, resolve, reject }; };
async function flush() { for (let i = 0; i < 30; i++) await Promise.resolve(); }
const tests = [];
const test = (name, body) => tests.push([name, body]);
let current, reads, readConfig;
const store = { getState: () => ({ config: current }), setState: update => { current = update.config; } };
const updates = execute(source('src/utils/configUpdates.ts'), {
  '../services/configService': { loadConfig: () => { reads++; return readConfig(); } },
  '../stores/useConfigStore': { useConfigStore: store },
});
test('failed settings save restores authoritative configuration', async () => {
  const pending = {}; const saved = { theme: 'saved' }; current = pending; reads = 0;
  readConfig = async () => saved;
  await updates.reloadSavedConfigIfUnchanged(pending);
  assert.equal(current, saved); assert.equal(reads, 1);
});
test('a newer edit suppresses recovery IO before it starts', async () => {
  current = {}; reads = 0;
  await updates.reloadSavedConfigIfUnchanged({}); assert.equal(reads, 0);
});
test('a delayed recovery cannot overwrite a newer edit', async () => {
  const pending = {}; const newer = { theme: 'newer' }; const gate = deferred(); current = pending;
  readConfig = () => gate.promise;
  const run = updates.reloadSavedConfigIfUnchanged(pending); current = newer; gate.resolve({ theme: 'old' });
  await run; assert.equal(current, newer);
});
test('unavailable recovery leaves the save error and current snapshot intact', async () => {
  const pending = {}; current = pending; readConfig = async () => { throw new Error('offline'); };
  await updates.reloadSavedConfigIfUnchanged(pending); assert.equal(current, pending);
});

const ast = ts.createSourceFile('ApiProxy.tsx', source('src/pages/ApiProxy.tsx'), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
let updateSource;
(function visit(node) {
  if (ts.isVariableDeclaration(node) && node.name.getText(ast) === 'updateProxyConfig') updateSource = node.initializer.getText(ast);
  ts.forEachChild(node, visit);
})(ast);
assert.ok(updateSource);
let writes, saveFails;
const save = async config => { current = config; writes.push(config); if (saveFails) throw new Error('rejected'); };
const onChange = execute(`export const update = ${updateSource};`, {}, {
  useConfigStore: store, appConfig: { theme: 'stale', proxy: {} }, saveConfig: save,
}).update;
const jsx = (type, props) => ({ type, props });
function component(name, proxy) {
  const module = execute(source(name), {
    react: { useState: value => [value, () => {}] },
    'react/jsx-runtime': { jsx, jsxs: jsx },
    'react-i18next': { useTranslation: () => ({ t: (key, options) => options?.defaultValue ?? key }) },
    '../../stores/useConfigStore': { useConfigStore: store },
    '../../utils/configUpdates': updates,
  });
  return module.default({ proxy, onChange });
}
function elements(node, type, result = []) {
  if (!node || typeof node !== 'object') return result;
  if (node.type === type) result.push(node);
  const children = node.props?.children;
  for (const child of Array.isArray(children) ? children.flat(Infinity) : [children]) elements(child, type, result);
  return result;
}
function reset(proxy) { current = { theme: 'newest', proxy }; writes = []; saveFails = false; reads = 0; }
const imageControl = proxy => elements(component('src/components/settings/ImageRequestLimits.tsx', proxy), 'input');
const blur = (input, value) => input.props.onBlur({ currentTarget: { value: String(value) } });
test('image defaults preserve legacy per-account settings and newer unrelated edits', async () => {
  reset({ image_scheduler: { per_account_concurrency: 1 }, port: 8443 });
  const controls = imageControl(current.proxy);
  assert.equal(controls[0].props.defaultValue, 10); assert.equal(controls[1].props.defaultValue, 1);
  blur(controls[0], 4); await flush();
  assert.equal(current.theme, 'newest'); assert.equal(current.proxy.port, 8443);
  assert.deepEqual(json(current.proxy.image_scheduler), { per_account_concurrency: 1, max_outputs_per_request: 4, max_parallel_outputs: 1 });
});
test('invalid image limits never publish an invalid optimistic snapshot', async () => {
  reset({ image_scheduler: { per_account_concurrency: 1, max_outputs_per_request: 10, max_parallel_outputs: 1 } });
  const controls = imageControl(current.proxy);
  for (const value of ['', '0', '-1', '1.5', '101']) blur(controls[0], value);
  blur(controls[1], 17); await flush(); assert.equal(writes.length, 0);
});
test('lowering the image cap also bounds parallel workers', async () => {
  reset({ image_scheduler: { per_account_concurrency: 1, max_outputs_per_request: 10, max_parallel_outputs: 8 } });
  blur(imageControl(current.proxy)[0], 3); await flush(); assert.equal(current.proxy.image_scheduler.max_parallel_outputs, 3);
});
test('stale image controls validate against the latest saved count', async () => {
  reset({ image_scheduler: { per_account_concurrency: 1, max_outputs_per_request: 10, max_parallel_outputs: 1 } });
  const controls = imageControl(current.proxy);
  current = { ...current, proxy: { image_scheduler: { per_account_concurrency: 1, max_outputs_per_request: 2, max_parallel_outputs: 1 } } };
  blur(controls[1], 3); await flush(); assert.equal(writes.length, 0);
});
test('trusted-host edits preserve existing IP filtering and deduplicate entries', async () => {
  const security = { blacklist: { enabled: false, block_message: 'custom' }, whitelist: { enabled: true, whitelist_priority: true }, trusted_proxies: [] };
  reset({ security_monitor: security });
  const textarea = elements(component('src/components/settings/TrustedHostsSettings.tsx', current.proxy), 'textarea')[0];
  blur(textarea, 'gateway.home:8443, gateway.home:8443\n[::1]:8443'); await flush();
  assert.deepEqual(json(current.proxy.security_monitor), { ...security, trusted_hosts: ['gateway.home:8443', '[::1]:8443'] });
});
test('a server-rejected host edit reloads persisted settings', async () => {
  const saved = { theme: 'saved', proxy: { security_monitor: { trusted_hosts: [] } } };
  reset(saved.proxy); saveFails = true; readConfig = async () => saved;
  const textarea = elements(component('src/components/settings/TrustedHostsSettings.tsx', current.proxy), 'textarea')[0];
  blur(textarea, 'https://invalid.example/'); await flush(); assert.equal(current, saved); assert.equal(reads, 1);
});
(async () => {
  for (const [name, body] of tests) { await body(); console.log('PASS ' + name); }
  console.log(`${tests.length} settings source-behavior checks passed`);
})().catch(error => { console.error(error); process.exitCode = 1; });
