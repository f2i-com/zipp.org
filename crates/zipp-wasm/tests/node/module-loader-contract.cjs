// The WASM host has no filesystem loader. Removing unreachable loader code
// must preserve property lookup, import coercion, error order and microtasks.
'use strict';
const assert = require('node:assert/strict');
const { Engine } = require('./pkg/zipp_wasm.js');
function check(source, expected) {
  const e = new Engine();
  try { e.initScript(source); assert.deepEqual(e.takeOutput(), expected); }
  finally { e.dispose(); e.free(); }
}

check(`
  const symbol = Symbol('x');
  const target = Object.create({inherited: 1});
  target.own = 2; target[symbol] = 3;
  const proxy = new Proxy(target, {
    has(t, k) { print('has', typeof k === 'symbol' ? 'symbol' : k); return Reflect.has(t, k); }
  });
  print('own' in proxy, Reflect.has(proxy, 'inherited'), symbol in proxy, 'missing' in proxy);
  const child = Object.create(proxy);
  print('inherited' in child, Reflect.has(child, symbol));
  const fixed = Object.defineProperty({}, 'fixed', {value: 1, configurable: false});
  try { print('fixed' in new Proxy(fixed, {has(){return false;}})); }
  catch (e) { print('invariant', e.name); }
  const revoked = Proxy.revocable({}, {}); revoked.revoke();
  try { Reflect.has(revoked.proxy, 'x'); } catch(e) { print('revoked', e.name); }
`, [
  'has own', 'has inherited', 'has symbol', 'has missing', 'true true true false',
  'has inherited', 'has symbol', 'true true', 'invariant TypeError', 'revoked TypeError',
]);

check(`
  const marker = {};
  const p = import(
    (print('specifier'), {toString(){print('coerce'); throw marker;}}),
    (print('options'), {get with(){print('unexpected getter'); return {};}})
  );
  print('returned', p instanceof Promise);
  p.then(()=>print('unexpected success'), e=>print('same', e===marker));
  print('sync tail');
`, ['specifier', 'options', 'coerce', 'returned true', 'sync tail', 'same true']);

check(`
  function record(label, p) { p.then(()=>print(label, 'unexpected success'), e=>print(label, e.name)); }
  record('normal', import('./missing.mjs'));
  record('options', import('./missing.mjs', 1));
  record('source', import.source('./missing.mjs', {}));
  record('source-options', import.source('./missing.mjs', 1));
  record('defer', import.defer('./missing.mjs', {}));
  print('sync tail');
`, ['sync tail', 'normal TypeError', 'options TypeError', 'source SyntaxError', 'source-options TypeError', 'defer TypeError']);

check(`
  const realm = new ShadowRealm();
  const marker = {};
  try { realm.importValue({toString(){throw marker;}}, 'x'); }
  catch(e) { print('coercion', e===marker); }
  try { realm.importValue('./missing.mjs', 1); }
  catch(e) { print('name', e.name); }
  const p = realm.importValue('./missing.mjs', 'x');
  print('returned', p instanceof Promise);
  p.then(()=>print('unexpected success'), e=>print('rejected', e.name, e.message));
  print('sync tail');
`, ['coercion true', 'name TypeError', 'returned true', 'sync tail', 'rejected TypeError ShadowRealm.prototype.importValue: no module base']);

console.log('property lookup and no-filesystem module contract passed');
