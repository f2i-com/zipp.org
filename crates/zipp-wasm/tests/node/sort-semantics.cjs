// Ordering contracts exercised by the size-reduced compiler and shared
// TypedArray sorter. Run against any variant through pkg-redirect.cjs.
'use strict';
const assert = require('node:assert/strict');
const { Engine, zippProfile } = require('./pkg/zipp_wasm.js');
const engine = new Engine();
try {
  engine.initScript(`
    function check(value, name) { if (!value) throw new Error(name); }
    function key(x) { return Number.isNaN(x) ? 'nan' : Object.is(x, -0) ? '-0' : String(x); }
    for (const method of ['sort', 'toSorted']) {
      const input = new Float64Array([NaN, 2, -0, 0, -3, NaN]);
      const result = input[method]();
      check(Array.from(result, key).join(',') === '-3,-0,0,2,nan,nan', method + ' numeric');
      check(method === 'sort' ? result === input : Number.isNaN(input[0]) && result !== input, method + ' identity');
      const ties = new Int32Array([21, 11, 22, 12])[method]((a, b) => Math.floor(a / 10) - Math.floor(b / 10));
      check(Array.from(ties).join(',') === '11,12,21,22', method + ' stable ties');
      const visits = [];
      new Int32Array([3, 1, 2])[method]((a, b) => {
        visits.push(a + ':' + b);
        return { valueOf() { visits.push('coerce'); return a - b; } };
      });
      check(visits.join(',') === '3:1,coerce,3:2,coerce,1:2,coerce', method + ' callback order');
      const marker = {};
      let caught = false;
      try { new Int32Array([2, 1])[method](() => ({ valueOf() { throw marker; } })); }
      catch (e) { caught = e === marker; }
      check(caught, method + ' abrupt coercion');
    }
    const log = [];
    function dec(value, context) { log.push(context.name); }
    class Decorated {
      @dec field1;
      @dec method1() {}
      @dec static fieldS1;
      @dec static methodS1() {}
      @dec field2;
      @dec static methodS2() {}
      @dec method2() {}
      @dec static fieldS2;
    }
    check(log.join(',') === 'methodS1,methodS2,method1,method2,fieldS1,fieldS2,field1,field2', 'decorator groups');
    print('ordering contracts passed');
  `);
  assert.deepEqual(engine.takeOutput(), ['ordering contracts passed']);
} finally { engine.dispose(); engine.free(); }

if (JSON.parse(zippProfile()).languages.includes('python')) {
  // More than 32 candidates, mixing immediate and allocating integers, with
  // repeated constants and negatives. Hoisting must preserve the result even
  // when it reaches its register budget partway through the priority groups.
  const terms = Array.from({ length: 40 }, (_, i) => i % 2 ? BigInt(i) : (1n << 65n) + BigInt(i));
  terms.push(-7n, -((1n << 70n) + 1n), terms[0]);
  const expected = terms.reduce((a, b) => a + b, 0n) * 3n;
  const py = new Engine();
  try {
    py.initSource(`def f():\n    total = 0\n    for i in range(3):\n        total += ${terms.join(' + ')}\n    return total\nprint(f())\n`, 'python');
    assert.deepEqual(py.takeOutput(), [String(expected)]);
  } finally { py.dispose(); py.free(); }
}
console.log('stable grouping and TypedArray ordering passed');
