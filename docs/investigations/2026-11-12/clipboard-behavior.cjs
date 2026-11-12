// Actual repository source; isolated clipboard/DOM mocks, no browser or network.
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const root = path.resolve(process.argv[2] || process.cwd());
const ts = require(path.join(root, 'node_modules/typescript'));
const source = fs.readFileSync(path.join(root, 'src/utils/clipboard.ts'), 'utf8');
const code = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2021 } }).outputText;
function deferred() { let resolve, reject; const promise = new Promise((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; }
const sentinel = 'TEST-ONLY-CLIPBOARD-SECRET';
const logs = [];
const unhandled = [];
process.on('unhandledRejection', error => unhandled.push(error));
function fixture(options = {}) {
  const nodes = [];
  const copied = [];
  const clipboard = options.clipboard;
  class Item { constructor(data) { if (options.itemThrows) throw new Error(sentinel); this.data = data; } }
  const document = {
    body: { appendChild(node) { node.attached = true; nodes.push(node); } },
    createElement() {
      return { value: '', style: {}, attached: false,
        focus() { if (options.focusThrows) throw new Error(sentinel); }, select() {},
        remove() { this.attached = false; },
      };
    },
    execCommand(command) {
      assert.equal(command, 'copy');
      copied.push(nodes.at(-1).value);
      if (options.execThrows) throw new Error(sentinel);
      return options.execResult !== false;
    },
  };
  const exports = {};
  vm.runInNewContext(code, { exports, navigator: { clipboard }, window: { isSecureContext: options.secure !== false },
    ClipboardItem: options.hasItem === false ? undefined : Item, Blob, document,
    console: { error(...args) { logs.push(args); } },
  }, { filename: 'src/utils/clipboard.ts' });
  return { copy: exports.copyToClipboard, nodes, copied };
}
let completed = 0;
async function check(name, fn) { await fn(); completed++; console.log(`PASS ${name}`); }
(async () => {
  await check('plain strings preserve synchronous modern writeText behavior', async () => {
    const calls = [];
    const f = fixture({ clipboard: { writeText(text) { calls.push(text); return Promise.resolve(); } } });
    const result = f.copy(sentinel);
    assert.deepEqual(calls, [sentinel]);
    assert.equal(await result, true); assert.equal(f.nodes.length, 0);
  });
  await check('lazy reveal starts clipboard write during gesture before the loader resolves', async () => {
    const reveal = deferred(); let loads = 0, writes = 0, gesture = true, copied;
    const f = fixture({ clipboard: {
      write(items) { assert.equal(gesture, true); writes++; return items[0].data['text/plain'].then(async blob => { assert.equal(blob.type, 'text/plain'); copied = await blob.text(); }); },
      writeText() { throw new Error('unexpected fallback'); },
    } });
    const result = f.copy(() => { loads++; return reveal.promise; });
    assert.equal(writes, 1); assert.equal(loads, 0); gesture = false;
    await Promise.resolve(); assert.equal(loads, 1); assert.equal(copied, undefined);
    reveal.resolve(sentinel); assert.equal(await result, true); assert.equal(copied, sentinel);
    assert.equal(loads, 1); assert.equal(f.nodes.length, 0);
  });
  await check('loader rejection fails without writing plaintext DOM or logging sensitive errors', async () => {
    let loads = 0;
    const f = fixture({ clipboard: { write(items) { return items[0].data['text/plain'].then(() => {}); } } });
    assert.equal(await f.copy(() => { loads++; return Promise.reject(new Error(sentinel)); }), false);
    assert.equal(loads, 1); assert.equal(f.nodes.length, 0);
  });
  await check('denied deferred write falls back with exactly one secret retrieval', async () => {
    let loads = 0, writes = 0, plain = [];
    const f = fixture({ clipboard: { write() { writes++; return Promise.reject(new Error(sentinel)); }, writeText(text) { plain.push(text); return Promise.resolve(); } } });
    assert.equal(await f.copy(() => { loads++; return Promise.resolve(sentinel); }), true);
    assert.equal(loads, 1); assert.equal(writes, 1); assert.deepEqual(plain, [sentinel]);
  });
  await check('unsupported promise ClipboardItem constructor preserves modern fallback', async () => {
    let loads = 0; const plain = [];
    const f = fixture({ itemThrows: true, clipboard: { write() { throw new Error('unexpected write'); }, writeText(text) { plain.push(text); return Promise.resolve(); } } });
    assert.equal(await f.copy(() => { loads++; return Promise.resolve(sentinel); }), true);
    assert.equal(loads, 1); assert.deepEqual(plain, [sentinel]);
  });
  await check('older secure clients without ClipboardItem retain lazy writeText fallback', async () => {
    let loads = 0; const plain = [];
    const f = fixture({ hasItem: false, clipboard: { writeText(text) { plain.push(text); return Promise.resolve(); } } });
    assert.equal(await f.copy(() => { loads++; return Promise.resolve(sentinel); }), true);
    assert.equal(loads, 1); assert.deepEqual(plain, [sentinel]);
  });
  await check('HTTP legacy fallback clears and removes its temporary secret DOM', async () => {
    const f = fixture({ secure: false });
    assert.equal(await f.copy(() => Promise.resolve(sentinel)), true);
    assert.deepEqual(f.copied, [sentinel]); assert.equal(f.nodes.length, 1);
    assert.equal(f.nodes[0].value, ''); assert.equal(f.nodes[0].attached, false);
  });
  await check('execCommand rejection still clears and removes temporary secret DOM', async () => {
    const f = fixture({ secure: false, execThrows: true });
    assert.equal(await f.copy(sentinel), false); assert.equal(f.nodes[0].value, ''); assert.equal(f.nodes[0].attached, false);
  });
  await check('DOM focus failure also cleans up after modern copy failure', async () => {
    const f = fixture({ focusThrows: true, clipboard: { writeText() { return Promise.reject(new Error(sentinel)); } } });
    assert.equal(await f.copy(sentinel), false); assert.equal(f.nodes[0].value, ''); assert.equal(f.nodes[0].attached, false);
  });
  await check('early permission denial plus later loader rejection causes no unhandled promise', async () => {
    const reveal = deferred(); let loads = 0;
    const f = fixture({ clipboard: { write() { return Promise.reject(new Error(sentinel)); } } });
    const result = f.copy(() => { loads++; return reveal.promise; });
    await Promise.resolve(); reveal.reject(new Error(sentinel));
    assert.equal(await result, false); assert.equal(loads, 1); assert.equal(f.nodes.length, 0);
    await new Promise(resolve => setImmediate(resolve)); assert.equal(unhandled.length, 0);
  });
  assert.equal(JSON.stringify(logs).includes(sentinel), false, 'logs must not contain clipboard contents or error details');
  assert.equal(completed, 10);
  console.log(`${completed}/10 clipboard behavior checks passed`);
})().catch(error => { console.error(error); process.exitCode = 1; });
process.on('beforeExit', () => { if (completed !== 10) { console.error(`Incomplete clipboard checks: ${completed}/10`); process.exitCode = 1; } });
