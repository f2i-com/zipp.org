import {check, fields, integer, shapeSize, sameShape} from './common.mjs';
const OPS = new Set(['input', 'full', 'add', 'sub', 'mul', 'div', 'relu', 'positive', 'gelu', 'exp', 'log',
  'neg', 'tanh', 'sigmoid', 'sqrt', 'matmul', 'transpose', 'reshape', 'permute', 'mean', 'sum', 'softmax', 'log_softmax']);

/** Resolve only data bindings. The returned object is still passed through
 * the existing ZIPP graph validator by runtime.execute(). No validation bypass.
 */
export async function bindGraph(template, weights, limits) {
  fields(template, ['version', 'graph', 'bindings'], ['version', 'graph', 'bindings']);
  check(template.version === 1, 'VERSION', 'Unsupported model binding protocol');
  const graph = template.graph;
  fields(graph, ['version', 'nodes', 'outputs'], ['version', 'nodes', 'outputs']);
  check(graph.version === 2, 'VERSION', 'Expected ZIPP Graph v2');
  check(Array.isArray(graph.nodes) && graph.nodes.length > 0 && graph.nodes.length <= limits.maxNodes, 'LIMIT', 'Graph node budget exceeded');
  check(Array.isArray(template.bindings) && template.bindings.length <= graph.nodes.length, 'LIMIT', 'Invalid binding count');
  check(Array.isArray(graph.outputs) && graph.outputs.length === 1, 'FORMAT', 'Model graph must expose exactly one logits output');
  fields(graph.outputs[0], ['name', 'id'], ['name', 'id']);
  check(graph.outputs[0].name === 'logits', 'FORMAT', 'Model output must be named logits');
  integer(graph.outputs[0].id, 0, graph.nodes.length - 1, 'Output node id');
  const byNode = new Map();
  for (const binding of template.bindings) {
    check(binding && typeof binding === 'object', 'FORMAT', 'Invalid binding');
    integer(binding.node, 0, graph.nodes.length - 1, 'Binding node');
    check(!byNode.has(binding.node), 'FORMAT', 'Duplicate input binding'); byNode.set(binding.node, binding);
  }
  let inputBytes = 0;
  const nodes = graph.nodes.map((raw, id) => {
    check(raw && typeof raw === 'object' && raw.id === id && OPS.has(raw.op), 'GRAPH', 'Unknown op or nonconsecutive graph id');
    const node = {...raw};
    if (Object.hasOwn(node, 'shape')) shapeSize(node.shape, limits);
    if (node.op === 'input') {
      fields(node, ['id', 'op', 'shape', 'data'], ['id', 'op', 'shape']);
      inputBytes += shapeSize(node.shape, limits) * 4;
      check(inputBytes <= limits.maxBoundInputBytes, 'LIMIT', 'Bound graph inputs exceed budget');
      check(byNode.has(id) !== Object.hasOwn(node, 'data'), 'FORMAT', 'Input must have exactly one data source');
      if (Object.hasOwn(node, 'data')) check(Array.isArray(node.data) && node.data.length === shapeSize(node.shape, limits), 'SHAPE', 'Literal input size mismatch');
    } else check(!byNode.has(id), 'FORMAT', 'Only input nodes may bind assets');
    return node;
  });
  // Preflight every binding before a potentially large weight read/allocation.
  for (const [id, b] of byNode) {
    const node = nodes[id]; check(node.op === 'input', 'FORMAT', 'Binding target must be an input');
    if (b.kind === 'tensor') {
      fields(b, ['node', 'kind', 'tensor', 'transpose'], ['node', 'kind', 'tensor']);
      const shape = weights.info(b.tensor).shape;
      // A checkpoint stores a linear layer as [out, in]; this graph protocol
      // multiplies [tokens, in] by [in, out]. Transposing on the way in lets a
      // plugin read the checkpoint's own tensors instead of requiring a
      // rewritten copy on disk. It is a relabelling of a matrix, not a licence
      // to reinterpret a tensor: rank two only, and the shape must still match.
      if (Object.hasOwn(b, 'transpose')) {
        check(b.transpose === true, 'FORMAT', 'transpose is true when present');
        check(shape.length === 2, 'SHAPE', `Only a matrix can be transposed: ${b.tensor}`);
      }
      check(sameShape(b.transpose ? [shape[1], shape[0]] : shape, node.shape), 'SHAPE', `Weight shape mismatch: ${b.tensor}`);
    } else if (b.kind === 'rows') {
      fields(b, ['node', 'kind', 'tensor', 'indices'], ['node', 'kind', 'tensor', 'indices']);
      const info = weights.info(b.tensor);
      check(info.shape.length === 2 && Array.isArray(b.indices) && b.indices.length > 0 && b.indices.length <= limits.maxContext,
        'SHAPE', 'Row gathering requires a bounded index list and matrix');
      for (const index of b.indices) integer(index, 0, info.shape[0] - 1, 'Embedding index');
      check(sameShape(node.shape, [b.indices.length, info.shape[1]]), 'SHAPE', 'Embedding shape mismatch');
    } else if (b.kind === 'causal') {
      fields(b, ['node', 'kind', 'length', 'window'], ['node', 'kind', 'length']);
      integer(b.length, 1, limits.maxContext, 'Attention length');
      if (b.window !== undefined) integer(b.window, 1, limits.maxContext, 'Local attention window');
      check(sameShape(node.shape, [b.length, b.length]), 'SHAPE', 'Attention mask shape mismatch');
    } else check(false, 'FORMAT', 'Unknown model asset binding');
  }
  for (const [id, b] of byNode) {
    if (b.kind === 'tensor') {
      const data = await weights.tensor(b.tensor);
      if (!b.transpose) nodes[id].data = data;
      else {
        // A fresh array: the store's copy is shared with every other binding.
        const [rows, columns] = weights.info(b.tensor).shape, out = new Float32Array(data.length);
        for (let row = 0; row < rows; row++) {
          for (let column = 0; column < columns; column++) out[column * rows + row] = data[row * columns + column];
        }
        nodes[id].data = out;
      }
    }
    else if (b.kind === 'rows') {
      const info = weights.info(b.tensor), width = info.shape[1];
      // A store that can read rows does; otherwise the whole tensor is decoded
      // once and sliced, which is what a Safetensors file wants anyway.
      const gathered = typeof weights.rows === 'function' ? await weights.rows(b.tensor, b.indices) : null;
      if (gathered) { nodes[id].data = gathered; }
      else {
        const data = await weights.tensor(b.tensor);
        const rows = new Float32Array(b.indices.length * width);
        b.indices.forEach((index, i) => rows.set(data.subarray(index * width, (index + 1) * width), i * width));
        nodes[id].data = rows;
      }
    } else {
      const data = new Float32Array(b.length * b.length);
      for (let row = 0; row < b.length; row++) for (let col = 0; col < b.length; col++) {
        // Graph v2 requires finite values. -1e9 is deliberate, not JSON -Infinity.
        if (col > row || (b.window !== undefined && col <= row - b.window)) data[row * b.length + col] = -1e9;
      }
      nodes[id].data = data;
    }
  }
  return {version: 2, nodes, outputs: graph.outputs.map(o => ({...o}))};
}
