"""Data-only Graph v2 builder for Qwen3. No torch, host JS, I/O or weight decoding.

What this needs that the GPT-Neo builder did not:

  * **Weights that stay quantized.** `matrix` binds a checkpoint's own
    `[out, in]` matrix and lets the host keep it in the file's block format
    where a backend can read it. The matmul is `transposed` either way, because
    blocks run along the last axis and cannot be transposed without decoding
    them -- which is the thing being avoided.
  * **RMSNorm**, not LayerNorm: no mean subtraction and no bias.
  * **Rotary position embeddings.** The angles depend only on position and
    index, never on the data, so the host precomputes cosines and sines and
    binds them as ordinary inputs. What is left is `x*cos + rotate_half(x)*sin`,
    and `rotate_half` is a fixed 2x2 mix once the head dimension is split in
    two -- a reshape, a permute and a matmul against a shared constant, rather
    than a new kernel.
  * **Grouped-query attention**: fewer key/value heads than query heads, so each
    key head is repeated. Broadcasting a multiply against a row of ones does it
    without a repeat operation.
  * **SwiGLU**, which is `down(silu(gate(x)) * up(x))` and needs only `sigmoid`.
"""
import math


class Graph:
    def __init__(self):
        self.nodes = []
        self.bindings = []
        self.weights = {}
        self.constants = {}
        self.literals = {}
        self.carried = []

    def op(self, name, **attrs):
        index = len(self.nodes)
        node = {"id": index, "op": name}
        node.update(attrs)
        self.nodes.append(node)
        return index

    def scalar(self, value):
        # Shared: a 28-layer model repeats the same epsilon and the same
        # attention scale in every block, and the node budget is a real limit.
        value = float(value)
        if value not in self.constants:
            self.constants[value] = self.op("full", shape=[], value=value)
        return self.constants[value]

    def literal(self, key, shape, data):
        """A constant the host does not own -- a rotation table, a selector."""
        if key not in self.literals:
            self.literals[key] = self.op("input", shape=shape, data=data)
        return self.literals[key]

    def vector(self, name, width):
        """A one-dimensional weight: a norm's scale. Always float32; these are
        F32 in the checkpoint and far too small to be worth blocking."""
        return self.weight(name, [width])

    def weight(self, name, shape):
        key = (name, tuple(shape), "tensor")
        if key not in self.weights:
            index = self.op("input", shape=shape)
            self.bindings.append({"node": index, "kind": "tensor", "tensor": name})
            self.weights[key] = index
        return self.weights[key]

    def matrix(self, name, outs, ins):
        """A weight matrix in the checkpoint's own [out, in] layout.

        The host keeps it as blocks where a backend can decode them and expands
        it where one cannot; either way it is multiplied transposed, so this
        graph does not change when the answer does."""
        key = (name, (outs, ins), "matrix")
        if key not in self.weights:
            index = self.op("input", shape=[outs, ins])
            self.bindings.append({"node": index, "kind": "matrix", "tensor": name})
            self.weights[key] = index
        return self.weights[key]

    def rows(self, name, indices, width):
        index = self.op("input", shape=[len(indices), width])
        self.bindings.append({"node": index, "kind": "rows", "tensor": name,
                              "indices": list(indices)})
        return index

    def feed(self, name, shape):
        """Data the caller supplies rather than the checkpoint.

        The prefill seam: a stage that does not start at layer 0 is handed the
        residual stream for the whole prompt, [length, hidden_size], instead of
        looking the tokens up."""
        node = self.op("input", shape=list(shape))
        self.bindings.append({"node": node, "kind": "feed", "name": name})
        return node

    def causal_mask(self, length):
        index = self.op("input", shape=[length, length])
        self.bindings.append({"node": index, "kind": "causal", "length": length})
        return index

    def one_hot_row(self, length, position):
        """A [1, length] selector. The protocol has no slice, and a 151,936-wide
        vocabulary makes logits for every position far larger than the one row
        anybody samples."""
        data = [0.0]*length
        data[position] = 1.0
        return self.literal(("one_hot", length, position), [1, length], data)

    def linear(self, x, name, ins, outs):
        """[tokens, ins] @ [outs, ins]^T -- the checkpoint's own layout."""
        return self.op("matmul", a=x, b=self.matrix(name, outs, ins), transposed=True)

    def rms_norm(self, x, name, width, epsilon):
        """x * rsqrt(mean(x^2)) * weight, over the last axis.

        No mean subtraction and no bias: that is the whole difference from
        LayerNorm, and it is why a Qwen checkpoint has one tensor per norm."""
        squares = self.op("mul", a=x, b=x)
        mean = self.op("mean", a=squares, axis=-1, keepdim=True)
        denominator = self.op("sqrt", a=self.op("add", a=mean, b=self.scalar(epsilon)))
        return self.op("mul", a=self.op("div", a=x, b=denominator),
                       b=self.vector(name, width))

    def silu(self, x):
        """x * sigmoid(x), the activation Qwen's gated MLP uses."""
        return self.op("mul", a=x, b=self.op("sigmoid", a=x))

    def rope(self, x, length, heads, dim, cos, sin):
        """Rotary embeddings, NeoX style: the head is split in half and the two
        halves are rotated into each other.

        `cos` and `sin` are [length, 1, dim] inputs the host computed, so no
        trigonometry happens here. `rotate_half` -- negate the second half and
        put it first -- is a permutation with a sign, which becomes a 2x2
        matrix multiply once the head dimension is viewed as [2, dim/2].

        Six operations, which is the right shape for a prompt: the alternative
        below costs a matrix per position, and a prompt has many. One token has
        one position, and `rope_once` is that case."""
        half = dim // 2
        # [L, H, dim] -> [L, H, 2, half] -> [L, H, half, 2] -> [L*H*half, 2]
        pairs = self.op("reshape", a=x, shape=[length, heads, 2, half])
        pairs = self.op("permute", a=pairs, dims=[0, 1, 3, 2])
        flat = self.op("reshape", a=pairs, shape=[length*heads*half, 2])
        # [[0, 1], [-1, 0]]: out0 = -x1, out1 = x0.
        swap = self.literal("rotate_half", [2, 2], [0.0, 1.0, -1.0, 0.0])
        turned = self.op("matmul", a=flat, b=swap)
        turned = self.op("reshape", a=turned, shape=[length, heads, half, 2])
        turned = self.op("permute", a=turned, dims=[0, 1, 3, 2])
        turned = self.op("reshape", a=turned, shape=[length, heads, dim])
        return self.op("add", a=self.op("mul", a=x, b=cos),
                       b=self.op("mul", a=turned, b=sin))

    def rope_once(self, x, heads, dim, rotation):
        """The same rotation for one token, as a single multiply.

        `x*cos + rotate_half(x)*sin` is a linear map of the head, and every
        term in it depends only on the position and the index -- so the host
        can hand over the map itself, a [dim, dim] matrix with two non-zeros a
        column, and this becomes one operation instead of six.

        The arithmetic is larger and the wall clock is smaller. A decode step
        is twenty-eight layers deep and spends more time telling a GPU what to
        do than doing it, so six dispatches saved fifty-six times a step is
        worth a matrix multiply that a GPU finishes in nanoseconds. The same
        trade is a bad one for a prompt, where the matrix would be per
        position and the JSON carrying it would dwarf the graph."""
        flat = self.op("reshape", a=x, shape=[heads, dim])
        turned = self.op("matmul", a=flat, b=rotation)
        return self.op("reshape", a=turned, shape=[1, heads, dim])

    def repeat_heads(self, x, length, heads, times, dim):
        """Grouped-query attention: each key/value head serves `times` query
        heads. Broadcasting against a row of ones repeats it without a repeat
        operation, which this protocol does not have."""
        if times == 1:
            return x
        wide = self.op("mul", a=self.op("reshape", a=x, shape=[length, heads, 1, dim]),
                       b=self.op("full", shape=[1, 1, times, 1], value=1.0))
        return self.op("reshape", a=wide, shape=[length, heads*times, dim])

    # ---- Cached decoding: one token at a time, the caches on the device ----

    def step_rows(self, tensor, index, width):
        """One gathered row the host supplies per token, by token id or position."""
        node = self.op("input", shape=[1, width])
        self.bindings.append({"node": node, "kind": "step", "slot": "rows",
                              "tensor": tensor, "index": index})
        return node

    def step_mask(self, context):
        """The additive mask over the positions written so far."""
        node = self.op("input", shape=[1, context])
        self.bindings.append({"node": node, "kind": "step", "slot": "mask"})
        return node

    def step_write(self, context):
        """A one-hot column marking where this token writes into the caches.

        The protocol has no scatter, so writing is arithmetic: the old cache
        times (1 - write), plus write times the new row."""
        node = self.op("input", shape=[context, 1])
        self.bindings.append({"node": node, "kind": "step", "slot": "write"})
        return node

    def step_rope(self, part, dim, base):
        """The cosines or sines of the current position.

        A prefill knows every position when it is built and can carry these as
        constants; a decode graph is built once and run at every position, so
        the host computes them per step. They are trigonometry over a position
        and an index, never over the data."""
        # A matrix is the whole rotation; a table is one of its two halves.
        shape = [dim, dim] if part == "matrix" else [1, 1, dim]
        node = self.op("input", shape=shape)
        self.bindings.append({"node": node, "kind": "step", "slot": "rope",
                              "part": part, "dim": dim, "base": float(base)})
        return node

    def step_hidden(self, width):
        """The residual stream, from whoever ran the layers before this stage.

        This is the seam. It is one `[1, width]` vector -- 4 KB where the
        weights it is travelling between are hundreds of megabytes -- and it is
        the residual rather than anything normalised, because every block
        normalises its own input. A stage can therefore be handed this and
        carry on as though it had computed it."""
        node = self.op("input", shape=[1, width])
        self.bindings.append({"node": node, "kind": "step", "slot": "hidden"})
        return node

    def cache(self, name, shape):
        """A device-resident tensor carried from one token to the next. It never
        crosses the host boundary; the host only ever sees the logits."""
        node = self.op("input", shape=shape, carry=name)
        self.bindings.append({"node": node, "kind": "zeros"})
        return node

    def write_cache(self, name, cache, keep, write, value):
        """cache * (1 - write) + write @ value, and register the result to carry."""
        updated = self.op("add", a=self.op("mul", a=cache, b=keep),
                          b=self.op("matmul", a=write, b=value))
        self.carried.append({"name": name, "id": updated})
        return updated

    def finish(self, logits, stage=None, manifest=None):
        return self._template("logits", logits, stage, manifest)

    def finish_hidden(self, hidden, stage=None, manifest=None):
        """A prefill stage that stops before the end of the model: its output
        is the residual stream for whoever runs the rest."""
        return self._template("hidden", hidden, stage, manifest)

    def _template(self, name, node, stage, manifest=None):
        template = {"version": 1,
                    "graph": {"version": 2, "nodes": self.nodes,
                              "outputs": [{"name": name, "id": node}]},
                    "bindings": self.bindings}
        if stage is not None:
            template["stage"] = {"first_layer": stage[0], "last_layer": stage[1]}
        if manifest is not None:
            template["manifest"] = manifest
        return template

    def finish_decode(self, logits, context, stage=None, manifest=None):
        outputs = [{"name": "logits", "id": logits}]
        outputs.extend({"name": entry["name"], "id": entry["id"]} for entry in self.carried)
        return self._decode_template(outputs, context, stage, manifest)

    def finish_stage(self, hidden, context, width, stage=None, manifest=None):
        """A stage that stops before the end of the model.

        Its output is the residual stream rather than logits, under the name
        the next stage's `step_hidden` binding expects to be given."""
        outputs = [{"name": "hidden", "id": hidden}]
        outputs.extend({"name": entry["name"], "id": entry["id"]} for entry in self.carried)
        template = self._decode_template(outputs, context, stage, manifest)
        template["hidden_size"] = width
        return template

    def _decode_template(self, outputs, context, stage, manifest=None):
        template = {"version": 1, "kind": "decode", "context": context,
                    "graph": {"version": 2, "nodes": self.nodes, "outputs": outputs},
                    "bindings": self.bindings}
        if stage is not None:
            # Which layers these are, so a host can say what it is holding
            # without re-deriving it from the tensor names.
            template["stage"] = {"first_layer": stage[0], "last_layer": stage[1]}
        if manifest is not None:
            template["manifest"] = manifest
        return template


def rope_tables(length, dim, base, offset=0):
    """The cosines and sines for positions [offset, offset+length), as flat
    [length, 1, dim] data. Duplicated across the two halves, which is what the
    NeoX rotation expects."""
    half = dim // 2
    cos, sin = [], []
    for position in range(offset, offset + length):
        for index in range(dim):
            angle = position / (base ** ((2.0 * (index % half)) / dim))
            cos.append(math.cos(angle))
            sin.append(math.sin(angle))
    return cos, sin
