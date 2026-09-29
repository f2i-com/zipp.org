'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const {Engine, zippProfile} = require(path.resolve(process.argv[2] || path.join(__dirname, 'pkg'), 'zipp_wasm.js'));
let source = fs.readFileSync(path.resolve(__dirname, '../../../zipp-vm/tests/fixtures/builtin_subclass_payloads.js'), 'utf8');
if (JSON.parse(zippProfile()).variant === 'lite') {
  source += fs.readFileSync(path.resolve(__dirname, '../../../zipp-vm/tests/fixtures/builtin_subclass_identity.js'), 'utf8');
}
const expected = [];
vm.runInNewContext(source, {console: {log: value => expected.push(String(value))}}, {microtaskMode: 'afterEvaluate'});
const e = new Engine();
try { e.initScript(source); assert.deepEqual(e.takeOutput(), expected); }
finally { e.dispose(); e.free(); }
console.log('intrinsic subclass copies and resizable views match Node; Lite also checks user-constructor identity');
