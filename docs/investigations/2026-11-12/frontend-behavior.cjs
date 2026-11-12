// Executes transpiled repository source with deterministic service/hook mocks.
// No application build, browser, network calls, or additional packages.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const childProcess = require('node:child_process');
const root = path.resolve(process.argv[2] || process.cwd());
const ts = require(path.join(root, 'node_modules/typescript'));
const quiet = { log() {}, info() {}, debug() {}, warn() {}, error() {} };
function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
async function flush() { for (let i = 0; i < 30; i++) await Promise.resolve(); }
function clock() {
  let now = 0, next = 1;
  const timers = new Map();
  function add(fn, delay, interval) {
    const id = next++;
    timers.set(id, { fn, at: now + delay, interval });
    return id;
  }
  return {
    setTimeout: (fn, delay) => add(fn, delay, 0),
    clearTimeout: id => timers.delete(id),
    setInterval: (fn, delay) => add(fn, delay, delay),
    clearInterval: id => timers.delete(id),
    async tick(duration) {
      const end = now + duration;
      while (true) {
        const due = [...timers].filter(([, value]) => value.at <= end)
          .sort((a, b) => a[1].at - b[1].at || a[0] - b[0])[0];
        if (!due) break;
        const [id, timer] = due;
        now = timer.at;
        if (timer.interval) timer.at += timer.interval;
        else timers.delete(id);
        timer.fn();
        await flush();
      }
      now = end;
      await flush();
    },
  };
}
function loadSource(relative, modules, globals = {}) {
  const source = process.argv.includes('--baseline')
    ? childProcess.execFileSync('git', ['show', 'HEAD:' + relative], { cwd: root, encoding: 'utf8' })
    : fs.readFileSync(path.join(root, relative), 'utf8');
  const code = ts.transpileModule(source, { compilerOptions: {
    module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2020,
    jsx: ts.JsxEmit.React, esModuleInterop: true,
  }, fileName: relative }).outputText;
  const exports = {};
  vm.runInNewContext(code, {
    exports, require(name) {
      if (Object.hasOwn(modules, name)) return modules[name];
      throw new Error('Unexpected dependency: ' + name);
    }, console: quiet, localStorage: { getItem() { return null; }, setItem() {}, removeItem() {} },
    ...globals,
  }, { filename: relative });
  return exports;
}
function createStore(initializer) {
  let state;
  const get = () => state;
  const set = update => {
    const value = typeof update === 'function' ? update(state) : update;
    state = { ...state, ...value };
  };
  state = initializer(set, get);
  const store = selector => selector ? selector(state) : state;
  store.getState = get;
  return store;
}
function accountFixture() {
  const time = clock();
  const discoveries = [];
  const service = {
    listAccounts: async () => [], getCurrentAccount: async () => null,
    getActiveClientsStatus() { const result = deferred(); discoveries.push(result); return result.promise; },
    switchAccount: async () => {}, addAccount: async () => {}, deleteAccount: async () => {},
    deleteAccounts: async () => {}, fetchAccountQuota: () => new Promise(() => {}),
    updateAccountPriority: async () => {}, reorderAccounts: async () => {},
    updateAccountProjectId: async () => {}, autoFetchAccountProjectId: async () => 'auto-project',
    syncAccountFromDb: async () => null,
  };
  const exports = loadSource('src/stores/useAccountStore.ts', {
    zustand: { create: createStore }, '../services/accountService': service,
  }, time);
  return { time, discoveries, service, store: exports.useAccountStore };
}
function monitorFixture({ tauri = true, delayLogs = false, delayListener = false } = {}) {
  const time = clock(), states = [], refs = [], effects = [], callbacks = [], calls = [], logReads = [], elements = [];
  let eventCallback, accountsRead = 0, unlistens = 0;
  const registration = deferred();
  const react = {
    createElement(type, props) { elements.push({ type, props }); return null; },
    useState(initial) {
      const slot = { value: typeof initial === 'function' ? initial() : initial };
      states.push(slot);
      return [slot.value, value => { slot.value = typeof value === 'function' ? value(slot.value) : value; }];
    },
    useRef(initial) { const ref = { current: initial, initial }; refs.push(ref); return ref; },
    useMemo: fn => fn(), useCallback(fn) { callbacks.push(fn); return fn; },
    useEffect(fn) { effects.push(fn); },
  };
  function invoke(command, args) {
    calls.push({ command, args });
    if (command === 'load_config') return Promise.resolve({ proxy: { enable_logging: true, capture_health_logs: false } });
    if (command === 'get_proxy_logs_count_filtered') return Promise.resolve(7);
    if (command === 'get_proxy_stats') return Promise.resolve({ total_requests: 7, success_count: 6, error_count: 1 });
    if (command === 'get_proxy_logs_filtered') {
      const result = deferred(); logReads.push({ ...result, args });
      if (!delayLogs) result.resolve([{ id: 'live', timestamp: 1 }]);
      return result.promise;
    }
    return Promise.resolve(null);
  }
  const modules = {
    react: { ...react, default: react, __esModule: true },
    '@tauri-apps/api/event': { listen(_event, callback) {
      eventCallback = callback;
      return delayListener ? registration.promise : Promise.resolve(() => { unlistens++; });
    } },
    '../common/ModalDialog': { default() {} },
    'react-i18next': { useTranslation: () => ({ t: key => key }) },
    '../../utils/request': { request: invoke },
    'lucide-react': {}, '../../utils/format': { formatCompactNumber: value => String(value) },
    '../../stores/useAccountStore': { useAccountStore: () => ({ accounts: [], fetchAccounts: async () => { accountsRead++; } }) },
    '../../utils/env': { isTauri: () => tauri }, './RequestDetailModal': {}, '../accounts/QoSStatusBadge': {},
  };
  const browser = { ...time, addEventListener() {}, removeEventListener() {} };
  const exports = loadSource('src/components/proxy/ProxyMonitor.tsx', modules, {
    ...time, window: browser, document: { addEventListener() {}, removeEventListener() {} },
  });
  exports.ProxyMonitor({});
  return {
    time, states, refs, calls, logReads, elements,
    load: callbacks.find(fn => fn.toString().includes('pendingQueryRef')),
    mainEffect: effects.find(fn => fn.toString().includes('setupListener')),
    emit: () => eventCallback({ payload: { id: 'event', response_body: 'large body' } }),
    resolveListener: () => registration.resolve(() => { unlistens++; }),
    accountReads: () => accountsRead, unlistens: () => unlistens,
  };
}
const tests = [];
function test(name, fn) { tests.push({ name, fn }); }
test('concurrent account/current/explicit discovery performs one request', async () => {
  const f = accountFixture(), actions = f.store.getState();
  const jobs = [actions.fetchAccounts(), actions.fetchCurrentAccount(), actions.fetchActiveClients()];
  assert.equal(f.discoveries.length, 1);
  f.discoveries[0].resolve({ agy: 'current' }); await Promise.all(jobs);
  assert.equal(f.store.getState().activeClients.agy, 'current');
});
test('pre-switch discovery cannot overwrite the new client', async () => {
  const f = accountFixture(), actions = f.store.getState();
  const old = actions.fetchActiveClients();
  const switched = actions.switchAccount('new'); await flush();
  assert.equal(f.discoveries.length, 2);
  f.discoveries[1].resolve({ agy: 'new' }); await switched;
  f.discoveries[0].resolve({ agy: 'old' }); await old;
  assert.equal(f.store.getState().activeClients.agy, 'new');
});
test('discovery timeout preserves state and permits a fresh retry', async () => {
  const f = accountFixture(), actions = f.store.getState();
  const first = actions.fetchActiveClients(); f.discoveries[0].resolve({ agy: 'known' }); await first;
  const stalled = actions.fetchActiveClients(); await f.time.tick(10000); await stalled;
  assert.equal(f.store.getState().activeClients.agy, 'known');
  const retry = actions.fetchActiveClients(); assert.equal(f.discoveries.length, 3);
  f.discoveries[2].resolve({ agy: 'fresh' }); await retry;
  f.discoveries[1].resolve({ agy: 'late' }); await flush();
  assert.equal(f.store.getState().activeClients.agy, 'fresh');
});
test('pre-delete list/current snapshots cannot resurrect the deleted account', async () => {
  const f = accountFixture(), actions = f.store.getState();
  f.service.listAccounts = async () => [{ id: 'deleted' }];
  f.service.getCurrentAccount = async () => ({ id: 'deleted' });
  const stale = [actions.fetchAccounts(), actions.fetchCurrentAccount()];
  await flush();
  f.service.listAccounts = async () => [{ id: 'remaining' }];
  f.service.getCurrentAccount = async () => ({ id: 'remaining' });
  const deleted = actions.deleteAccounts(['deleted']); await flush();
  f.discoveries[1].resolve({}); await deleted;
  assert.equal(f.store.getState().accounts[0].id, 'remaining');
  f.discoveries[0].resolve({}); await Promise.all(stale);
  assert.equal(f.store.getState().accounts[0].id, 'remaining');
  assert.equal(f.store.getState().currentAccount.id, 'remaining');
});
test('older list failure cannot replace a newer successful account snapshot', async () => {
  const f = accountFixture(), actions = f.store.getState(), first = deferred();
  f.service.listAccounts = () => first.promise;
  const old = actions.fetchAccounts();
  f.service.listAccounts = async () => [{ id: 'fresh' }];
  const fresh = actions.fetchAccounts(); f.discoveries[0].resolve({}); await fresh;
  first.reject(new Error('stale read failed')); await old;
  assert.equal(f.store.getState().accounts[0].id, 'fresh');
  assert.equal(f.store.getState().error, null);
});
test('older current-account response cannot overwrite the latest response', async () => {
  const f = accountFixture(), actions = f.store.getState(), first = deferred();
  f.service.getCurrentAccount = () => first.promise;
  const old = actions.fetchCurrentAccount();
  f.service.getCurrentAccount = async () => ({ id: 'new' });
  const fresh = actions.fetchCurrentAccount(); f.discoveries[0].resolve({}); await fresh;
  first.resolve({ id: 'old' }); await old;
  assert.equal(f.store.getState().currentAccount.id, 'new');
});
test('an acknowledged priority edit survives older list/current snapshots', async () => {
  const f = accountFixture(), actions = f.store.getState();
  f.service.listAccounts = async () => [{ id: 'account', priority: 1 }];
  f.service.getCurrentAccount = async () => ({ id: 'account', priority: 1 });
  let reads = [actions.fetchAccounts(), actions.fetchCurrentAccount()];
  f.discoveries[0].resolve({}); await Promise.all(reads);
  reads = [actions.fetchAccounts(), actions.fetchCurrentAccount()]; await flush();
  f.service.updateAccountPriority = async () => {
    f.service.getCurrentAccount = async () => ({ id: 'account', priority: 9 });
  };
  await actions.updateAccountPriority('account', 9);
  f.discoveries[1].resolve({}); await Promise.all(reads);
  assert.equal(f.store.getState().accounts[0].priority, 9);
  assert.equal(f.store.getState().currentAccount.priority, 9);
  assert.equal(f.store.getState().loading, false);
});
test('an older failed reorder does not roll back a newer list snapshot', async () => {
  const f = accountFixture(), actions = f.store.getState(), order = deferred();
  f.service.reorderAccounts = () => order.promise;
  const reordered = actions.reorderAccounts(['old']);
  const failure = assert.rejects(reordered, /reorder failed/);
  f.service.listAccounts = async () => [{ id: 'latest' }];
  const read = actions.fetchAccounts(); f.discoveries[0].resolve({}); await read;
  order.reject(new Error('reorder failed')); await failure;
  assert.equal(f.store.getState().accounts[0].id, 'latest');
});
test('a delayed DB sync cannot overwrite a newer current-account selection', async () => {
  const f = accountFixture(), actions = f.store.getState(), synced = deferred();
  f.service.syncAccountFromDb = () => synced.promise;
  const old = actions.syncAccountFromDb();
  f.service.getCurrentAccount = async () => ({ id: 'new' });
  const read = actions.fetchCurrentAccount(); f.discoveries[0].resolve({}); await read;
  synced.resolve({ id: 'old' }); await flush(); f.discoveries[1].resolve({}); await old;
  assert.equal(f.store.getState().currentAccount.id, 'new');
});
for (const field of ['priority', 'project', 'auto-project']) test(`an A ${field} edit completing during switch B cannot discard B identity`, async () => {
  const f = accountFixture(), actions = f.store.getState(), edited = deferred(), reads = [];
  const A = { id: 'A', priority: 1, token: { project_id: 'old-project' } };
  const B = { id: 'B', priority: 2, token: { project_id: 'B-project' } };
  f.service.listAccounts = async () => [A, B];
  f.service.getCurrentAccount = async () => A;
  const initial = [actions.fetchAccounts(), actions.fetchCurrentAccount()];
  f.discoveries[0].resolve({}); await Promise.all(initial);
  f.service.updateAccountPriority = () => edited.promise;
  f.service.updateAccountProjectId = () => edited.promise;
  f.service.autoFetchAccountProjectId = () => edited.promise;
  const edit = field === 'priority' ? actions.updateAccountPriority('A', 9)
    : field === 'project' ? actions.updateAccountProjectId('A', 'new-project')
    : actions.autoFetchAccountProjectId('A');
  f.service.getCurrentAccount = () => { const read = deferred(); reads.push(read); return read.promise; };
  const switched = actions.switchAccount('B'); await flush();
  edited.resolve('new-project'); await edit; await flush();
  assert.equal(reads.length, 2, 'the invalidated identity read needs a replacement');
  reads[0].resolve(B);
  f.discoveries[1].resolve({}); await switched; await flush();
  reads[1].resolve(B); await flush();
  assert.equal(f.store.getState().currentAccount.id, 'B');
  assert.equal(f.store.getState().currentAccount.token.project_id, 'B-project');
  const savedA = f.store.getState().accounts.find(account => account.id === 'A');
  if (field === 'priority') assert.equal(savedA.priority, 9);
  else assert.equal(savedA.token.project_id, 'new-project');
});
test('field edits without a pending current read do not add identity RPCs', async () => {
  const f = accountFixture(), actions = f.store.getState(); let reads = 0;
  f.service.getCurrentAccount = async () => { reads++; return { id: 'A', token: {} }; };
  const initial = actions.fetchCurrentAccount(); f.discoveries[0].resolve({}); await initial;
  await actions.updateAccountPriority('A', 9);
  await actions.updateAccountProjectId('A', 'project');
  await actions.autoFetchAccountProjectId('A');
  const added = actions.addAccount('C@example.test', 'refresh'); await flush();
  f.discoveries[1].resolve({}); await added;
  assert.equal(reads, 1);
});
test('an add C completing during switch B cannot discard B identity', async () => {
  const f = accountFixture(), actions = f.store.getState(), added = deferred(), reads = [];
  f.service.getCurrentAccount = async () => ({ id: 'A' });
  const initial = actions.fetchCurrentAccount(); f.discoveries[0].resolve({}); await initial;
  f.service.addAccount = () => added.promise;
  const add = actions.addAccount('C@example.test', 'refresh');
  f.service.getCurrentAccount = () => { const read = deferred(); reads.push(read); return read.promise; };
  const switched = actions.switchAccount('B'); await flush();
  added.resolve(); await flush();
  reads[0].resolve({ id: 'B' }); f.discoveries[1].resolve({});
  await switched; await flush();
  if (reads[1]) reads[1].resolve({ id: 'B' });
  f.discoveries[2].resolve({}); await add; await flush();
  assert.equal(f.store.getState().currentAccount.id, 'B');
});
test('batch deletion cleanup errors still refresh committed index state', async () => {
  const f = accountFixture(), actions = f.store.getState();
  f.service.listAccounts = async () => [{ id: 'removed' }];
  const initial = actions.fetchAccounts(); f.discoveries[0].resolve({}); await initial;
  f.service.listAccounts = async () => [];
  f.service.deleteAccounts = async () => { throw new Error('cleanup failed; review remaining credentials'); };
  const deleted = actions.deleteAccounts(['removed']);
  const failure = assert.rejects(deleted, /cleanup failed/);
  await flush(); f.discoveries[1].resolve({}); await failure;
  assert.equal(f.store.getState().accounts.length, 0);
  assert.match(f.store.getState().error, /cleanup failed/);
});
test('monitor serializes work and replaces pending B with latest C', async () => {
  const f = monitorFixture({ delayLogs: true });
  const work = f.load(1, 'A', ''); f.load(1, 'B', ''); f.load(1, 'C', '');
  assert.equal(f.logReads.length, 1);
  f.logReads[0].resolve([{ id: 'A' }]); await flush();
  assert.equal(f.logReads.length, 2); assert.equal(f.logReads[1].args.filter, 'C');
  assert.equal(f.states[0].value.length, 0, 'old A result must not publish');
  f.logReads[1].resolve([{ id: 'C' }]); await work;
  assert.equal(f.states[0].value[0].id, 'C');
  assert.equal(f.calls.filter(call => call.command.startsWith('set_proxy_')).length, 0);
});
test('monitor snapshots current page size and account/error filters', async () => {
  const f = monitorFixture(); f.refs.find(ref => ref.initial === 100).current = 500;
  await f.load(2, '__ERROR__', 'alice@example.test');
  const args = f.logReads[0].args;
  assert.equal(args.limit, 500); assert.equal(args.offset, 500);
  assert.equal(args.filter, 'alice@example.test'); assert.equal(args.errorsOnly, true);
});
test('clearing logs invalidates a read already in progress', async () => {
  const f = monitorFixture({ delayLogs: true });
  const work = f.load(1, '', '');
  const dialog = f.elements.find(element => element.props?.title === 'monitor.dialog.clear_title');
  await dialog.props.onConfirm();
  f.logReads[0].resolve([{ id: 'deleted' }]); await flush();
  assert.equal(f.states[0].value.length, 0, 'pre-clear rows must not reappear');
  assert.equal(f.logReads.length, 2);
  f.logReads[1].resolve([]); await work;
  assert.equal(f.states[0].value.length, 0);
});
test('continuous traffic flushes every batch without scanning accounts each batch', async () => {
  const f = monitorFixture(); const cleanup = f.mainEffect(); await flush();
  for (let i = 0; i < 20; i++) { f.emit(); await f.time.tick(100); }
  assert.equal(f.logReads.length, 4, 'fixed 500ms window must not wait for traffic to stop');
  assert.equal(f.accountReads(), 1, 'only initial discovery during 2 seconds of traffic');
  assert.equal(f.calls.filter(call => call.command === 'load_config').length, 0, 'live batches need no config GET');
  await f.time.tick(8000); assert.equal(f.accountReads(), 2, 'independent 10-second discovery');
  cleanup();
});
test('late asynchronous listener registration is immediately released', async () => {
  const f = monitorFixture({ delayListener: true }); const cleanup = f.mainEffect();
  cleanup(); f.resolveListener(); await flush(); assert.equal(f.unlistens(), 1);
  f.emit(); await f.time.tick(1000); assert.equal(f.logReads.length, 0);
});
test('Web polling reads the latest refs rather than the initial page/limit', async () => {
  const f = monitorFixture({ tauri: false }); const cleanup = f.mainEffect(); await flush();
  f.refs.find(ref => ref.initial === 100).current = 500;
  f.refs.find(ref => ref.initial === 1).current = 2;
  f.refs[0].current = 'claude'; f.refs[1].current = 'alice';
  await f.time.tick(10000);
  assert.equal(f.logReads.length, 1); assert.equal(f.logReads[0].args.limit, 500);
  assert.equal(f.logReads[0].args.offset, 500); assert.equal(f.logReads[0].args.filter, 'claude alice');
  cleanup();
});
let completed = 0;
process.on('beforeExit', () => {
  if (!process.exitCode && completed !== tests.length) {
    console.error(`FAIL harness stopped with a pending check (${completed}/${tests.length} completed)`);
    process.exitCode = 1;
  }
});
(async () => {
  for (const { name, fn } of tests) { await fn(); completed++; console.log('PASS ' + name); }
  console.log(tests.length + ' source-behavior checks passed');
})().catch(error => { console.error(error); process.exitCode = 1; });
