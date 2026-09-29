// Intrinsic subclass payloads and user functions that share intrinsic prototypes.
(() => {
  class A extends Array { field = 'a'; }
  const a = new A(3); a[1] = 8;
  console.log('array:' + (a instanceof A) + ':' + Array.isArray(a) + ':' +
    a.length + ':' + (0 in a) + ':' + a[1] + ':' + a.field);
  for (const Base of [Boolean, Number, String, Date, RegExp]) {
    class Child extends Base { field = 'child'; }
    const x = new Child(Base === RegExp ? 'a+' : 17);
    const value = Base === Date ? x.getTime() : Base === RegExp ? x.test('aaa') : x.valueOf();
    console.log('boxed:' + Base.name + ':' + (Object.getPrototypeOf(x) === Child.prototype) + ':' + value + ':' + x.field);
  }
  for (const Base of [Int8Array, Uint8Array, Uint8ClampedArray, Int16Array,
    Uint16Array, Int32Array, Uint32Array, Float32Array, Float64Array, BigInt64Array, BigUint64Array]) {
    class Child extends Base { field = 'view'; }
    const big = Base === BigInt64Array || Base === BigUint64Array;
    const values = big ? [1n, 2n, 3n] : [1, 2, 3];
    const x = new Child(values);
    console.log('typed:' + Base.name + ':' + (x instanceof Child) + ':' + x.join(',') + ':' + x.field);
  }
  class T extends Uint8Array {}
  class D extends DataView { getUint8(i) { return super.getUint8(i) + 1; } }
  const buffer = new ArrayBuffer(8, {maxByteLength: 32});
  const tracking = new T(buffer, 2), fixed = new T(buffer, 2, 4);
  const view = new D(buffer, 2);
  tracking[0] = 7;
  console.log('views:' + view.getUint8(0) + ':' + view.byteLength + ':' + tracking.length + ':' + fixed.length);
  buffer.resize(16);
  console.log('grown:' + view.byteLength + ':' + tracking.length + ':' + fixed.length);
  buffer.resize(3);
  console.log('shrunk:' + view.byteLength + ':' + tracking.length + ':' + fixed.length);
  class B extends ArrayBuffer {}
  const own = new B(4, {maxByteLength: 8}); own.resize(6);
  console.log('buffer:' + (own instanceof B) + ':' + own.byteLength + ':' + own.maxByteLength);
  const GF = Object.getPrototypeOf(function*(){}).constructor;
  class F extends Function {}
  class G extends GF {}
  const f = new F('x', 'return x + 2;');
  const g = new G('x', 'yield x + 3;');
  console.log('functions:' + (f instanceof F) + ':' + f(5) + ':' + (g instanceof G) + ':' + g(5).next().value);
  const AF = Object.getPrototypeOf(async function(){}).constructor;
  const AGF = Object.getPrototypeOf(async function*(){}).constructor;
  class AsyncF extends AF {}
  class AsyncG extends AGF {}
  const af = new AsyncF('x', 'return x + 4;');
  const ag = new AsyncG('x', 'yield x + 5;');
  af(5).then(v => console.log('async:' + (af instanceof AsyncF) + ':' + v));
  ag(5).next().then(r => console.log('async-generator:' + (ag instanceof AsyncG) + ':' + r.value));
})();
