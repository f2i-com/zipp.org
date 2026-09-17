"""Small data-only graph builder; no torch, host JS, I/O or weight decoding.

The same shape as the tiny-causal builder, with what GPT-Neo actually needs:
projections that carry no bias, the tanh GELU approximation rather than ZIPP's
erf kernel, a windowed causal mask for local-attention layers, and a one-hot
row selector, because the graph protocol has no slice operation and a real
vocabulary makes full-context logits far larger than the last row anyone reads.
"""
import math


class Graph:
    def __init__(self):
        self.nodes = []
        self.bindings = []
        self.weights = {}
        self.constants = {}
        self.carried = []

    def op(self, name, **attrs):
        index = len(self.nodes)
        node = {"id": index, "op": name}
        node.update(attrs)
        self.nodes.append(node)
        return index

    def scalar(self, value):
        # Constants are shared. A deep model repeats the same epsilon and GELU
        # coefficients in every layer, and the node budget is a real limit: the
        # eight-layer checkpoint below emits 573 nodes before this, 529 after.
        value = float(value)
        if value not in self.constants:
            self.constants[value] = self.op("full", shape=[], value=value)
        return self.constants[value]

    def weight(self, name, shape, transpose=False):
        """Bind a checkpoint tensor. `shape` is what the graph needs; with
        transpose the checkpoint stores it the other way round, which is how a
        PyTorch [out, in] linear is read without rewriting the file."""
        key = (name, transpose)
        if key in self.weights:
            return self.weights[key]
        index = self.op("input", shape=shape)
        binding = {"node": index, "kind": "tensor", "tensor": name}
        if transpose:
            binding["transpose"] = True
        self.bindings.append(binding)
        self.weights[key] = index
        return index

    def rows(self, name, indices, width):
        index = self.op("input", shape=[len(indices), width])
        self.bindings.append({"node": index, "kind": "rows", "tensor": name, "indices": list(indices)})
        return index

    def causal_mask(self, length, window=None):
        index = self.op("input", shape=[length, length])
        binding = {"node": index, "kind": "causal", "length": length}
        if window is not None:
            binding["window"] = window
        self.bindings.append(binding)
        return index

    def one_hot_row(self, length, position):
        """A [1, length] selector: the only way to read one row of a tensor when
        the protocol has no slice, and cheap enough that it beats computing
        vocabulary-sized logits for every position and discarding all but one."""
        data = [0.0]*length
        data[position] = 1.0
        return self.op("input", shape=[1, length], data=data)

    def step_rows(self, tensor, index, width):
        """One gathered row the host supplies per token, by token id or position."""
        node = self.op("input", shape=[1, width])
        self.bindings.append({"node": node, "kind": "step", "slot": "rows",
                              "tensor": tensor, "index": index})
        return node

    def step_mask(self, context, window=None):
        """The additive mask for the positions written so far."""
        node = self.op("input", shape=[1, context])
        binding = {"node": node, "kind": "step", "slot": "mask"}
        if window is not None:
            binding["window"] = window
        self.bindings.append(binding)
        return node

    def step_write(self, context):
        """A one-hot column marking where this token writes into the caches.

        The protocol has no scatter, so writing is arithmetic: the old cache
        times (1 - write), plus write times the new row.
        """
        node = self.op("input", shape=[context, 1])
        self.bindings.append({"node": node, "kind": "step", "slot": "write"})
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

    def linear(self, x, weight, ins, outs, bias=None):
        """[tokens, ins] @ [ins, outs], reading the checkpoint's own [outs, ins]
        matrix through a transposing binding. `bias` is the bias tensor's name,
        or None where the checkpoint has none -- GPT-Neo's q/k/v projections
        carry no bias and its output projection does."""
        y = self.op("matmul", a=x, b=self.weight(weight, [ins, outs], transpose=True))
        if bias is None:
            return y
        return self.op("add", a=y, b=self.weight(bias, [outs]))

    def layer_norm(self, x, weight, bias, width, epsilon):
        mean = self.op("mean", a=x, axis=-1, keepdim=True)
        centered = self.op("sub", a=x, b=mean)
        variance = self.op("mean", a=self.op("mul", a=centered, b=centered), axis=-1, keepdim=True)
        denominator = self.op("sqrt", a=self.op("add", a=variance, b=self.scalar(epsilon)))
        normalized = self.op("div", a=centered, b=denominator)
        return self.op("add", a=self.op("mul", a=normalized, b=self.weight(weight, [width])),
                       b=self.weight(bias, [width]))

    def gelu_new(self, x):
        """GPT-2/GPT-Neo's `gelu_new`: 0.5x(1 + tanh(sqrt(2/pi)(x + 0.044715x^3))).

        ZIPP's `gelu` kernel is the erf form. The two differ by roughly 1e-3 at
        the peak, which is far outside any sane logit tolerance, so this composes
        the tanh form the checkpoint was trained with instead of reusing the
        kernel that merely shares the name."""
        cubed = self.op("mul", a=x, b=self.op("mul", a=x, b=x))
        inner = self.op("add", a=x, b=self.op("mul", a=self.scalar(0.044715), b=cubed))
        tanh = self.op("tanh", a=self.op("mul", a=self.scalar(math.sqrt(2.0/math.pi)), b=inner))
        return self.op("mul", a=self.op("mul", a=self.scalar(0.5), b=x),
                       b=self.op("add", a=self.scalar(1.0), b=tanh))

    def finish(self, logits):
        return {"version": 1, "graph": {"version": 2, "nodes": self.nodes,
                "outputs": [{"name": "logits", "id": logits}]}, "bindings": self.bindings}

    def finish_decode(self, logits, context):
        outputs = [{"name": "logits", "id": logits}]
        outputs.extend({"name": entry["name"], "id": entry["id"]} for entry in self.carried)
        return {"version": 1, "kind": "decode", "context": context,
                "graph": {"version": 2, "nodes": self.nodes, "outputs": outputs},
                "bindings": self.bindings}
