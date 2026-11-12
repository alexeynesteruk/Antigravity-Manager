// Exercises actual TypeScript source with deterministic local service/hook mocks.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const root = path.resolve(__dirname, '../../..');
const ts = require(path.join(root, 'node_modules/typescript'));
const quiet = { log() {}, debug() {}, info() {}, warn() {}, error() {} };
function execute(source, modules = {}, globals = {}) {
  const exports = {};
  const code = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2020 } }).outputText;
  vm.runInNewContext(code, { exports, require(name) { assert.ok(Object.hasOwn(modules, name), name); return modules[name]; }, console: quiet, ...globals });
  return exports;
}
const source = name => fs.readFileSync(path.join(root, name), 'utf8');
const load = (name, modules, globals) => execute(source(name), modules, globals);
async function flush() { for (let i = 0; i < 30; i++) await Promise.resolve(); }
function createStore(init) {
  let state;
  const set = changes => { state = { ...state, ...(typeof changes === 'function' ? changes(state) : changes) }; };
  const get = () => state;
  state = init(set, get);
  return { getState: get, setState: set };
}
async function main() {
  let count = 0;
  const calls = [];
  const service = load('src/services/accountService.ts', {
    '../i18n': { default: { t: key => key }, __esModule: true },
    '../utils/request': { request: async (command, args) => { calls.push([command, args]); return {}; } },
  });
  await service.fetchAccountQuota('a'); await service.fetchAccountQuota('a', true);
  await service.refreshAllQuotas(); await service.refreshAllQuotas(true);
  assert.deepEqual(calls.map(([command]) => command), ['fetch_account_quota', 'fetch_account_quota_automatically', 'refresh_all_quotas', 'refresh_all_quotas_automatically']); count++;

  const flags = [];
  const accounts = [{ id: 'a', email: 'test@example.invalid' }];
  const mocks = {
    listAccounts: async () => accounts, getCurrentAccount: async () => accounts[0],
    getActiveClientsStatus: async () => ({ clients: [] }), switchAccount: async () => {},
    fetchAccountQuota: async (id, automatic) => { flags.push(['single', id, automatic]); return {}; },
    refreshAllQuotas: async automatic => { flags.push(['all', automatic]); return { total: 1, success: 1, failed: 0, details: [] }; },
  };
  const { useAccountStore: store } = load('src/stores/useAccountStore.ts', { zustand: { create: createStore }, '../services/accountService': mocks });
  await store.getState().refreshQuota('a', true); await store.getState().refreshQuota('a');
  await store.getState().refreshAllQuotas(true); await store.getState().refreshAllQuotas();
  assert.deepEqual(flags.slice(0, 4), [['single', 'a', true], ['single', 'a', false], ['all', true], ['all', false]]); count++;
  await store.getState().switchAccount('a'); await flush();
  assert.deepEqual(flags.at(-1), ['single', 'a', true]); count++;

  const effects = [], intervals = [], runnerFlags = [];
  const runner = load('src/components/common/BackgroundTaskRunner.tsx', {
    react: { useEffect: callback => effects.push(callback), useRef: initial => ({ current: initial }) },
    '../../stores/useConfigStore': { useConfigStore: () => ({ config: { auto_refresh: true, refresh_interval: 1 } }) },
    '../../stores/useAccountStore': { useAccountStore: () => ({ refreshAllQuotas: async automatic => runnerFlags.push(automatic) }) },
  }, { setInterval: callback => { intervals.push(callback); return 1; }, clearInterval() {} });
  runner.default(); const cleanup = effects[0](); assert.equal(runnerFlags.length, 0);
  intervals[0](); await flush(); assert.deepEqual(runnerFlags, [true]); cleanup(); count++;

  // Extract the real component closures using TypeScript's AST, avoiding a UI renderer.
  const mini = source('src/components/layout/MiniView.tsx');
  const ast = ts.createSourceFile('MiniView.tsx', mini, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  let refresh, timer;
  function visit(node) {
    if (ts.isVariableDeclaration(node) && node.name.getText(ast) === 'handleRefresh') refresh = node.initializer.getText(ast);
    if (ts.isCallExpression(node) && node.expression.getText(ast) === 'useEffect' && node.arguments[0]?.getText(ast).includes('handleRefreshRef.current')) timer = node.arguments[0].getText(ast);
    ts.forEachChild(node, visit);
  }
  visit(ast); assert.ok(refresh && timer);
  for (const activeTab of ['pool', 'classic']) {
    const used = [];
    const { run } = execute(`export const run = ${refresh};`, {}, {
      isRefreshing: false, activeTab, displayedAccount: accounts[0], setIsRefreshing() {},
      refreshAllQuotas: async automatic => used.push(['all', automatic]),
      refreshQuota: async (id, automatic) => used.push(['single', id, automatic]),
      fetchAccounts: async () => {}, fetchCurrentAccount: async () => {}, setTimeout: callback => callback(),
    });
    await run(true); await run();
    assert.deepEqual(used, activeTab === 'pool' ? [['all', true], ['all', false]] : [['single', 'a', true], ['single', 'a', false]]);
  }
  count++;
  const timerFlags = []; let tick;
  const { effect } = execute(`export const effect = ${timer};`, {}, {
    config: { auto_refresh: true, refresh_interval: 1 }, document: { hidden: false },
    handleRefreshRef: { current: automatic => timerFlags.push(automatic) },
    setInterval: callback => { tick = callback; return 1; }, clearInterval() {},
  });
  effect(); tick(); assert.deepEqual(timerFlags, [true]); count++;
  console.log(`${count} automatic refresh source-behavior cases passed`);
}
main().catch(error => { console.error(error); process.exitCode = 1; });
