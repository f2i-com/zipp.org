/**
 * Graph IR v2 fixtures shared by the Node tests and the real-browser harness:
 * one case per operation (with broadcasting and gradient edge cases) and a
 * generator for a whole MLP classification training step.
 */
export function seeded(seed = 1234567) {
  return (lo = -2, hi = 2) => { seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0; return lo + seed / 4294967296 * (hi - lo); };
}
export function builder() {
  const nodes = [];
  const add = (op, fields) => { nodes.push({id: nodes.length, op, ...fields}); return nodes.length - 1; };
  const size = shape => shape.reduce((x, y) => x * y, 1);
  return {
    nodes,
    input: (data, shape = [data.length]) => add('input', {shape, data: Array.from(data)}),
    random: (shape, rnd, lo, hi) => add('input', {shape, data: Array.from({length: size(shape)}, () => rnd(lo, hi))}),
    full: (shape, value) => add('full', {shape, value}),
    op: (op, a, b, fields = {}) => add(op, b === undefined ? {a, ...fields} : {a, b, ...fields}),
    node: (op, fields) => add(op, fields),
    program: outputs => ({version: 2, nodes, outputs: Object.entries(outputs).map(([name, id]) => ({name, id}))}),
  };
}
const UNARY = ['relu', 'positive', 'neg', 'exp', 'log', 'sqrt', 'tanh', 'sigmoid', 'gelu', 'gelu_grad'];
/** [name, program] pairs; each program's outputs are compared element by element. */
export function opCases() {
  const cases = [], rnd = seeded(97);
  const one = (name, build) => { const g = builder(); cases.push([name, g.program(build(g))]); };
  const broadcasts = [[[3], [3]], [[4, 3], [3]], [[4, 1], [1, 5]], [[2, 1, 3], [4, 1]], [[], [2, 3]], [[1], [5]],
    [[2, 3, 4, 5], [3, 1, 5]], [[1, 1, 1, 7], [6, 1, 1, 1]], [[65], [1]], [[3, 129], [129]]];
  for (const op of ['add', 'sub', 'mul', 'div']) for (const [sa, sb] of broadcasts)
    one(`${op} [${sa}] with [${sb}]`, g => {
      const a = g.random(sa, rnd), b = op === 'div' ? g.random(sb, rnd, 0.5, 3) : g.random(sb, rnd);
      return {result: g.op(op, a, b), reversed: g.op(op, b, a)};
    });
  for (const op of UNARY) for (const len of [1, 7, 64, 65, 1025]) one(`${op} length ${len}`, g => {
    const lo = op === 'log' || op === 'sqrt' ? 0.01 : -6, hi = 6;
    const data = Array.from({length: len}, () => rnd(lo, hi));
    if (len > 1 && lo < 0) data[0] = 0;
    return {result: g.op(op, g.input(data))};
  });
  one('exact-zero and tiny arguments of odd functions', g => {
    const x = g.input([0, 1e-9, -1e-6, 0.0049, -0.0099, 0.3, 0.49, 0.51, -0.5]);
    return {tanh: g.op('tanh', x), gelu: g.op('gelu', x), grad: g.op('gelu_grad', x), sigmoid: g.op('sigmoid', x)};
  });
  for (const [shape, axis] of [[[5], 0], [[4, 6], 0], [[4, 6], 1], [[4, 6], -1], [[2, 3, 4], 1], [[2, 3, 4, 5], 2], [[3, 1025], 1], [[1025, 3], 0]])
    for (const op of ['sum', 'mean']) one(`${op} over axis ${axis} of [${shape}]`, g => {
      const a = g.random(shape, rnd);
      return {result: g.op(op, a, undefined, {axis}), keep: g.op(op, a, undefined, {axis, keepdim: true})};
    });
  for (const shape of [[1], [7], [65], [3, 4], [2, 3, 4]]) one(`whole sum/mean of [${shape}]`, g => {
    const a = g.random(shape, rnd);
    return {sum: g.op('sum', a), mean: g.op('mean', a), keep: g.op('mean', a, undefined, {keepdim: true})};
  });
  for (const shape of [[1], [10], [4, 10], [3, 1], [2, 3, 7], [5, 257]]) one(`softmax/log_softmax [${shape}]`, g => {
    const a = g.random(shape, rnd, -20, 20);
    return {softmax: g.op('softmax', a), log: g.op('log_softmax', a, undefined, {axis: -1})};
  });
  for (const [shape, dims] of [[[2, 3], [1, 0]], [[2, 3, 4], [2, 0, 1]], [[2, 3, 4, 5], [3, 1, 0, 2]], [[7], [0]], [[1, 65], [1, 0]]])
    one(`permute [${shape}] by [${dims}]`, g => ({result: g.op('permute', g.random(shape, rnd), undefined, {dims})}));
  one('transpose and reshape share or copy storage correctly', g => {
    const a = g.random([4, 6], rnd), r = g.node('reshape', {a, shape: [2, 3, 4]}), rr = g.node('reshape', {a: r, shape: [24]});
    return {source: a, view: r, flat: rr, t: g.op('transpose', a), after: g.op('relu', rr)};
  });
  for (const [sa, sb] of [[[1, 1], [1, 1]], [[3, 5], [5, 7]], [[17, 13], [13, 9]], [[64, 33], [33, 20]], [[2, 3, 4], [2, 4, 5]], [[3, 4, 6], [6, 2]], [[5, 7], [3, 7, 4]], [[1, 3, 9], [4, 9, 2]]])
    one(`matmul [${sa}] @ [${sb}]`, g => ({result: g.op('matmul', g.random(sa, rnd), g.random(sb, rnd))}));
  for (const [rows, cols] of [[1, 1], [1, 3], [5, 10], [64, 10], [3, 257]]) one(`cross-entropy ${rows}x${cols}`, g => {
    const logits = g.random([rows, cols], rnd, -8, 8), targets = g.input(Array.from({length: rows}, (_, i) => (i * 7 + 3) % cols));
    return {loss: g.op('cross_entropy', logits, targets), grad: g.op('cross_entropy_grad', logits, targets)};
  });
  one('optimizer updates (SGD, momentum, Adam)', g => {
    const p = g.random([5, 7], rnd), grad = g.random([5, 7], rnd), buf = g.random([5, 7], rnd), m = g.random([5, 7], rnd), v = g.random([5, 7], rnd, 0, 2);
    const m1 = g.op('adam_m', m, grad, {beta1: 0.9}), v1 = g.op('adam_v', v, grad, {beta2: 0.999});
    return {sgd: g.op('sgd_update', p, grad, {lr: 0.05}), momentum: g.op('momentum_update', buf, grad, {momentum: 0.9, dampening: 0.1}),
      m: m1, v: v1, adam: g.node('adam_update', {a: p, b: m1, c: v1, lr: 0.001, beta1: 0.9, beta2: 0.999, eps: 1e-8, step: 3}),
      lerpHigh: g.op('adam_m', m, grad, {beta1: 0.25})};
  });
  return cases;
}

/**
 * One MLP classification training step: forward, mean cross-entropy, backward
 * and an optimizer update of every parameter, all in one graph. Parameters use
 * x @ W + b with W as [in, out]. Returns the program and its output names.
 */
export function mlpTrainingStep({sizes = [784, 256, 10], batch = 64, seed = 5, optimizer = 'adam', step = 1, lr = 0.001,
  activation = 'relu', params = null, state = null, x = null, targets = null} = {}) {
  const g = builder(), rnd = seeded(seed), layers = sizes.length - 1;
  const xin = g.node('input', {shape: [batch, sizes[0]], data: x ?? Array.from({length: batch * sizes[0]}, () => rnd(0, 1))});
  const y = g.input(targets ?? Array.from({length: batch}, (_, i) => (i * 7 + seed) % sizes[layers]));
  const W = [], B = [];
  for (let l = 0; l < layers; l++) {
    const scale = Math.sqrt(2 / sizes[l]);
    W.push(g.node('input', {shape: [sizes[l], sizes[l + 1]], data: params?.[2 * l] ?? Array.from({length: sizes[l] * sizes[l + 1]}, () => rnd(-scale, scale))}));
    B.push(g.node('input', {shape: [sizes[l + 1]], data: params?.[2 * l + 1] ?? Array.from({length: sizes[l + 1]}, () => rnd(-0.1, 0.1))}));
  }
  const pre = [], act = [xin];
  for (let l = 0; l < layers; l++) {
    const z = g.op('add', g.op('matmul', act[l], W[l]), B[l]); pre.push(z);
    act.push(l < layers - 1 ? g.op(activation, z) : z);
  }
  const logits = act[layers], loss = g.op('cross_entropy', logits, y);
  let delta = g.op('cross_entropy_grad', logits, y);
  const grads = [];
  for (let l = layers - 1; l >= 0; l--) {
    const dW = g.op('matmul', g.op('transpose', act[l]), delta), dB = g.op('sum', delta, undefined, {axis: 0});
    grads[2 * l] = dW; grads[2 * l + 1] = dB;
    if (l > 0) {
      const back = g.op('matmul', delta, g.op('transpose', W[l]));
      const local = activation === 'relu' ? g.op('positive', pre[l - 1]) : activation === 'gelu' ? g.op('gelu_grad', pre[l - 1]) : null;
      if (local !== null) delta = g.op('mul', back, local);
      else if (activation === 'tanh') { const t = act[l]; delta = g.op('mul', back, g.op('sub', g.full([], 1), g.op('mul', t, t))); }
      else { const s = act[l]; delta = g.op('mul', back, g.op('mul', s, g.op('sub', g.full([], 1), s))); }
    }
  }
  const outputs = {loss}, weights = W.flatMap((w, l) => [w, B[l]]);
  weights.forEach((p, i) => {
    const shape = g.nodes[p].shape, zeros = () => g.full(shape, 0), s = state?.[i];
    if (optimizer === 'sgd') { outputs[`p${i}`] = g.op('sgd_update', p, grads[i], {lr}); return; }
    if (optimizer === 'momentum') {
      const buf = s ? g.input(s[0], shape) : null;
      const next = buf === null ? grads[i] : g.op('momentum_update', buf, grads[i], {momentum: 0.9, dampening: 0});
      outputs[`p${i}`] = g.op('sgd_update', p, next, {lr}); outputs[`buf${i}`] = next; return;
    }
    const m = g.op('adam_m', s ? g.input(s[0], shape) : zeros(), grads[i], {beta1: 0.9});
    const v = g.op('adam_v', s ? g.input(s[1], shape) : zeros(), grads[i], {beta2: 0.999});
    outputs[`p${i}`] = g.node('adam_update', {a: p, b: m, c: v, lr, beta1: 0.9, beta2: 0.999, eps: 1e-8, step});
    outputs[`m${i}`] = m; outputs[`v${i}`] = v;
  });
  return {program: g.program(outputs), parameters: weights.length};
}

/**
 * The training step as a prepared session: parameters and Adam moments are
 * carried inputs (their data is the initial value), x (node 0) and the class
 * targets (node 1) are fed per step. `resident` names every carried output.
 * The seeded parameter draw matches `mlpTrainingStep({sizes, batch, seed, x,
 * targets})`, so chained executes fed the same batches are its reference.
 */
export function mlpSessionProgram({sizes = [784, 256, 10], batch = 64, seed = 5, lr = 0.002} = {}) {
  const zeros = shape => new Array(shape.reduce((x, y) => x * y, 1)).fill(0);
  const shapes = []; for (let l = 0; l < sizes.length - 1; l++) shapes.push([sizes[l], sizes[l + 1]], [sizes[l + 1]]);
  const state = shapes.map(s => [zeros(s), zeros(s)]);
  const {program, parameters} = mlpTrainingStep({sizes, batch, seed, lr, state, step: 1, x: zeros([batch, sizes[0]]), targets: zeros([batch])});
  const nodes = program.nodes.map(n => ({...n})), byName = new Map(program.outputs.map(o => [o.name, o.id]));
  delete nodes[0].data; delete nodes[1].data;
  const resident = [];
  for (let i = 0; i < parameters; i++) for (const k of ['p', 'm', 'v']) {
    nodes[nodes[byName.get(`${k}${i}`)].a].carry = `${k}${i}`; resident.push(`${k}${i}`);
  }
  return {program: {...program, nodes}, parameters, state, resident, sizes, batch, seed, lr};
}
