// Warm Promise dispatch, then mutate values, attributes and property layout.
'use strict';
const assert = require('node:assert/strict');
const vm = require('node:vm');
const path = require('node:path');
const { Engine } = require(path.resolve(process.argv[2] || path.join(__dirname, 'pkg'), 'zipp_wasm.js'));
const source = `
  const proto = Promise.prototype;
  const descriptor = Object.getOwnPropertyDescriptor(proto, 'then');
  const p = Promise.resolve(1);
  for (let i = 0; i < 100; i++) p.then(v => v);
  proto.then = function () { return 'assigned'; };
  print(p.then());
  Object.defineProperty(proto, 'then', descriptor);
  p.then(v => print('restored-value', v));
  delete proto.then;
  proto.padding = function () { return 'wrong-slot'; };
  proto.then = function () { return 'reinserted'; };
  print(p.then());
  Object.defineProperty(proto, 'then', descriptor);
  p.then(v => print('restored-layout', v));
  let reads = 0;
  Object.defineProperty(proto, 'then', {
    configurable: true,
    get() { reads++; return function () { return 'accessor'; }; }
  });
  print(p.then(), reads);
  Object.defineProperty(proto, 'then', descriptor);
  p.then = function () { return 'own'; };
  print(p.then());
  delete p.then;
  p.then(v => print('unshadowed', v));
  delete proto.padding;
  let holder = async function retained() {
    for (let i = 0; i < 3000; i++) {
      await (i % 2 ? Promise.resolve(i) : i);
      if (retained.tag !== 17) throw new Error('callee identity changed');
    }
    return retained.tag;
  };
  holder.tag = 17;
  const done = holder();
  holder = null;
  done.then(v => print('callee', v));
`;
function check(program) {
  const expected = [];
  const print = (...args) => expected.push(args.join(' '));
  vm.runInNewContext(program, {print, console:{log:print}}, {microtaskMode:'afterEvaluate'});
  const e = new Engine();
  try {
    e.initScript(program);
    assert.deepEqual(e.takeOutput(), expected);
  } finally { e.dispose(); e.free(); }
}
check(source);
check(require('node:fs').readFileSync(path.resolve(__dirname, '../../../zipp-vm/tests/fixtures/promise_method_fallbacks.js'), 'utf8'));
console.log('Promise mutations, method fallbacks and async callee identity match Node');
