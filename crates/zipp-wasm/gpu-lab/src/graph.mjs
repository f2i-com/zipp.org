/** Strict, versioned data-only compute protocol. No shader source or eval. */
export class ComputeError extends Error {
  constructor(code, message) { super(message); this.name = 'ComputeError'; this.code = code; }
}
export function check(ok, code, message) { if (!ok) throw new ComputeError(code, message); }
export const DEFAULT_LIMITS = Object.freeze({
  maxNodes: 512, maxElements: 1048576, maxInputElements: 1048576,
  maxOutputElements: 1048576, maxLogicalBytes: 32 * 1024 * 1024,
  maxWork: 100000000, maxDimension: 4096, maxOutputs: 16,
  maxWebGLTextureBytes: 128 * 1024 * 1024,
});
export function sizeOf(shape) { return shape.reduce((a, b) => a * b, 1); }
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
function shapeOf(shape, limits) {
  check(Array.isArray(shape) && shape.length <= 2, 'SHAPE', 'Only scalar, vector and matrix shapes are supported');
  check(shape.every(d => Number.isSafeInteger(d) && d > 0 && d <= limits.maxDimension),
    'SHAPE', 'Shape dimensions must be positive bounded integers');
  check(sizeOf(shape) <= limits.maxElements, 'LIMIT', 'Tensor exceeds element limit');
  return [...shape];
}
function finiteF32(v) {
  check(typeof v === 'number' && Number.isFinite(v) && Number.isFinite(Math.fround(v)),
    'NUMBER', 'Values must be finite float32 numbers');
  return Math.fround(v);
}
function sameShape(a, b) { return a.length === b.length && a.every((v, i) => v === b[i]); }
export function validateProgram(program, overrides = {}) {
  keys(overrides, Object.keys(DEFAULT_LIMITS));
  for (const v of Object.values(overrides)) check(Number.isSafeInteger(v) && v > 0,
    'LIMIT', 'Limits must be positive safe integers');
  const limits = {...DEFAULT_LIMITS, ...overrides};
  keys(program, ['version', 'nodes', 'outputs'], ['version', 'nodes', 'outputs']);
  check(program.version === 1, 'PROTOCOL', 'Only graph protocol version 1 is supported');
  check(Array.isArray(program.nodes) && program.nodes.length > 0 && program.nodes.length <= limits.maxNodes,
    'LIMIT', 'Invalid graph node count');
  check(Array.isArray(program.outputs) && program.outputs.length > 0 && program.outputs.length <= limits.maxOutputs,
    'LIMIT', 'Invalid output count');
  const nodes = []; let logicalBytes = 0, work = 0, inputElements = 0;
  for (let id = 0; id < program.nodes.length; id++) {
    const raw = program.nodes[id];
    // Validate op before looking up a kernel; identifiers never become code.
    keys(raw, ['id', 'op', 'shape', 'data', 'value', 'a', 'b'], ['id', 'op']);
    check(raw.id === id, 'PROTOCOL', 'Node IDs must be consecutive integers starting at zero');
    const n = {id, op: raw.op, refs: []};
    const ref = key => {
      const r = raw[key];
      check(Number.isSafeInteger(r) && r >= 0 && r < id, 'REFERENCE', 'References must point to earlier nodes');
      n[key] = r; n.refs.push(r); return nodes[r];
    };
    let units = 0;
    switch (raw.op) {
      case 'input':
        keys(raw, ['id', 'op', 'shape', 'data'], ['shape', 'data']);
        n.shape = shapeOf(raw.shape, limits);
        check(Array.isArray(raw.data) && raw.data.length === sizeOf(n.shape), 'SHAPE', 'Flat input length does not match shape');
        inputElements += raw.data.length;
        check(inputElements <= limits.maxInputElements, 'LIMIT', 'Total input exceeds limit');
        n.data = Float32Array.from(raw.data, finiteF32); break;
      case 'full':
        keys(raw, ['id', 'op', 'shape', 'value'], ['shape', 'value']);
        n.shape = shapeOf(raw.shape, limits); n.value = finiteF32(raw.value); break;
      case 'add': case 'sub': case 'mul': {
        keys(raw, ['id', 'op', 'a', 'b'], ['a', 'b']);
        const a = ref('a'), b = ref('b');
        check(sameShape(a.shape, b.shape) || a.shape.length === 0 || b.shape.length === 0,
          'SHAPE', 'Binary ops support equal shapes or a scalar only');
        n.shape = [...(a.shape.length === 0 ? b.shape : a.shape)];
        n.aScalar = a.shape.length === 0; n.bScalar = b.shape.length === 0; break;
      }
      case 'relu': case 'sum': case 'life': {
        keys(raw, ['id', 'op', 'a'], ['a']); const a = ref('a');
        check(raw.op !== 'life' || a.shape.length === 2, 'SHAPE', 'life requires a matrix');
        n.shape = raw.op === 'sum' ? [] : [...a.shape];
        n.inputSize = a.size; n.inputShape = [...a.shape];
        units = a.size * (raw.op === 'life' ? 9 : raw.op === 'sum' ? 2 : 1); break;
      }
      case 'matmul': {
        keys(raw, ['id', 'op', 'a', 'b'], ['a', 'b']);
        const a = ref('a'), b = ref('b');
        check(a.shape.length === 2 && b.shape.length === 2 && a.shape[1] === b.shape[0],
          'SHAPE', 'matmul requires [M,K] @ [K,N]');
        n.shape = [a.shape[0], b.shape[1]]; n.m = a.shape[0]; n.k = a.shape[1]; n.n = b.shape[1];
        shapeOf(n.shape, limits); units = 2 * n.m * n.k * n.n; break;
      }
      default: throw new ComputeError('OP', `Unsupported operation: ${String(raw.op)}`);
    }
    n.size = sizeOf(n.shape);
    logicalBytes += n.size * 4; work += units || n.size;
    check(logicalBytes <= limits.maxLogicalBytes && work <= limits.maxWork,
      'LIMIT', 'Graph exceeds allocation or work budget');
    nodes.push(Object.freeze(n));
  }
  const names = new Set(); let outputElements = 0;
  const outputs = program.outputs.map(o => {
    keys(o, ['name', 'id'], ['name', 'id']);
    check(typeof o.name === 'string' && /^[A-Za-z][A-Za-z0-9_]{0,63}$/.test(o.name) &&
      !['__proto__', 'prototype', 'constructor'].includes(o.name), 'PROTOCOL', 'Invalid output name');
    check(!names.has(o.name), 'PROTOCOL', 'Duplicate output name'); names.add(o.name);
    check(Number.isSafeInteger(o.id) && o.id >= 0 && o.id < nodes.length, 'REFERENCE', 'Invalid output reference');
    outputElements += nodes[o.id].size;
    check(outputElements <= limits.maxOutputElements, 'LIMIT', 'Requested readback exceeds limit');
    return Object.freeze({...o});
  });
  const uses = Array(nodes.length).fill(0);
  for (const n of nodes) for (const r of n.refs) uses[r]++;
  for (const o of outputs) uses[o.id]++;
  return {nodes, outputs, uses, limits, logicalBytes, work, inputElements, outputElements};
}
