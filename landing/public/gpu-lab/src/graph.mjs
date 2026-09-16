/** Strict, versioned data-only compute protocol. No shader source or eval. */
export class ComputeError extends Error {
  constructor(code, message) { super(message); this.name = 'ComputeError'; this.code = code; }
}
export function check(ok, code, message) { if (!ok) throw new ComputeError(code, message); }
export const DEFAULT_LIMITS = Object.freeze({
  maxNodes: 512, maxElements: 4194304, maxInputElements: 4194304,
  maxOutputElements: 4194304, maxLogicalBytes: 64 * 1024 * 1024,
  maxWork: 100000000, maxDimension: 65536, maxOutputs: 64,
  maxWebGLTextureBytes: 128 * 1024 * 1024,
});
/** Highest tensor rank. Kernels address every tensor as four padded dimensions. */
export const MAX_RANK = 4;
export function sizeOf(shape) { return shape.reduce((a, b) => a * b, 1); }
// Operation families. Every name here is a fixed kernel; none becomes code.
export const BINARY_OPS = Object.freeze(['add', 'sub', 'mul', 'div']);
export const UNARY_OPS = Object.freeze(['relu', 'positive', 'neg', 'exp', 'log', 'sqrt', 'tanh', 'sigmoid', 'gelu', 'gelu_grad']);
export const REDUCE_OPS = Object.freeze(['sum', 'mean']);
function plain(obj) {
  return obj !== null && typeof obj === 'object' &&
    (Object.getPrototypeOf(obj) === Object.prototype || Object.getPrototypeOf(obj) === null);
}
function keys(obj, allowed, required = []) {
  check(plain(obj), 'PROTOCOL', 'Expected a plain data object');
  for (const k of Reflect.ownKeys(obj)) {
    check(typeof k === 'string' && allowed.includes(k), 'PROTOCOL', `Unknown field: ${String(k)}`);
    check('value' in Object.getOwnPropertyDescriptor(obj, k), 'PROTOCOL', 'Accessors are not data');
  }
  for (const k of required) check(Object.hasOwn(obj, k), 'PROTOCOL', `Missing field: ${k}`);
}
// Arrays are read by index, never with every/reduce, which skip holes.
function intArray(value, maxLength, what) {
  check(Array.isArray(value) && value.length <= maxLength, 'SHAPE', `${what} must be an array of at most ${maxLength} integers`);
  const out = [];
  for (let i = 0; i < value.length; i++) {
    check(Object.hasOwn(value, i) && Number.isSafeInteger(value[i]), 'SHAPE', `${what} must contain integers`);
    out.push(value[i]);
  }
  return out;
}
function shapeOf(shape, limits) {
  const dims = intArray(shape, MAX_RANK, 'Shape');
  let size = 1;
  for (const d of dims) {
    check(d > 0 && d <= limits.maxDimension, 'SHAPE', 'Shape dimensions must be positive bounded integers');
    // Checked after every factor, so the product never leaves the safe-integer range.
    size *= d; check(size <= limits.maxElements, 'LIMIT', 'Tensor exceeds element limit');
  }
  return dims;
}
function finiteF32(v) {
  check(typeof v === 'number' && Number.isFinite(v) && Number.isFinite(Math.fround(v)),
    'NUMBER', 'Values must be finite float32 numbers');
  return Math.fround(v);
}
/** An owned float32 copy of a flat number array, holes and non-finite values rejected. */
function float32Data(data) {
  const out = new Float32Array(data.length);
  for (let i = 0; i < data.length; i++) {
    const v = data[i];
    if (typeof v !== 'number') finiteF32(v);
    out[i] = v;
    if (!Number.isFinite(out[i])) finiteF32(v); // NaN, infinity, or float32 overflow
  }
  return out;
}
function sameShape(a, b) { return a.length === b.length && a.every((v, i) => v === b[i]); }
/** Contiguous row-major strides, padded on the left to four dimensions. */
function strides4(shape) {
  const padded = pad4(shape), out = [0, 0, 0, 0];
  for (let d = 3, s = 1; d >= 0; d--) { out[d] = s; s *= padded[d]; }
  return out;
}
function pad4(shape) { return [...Array(MAX_RANK - shape.length).fill(1), ...shape]; }
/** NumPy broadcasting, right-aligned. Returns the output shape and per-operand strides (0 = broadcast). */
export function broadcast(aShape, bShape) {
  const rank = Math.max(aShape.length, bShape.length), shape = [];
  for (let i = 0; i < rank; i++) {
    const x = aShape[aShape.length - rank + i] ?? 1, y = bShape[bShape.length - rank + i] ?? 1;
    check(x === y || x === 1 || y === 1, 'SHAPE', `Shapes [${aShape}] and [${bShape}] cannot be broadcast`);
    shape.push(Math.max(x, y));
  }
  const dims = pad4(shape), expand = s => {
    const p = pad4(s), st = strides4(s);
    return st.map((v, d) => (p[d] === 1 && dims[d] !== 1) ? 0 : v);
  };
  return {shape, dims, aStrides: expand(aShape), bStrides: expand(bShape)};
}
/** A permutation as a strided gather: output dims (padded) and the source stride of each. */
function gather(n, inShape) {
  const r = inShape.length, inStrides = strides4(inShape);
  n.dims = pad4(n.shape);
  n.srcStrides = n.dims.map((_, d) => d < MAX_RANK - r ? 0 : inStrides[MAX_RANK - r + n.perm[d - (MAX_RANK - r)]]);
}
function axisOf(raw, rank) {
  check(Number.isSafeInteger(raw) && raw >= -Math.max(rank, 1) && raw < Math.max(rank, 1), 'SHAPE', 'Axis is out of range');
  return raw < 0 ? raw + rank : raw;
}
const OPTIMIZER_FIELDS = {sgd_update: ['lr'], momentum_update: ['momentum', 'dampening'],
  adam_m: ['beta1'], adam_v: ['beta2'], adam_update: ['lr', 'beta1', 'beta2', 'eps', 'step']};
export function validateProgram(program, overrides = {}) {
  keys(overrides, Object.keys(DEFAULT_LIMITS));
  for (const v of Object.values(overrides)) check(Number.isSafeInteger(v) && v > 0,
    'LIMIT', 'Limits must be positive safe integers');
  const limits = {...DEFAULT_LIMITS, ...overrides};
  keys(program, ['version', 'nodes', 'outputs'], ['version', 'nodes', 'outputs']);
  // Versions 1 and 2 share one validator: version 2 names the extended operation
  // set, and every version-1 graph means the same thing under it.
  check(program.version === 1 || program.version === 2, 'PROTOCOL', 'Only graph protocol versions 1 and 2 are supported');
  check(Array.isArray(program.nodes) && program.nodes.length > 0 && program.nodes.length <= limits.maxNodes,
    'LIMIT', 'Invalid graph node count');
  check(Array.isArray(program.outputs) && program.outputs.length > 0 && program.outputs.length <= limits.maxOutputs,
    'LIMIT', 'Invalid output count');
  const nodes = [], root = []; let logicalBytes = 0, work = 0, inputElements = 0;
  for (let id = 0; id < program.nodes.length; id++) {
    check(Object.hasOwn(program.nodes, id), 'PROTOCOL', 'Expected a plain data object');
    const raw = program.nodes[id];
    // Validate op before looking up a kernel; identifiers never become code.
    keys(raw, ['id', 'op', 'shape', 'data', 'value', 'a', 'b', 'c', 'axis', 'keepdim', 'dims',
      'lr', 'momentum', 'dampening', 'beta1', 'beta2', 'eps', 'step'], ['id', 'op']);
    check(raw.id === id, 'PROTOCOL', 'Node IDs must be consecutive integers starting at zero');
    const n = {id, op: raw.op, refs: []};
    const ref = key => {
      const r = raw[key];
      check(Number.isSafeInteger(r) && r >= 0 && r < id, 'REFERENCE', 'References must point to earlier nodes');
      n[key] = r; n.refs.push(r); return nodes[r];
    };
    let units = 0;
    const op = typeof raw.op === 'string' ? raw.op : '';
    if (BINARY_OPS.includes(op)) {
      keys(raw, ['id', 'op', 'a', 'b'], ['a', 'b']);
      const a = ref('a'), b = ref('b'), bc = broadcast(a.shape, b.shape);
      n.shape = bc.shape; n.dims = bc.dims; n.aStrides = bc.aStrides; n.bStrides = bc.bStrides;
      // Fast paths every backend recognises; 'general' uses the padded strides.
      n.aScalar = a.size === 1 && n.shape.length > 0; n.bScalar = b.size === 1 && n.shape.length > 0;
      n.mode = sameShape(a.shape, b.shape) ? 'same' : a.size === 1 && sameShape(b.shape, n.shape) ? 'aScalar'
        : b.size === 1 && sameShape(a.shape, n.shape) ? 'bScalar' : 'general';
    } else if (UNARY_OPS.includes(op)) {
      keys(raw, ['id', 'op', 'a'], ['a']);
      const a = ref('a'); n.shape = [...a.shape];
      units = a.size * (['exp', 'log', 'tanh', 'sigmoid', 'gelu', 'gelu_grad'].includes(op) ? 4 : 1);
    } else if (REDUCE_OPS.includes(op)) {
      keys(raw, ['id', 'op', 'a', 'axis', 'keepdim'], ['a']);
      const a = ref('a');
      check(!Object.hasOwn(raw, 'keepdim') || typeof raw.keepdim === 'boolean', 'PROTOCOL', 'keepdim must be a boolean');
      n.keepdim = raw.keepdim === true; n.inputSize = a.size; n.inputShape = [...a.shape];
      if (Object.hasOwn(raw, 'axis')) {
        const axis = axisOf(raw.axis, a.shape.length); n.axis = axis;
        const dims = a.shape.length ? a.shape : [1];
        n.len = dims[axis]; n.outer = sizeOf(dims.slice(0, axis)); n.inner = sizeOf(dims.slice(axis + 1));
        n.shape = n.keepdim ? a.shape.map((d, i) => i === axis ? 1 : d) : a.shape.filter((_, i) => i !== axis);
      } else {
        n.whole = true; n.shape = n.keepdim ? a.shape.map(() => 1) : [];
      }
      units = a.size * 2;
    } else switch (op) {
      case 'input':
        keys(raw, ['id', 'op', 'shape', 'data'], ['shape', 'data']);
        n.shape = shapeOf(raw.shape, limits);
        // A flat list of numbers, or a Float32Array (a ZIPP engine's binary
        // tensor transport): owned by the plan either way, and checked in
        // one pass for the typed form, whose elements are float32 already.
        check((raw.data instanceof Float32Array || Array.isArray(raw.data)) && raw.data.length === sizeOf(n.shape),
          'SHAPE', 'Flat input length does not match shape');
        inputElements += raw.data.length;
        check(inputElements <= limits.maxInputElements, 'LIMIT', 'Total input exceeds limit');
        n.data = float32Data(raw.data); break;
      case 'full':
        keys(raw, ['id', 'op', 'shape', 'value'], ['shape', 'value']);
        n.shape = shapeOf(raw.shape, limits); n.value = finiteF32(raw.value); break;
      case 'life': case 'transpose': {
        keys(raw, ['id', 'op', 'a'], ['a']); const a = ref('a');
        check(a.shape.length === 2, 'SHAPE', `${op} requires a matrix`);
        n.inputSize = a.size; n.inputShape = [...a.shape];
        if (op === 'life') { n.shape = [...a.shape]; units = a.size * 9; break; }
        n.shape = [a.shape[1], a.shape[0]]; n.perm = [1, 0]; gather(n, a.shape); break;
      }
      case 'permute': {
        keys(raw, ['id', 'op', 'a', 'dims'], ['a', 'dims']); const a = ref('a');
        const perm = intArray(raw.dims, MAX_RANK, 'Permutation');
        check(perm.length === a.shape.length && perm.every(d => d >= 0 && d < perm.length) && new Set(perm).size === perm.length,
          'SHAPE', 'dims must be a permutation of the input axes');
        n.perm = perm; n.shape = perm.map(d => a.shape[d]); n.inputShape = [...a.shape]; gather(n, a.shape); break;
      }
      case 'reshape': {
        keys(raw, ['id', 'op', 'a', 'shape'], ['a', 'shape']); const a = ref('a');
        n.shape = shapeOf(raw.shape, limits);
        check(sizeOf(n.shape) === a.size, 'SHAPE', 'reshape must keep the element count');
        // A reshape of a contiguous tensor is the same storage: no kernel runs.
        n.alias = true; units = 1; break;
      }
      case 'softmax': case 'log_softmax': {
        keys(raw, ['id', 'op', 'a', 'axis'], ['a']); const a = ref('a');
        check(a.shape.length >= 1, 'SHAPE', `${op} requires at least one dimension`);
        check(!Object.hasOwn(raw, 'axis') || axisOf(raw.axis, a.shape.length) === a.shape.length - 1,
          'SHAPE', `${op} supports the last axis only`);
        n.shape = [...a.shape]; n.cols = a.shape[a.shape.length - 1]; n.rows = a.size / n.cols;
        units = a.size * 4; break;
      }
      case 'matmul': {
        keys(raw, ['id', 'op', 'a', 'b'], ['a', 'b']);
        const a = ref('a'), b = ref('b');
        const ra = a.shape.length, rb = b.shape.length;
        check((ra === 2 || ra === 3) && (rb === 2 || rb === 3) && a.shape[ra - 1] === b.shape[rb - 2],
          'SHAPE', 'matmul requires [M,K] @ [K,N] or batched [B,M,K] @ [B,K,N]');
        const ba = ra === 3 ? a.shape[0] : 1, bb = rb === 3 ? b.shape[0] : 1;
        check(ba === bb || ba === 1 || bb === 1, 'SHAPE', 'matmul batch dimensions must match or be 1');
        n.batch = Math.max(ba, bb); n.m = a.shape[ra - 2]; n.k = a.shape[ra - 1]; n.n = b.shape[rb - 1];
        n.aBatchStride = ba === 1 ? 0 : n.m * n.k; n.bBatchStride = bb === 1 ? 0 : n.k * n.n;
        n.shape = ra === 3 || rb === 3 ? [n.batch, n.m, n.n] : [n.m, n.n];
        shapeOf(n.shape, limits); units = 2 * n.batch * n.m * n.k * n.n; break;
      }
      case 'cross_entropy': case 'cross_entropy_grad': {
        keys(raw, ['id', 'op', 'a', 'b'], ['a', 'b']);
        const a = ref('a'), b = ref('b');
        check(a.shape.length === 2 && b.shape.length === 1 && b.shape[0] === a.shape[0],
          'SHAPE', `${op} requires logits [N,C] and class targets [N]`);
        // Targets are data, checked here: every kernel may then index with them.
        check(b.op === 'input' && b.data.every(t => Number.isInteger(t) && t >= 0 && t < a.shape[1]),
          'NUMBER', `${op} targets must be an input of integer class indices in [0, C)`);
        n.rows = a.shape[0]; n.cols = a.shape[1];
        n.shape = op === 'cross_entropy' ? [] : [...a.shape]; units = a.size * 4; break;
      }
      case 'sgd_update': case 'momentum_update': case 'adam_m': case 'adam_v': case 'adam_update': {
        const fields = OPTIMIZER_FIELDS[op], refNames = op === 'adam_update' ? ['a', 'b', 'c'] : ['a', 'b'];
        keys(raw, ['id', 'op', ...refNames, ...fields], [...refNames, ...fields]);
        const inputs = refNames.map(ref);
        check(inputs.every(t => sameShape(t.shape, inputs[0].shape)), 'SHAPE', `${op} requires equal shapes`);
        for (const f of fields) if (f !== 'step') n[f] = finiteF32(raw[f]);
        for (const f of ['beta1', 'beta2']) if (f in n) check(raw[f] >= 0 && raw[f] < 1, 'NUMBER', `${f} must be in [0, 1)`);
        // Derived scalars come from the given doubles and round once, as PyTorch
        // rounds a Python-float expression when it scales a float32 tensor.
        if (op === 'momentum_update') n.w = finiteF32(1 - raw.dampening);
        if (op === 'adam_m') n.w = Math.fround(1 - raw.beta1);
        if (op === 'adam_v') n.w = Math.fround(1 - raw.beta2);
        if (op === 'adam_update') {
          check(Number.isSafeInteger(raw.step) && raw.step >= 1 && raw.step <= 2 ** 31, 'NUMBER', 'step must be a positive integer');
          check(raw.eps >= 0, 'NUMBER', 'eps must be non-negative');
          n.step = raw.step;
          // Bias corrections are host constants, so no backend evaluates pow.
          const bc1 = 1 - Math.pow(raw.beta1, n.step), bc2 = 1 - Math.pow(raw.beta2, n.step);
          n.stepSize = finiteF32(raw.lr / bc1); n.bc2Sqrt = finiteF32(Math.sqrt(bc2));
          check(n.bc2Sqrt > 0, 'NUMBER', 'beta2 bias correction underflows float32');
        }
        n.shape = [...inputs[0].shape]; units = inputs[0].size * 8; break;
      }
      default: throw new ComputeError('OP', `Unsupported operation: ${String(raw.op)}`);
    }
    n.size = sizeOf(n.shape);
    root.push(n.alias ? root[n.a] : id);
    logicalBytes += n.size * 4; work += units || n.size;
    check(logicalBytes <= limits.maxLogicalBytes && work <= limits.maxWork,
      'LIMIT', 'Graph exceeds allocation or work budget');
    nodes.push(Object.freeze(n));
  }
  const names = new Set(); let outputElements = 0;
  const outputs = Array.from({length: program.outputs.length}, (_, i) => {
    check(Object.hasOwn(program.outputs, i), 'PROTOCOL', 'Expected a plain data object');
    const o = program.outputs[i];
    keys(o, ['name', 'id'], ['name', 'id']);
    check(typeof o.name === 'string' && /^[A-Za-z][A-Za-z0-9_]{0,63}$/.test(o.name) &&
      !['__proto__', 'prototype', 'constructor'].includes(o.name), 'PROTOCOL', 'Invalid output name');
    check(!names.has(o.name), 'PROTOCOL', 'Duplicate output name'); names.add(o.name);
    check(Number.isSafeInteger(o.id) && o.id >= 0 && o.id < nodes.length, 'REFERENCE', 'Invalid output reference');
    outputElements += nodes[o.id].size;
    check(outputElements <= limits.maxOutputElements, 'LIMIT', 'Requested readback exceeds limit');
    return Object.freeze({...o});
  });
  // Uses are counted per storage root: an alias (reshape) keeps its source alive
  // through its own consumers and outputs, and never frees anything itself.
  const uses = Array(nodes.length).fill(0);
  for (const n of nodes) if (!n.alias) for (const r of n.refs) uses[root[r]]++;
  for (const o of outputs) uses[root[o.id]]++;
  return {nodes, outputs, uses, root, limits, logicalBytes, work, inputElements, outputElements};
}
