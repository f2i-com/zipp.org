"""Small data-only graph builder; no torch, host JS, I/O or weight decoding."""
class Graph:
    def __init__(self):
        self.nodes = []
        self.bindings = []
        self.weights = {}

    def op(self, name, **attrs):
        index = len(self.nodes)
        node = {"id": index, "op": name}
        node.update(attrs)
        self.nodes.append(node)
        return index

    def scalar(self, value):
        return self.op("full", shape=[], value=float(value))

    def weight(self, name, shape):
        if name in self.weights:
            return self.weights[name]
        index = self.op("input", shape=shape)
        self.bindings.append({"node": index, "kind": "tensor", "tensor": name})
        self.weights[name] = index
        return index

    def rows(self, name, indices, width):
        index = self.op("input", shape=[len(indices), width])
        self.bindings.append({"node": index, "kind": "rows", "tensor": name, "indices": list(indices)})
        return index

    def causal_mask(self, length):
        index = self.op("input", shape=[length, length])
        self.bindings.append({"node": index, "kind": "causal", "length": length})
        return index

    def linear(self, x, name, ins, outs):
        # The package's weight layout is [input, output], not PyTorch's [out,in].
        w = self.weight(name + ".weight", [ins, outs])
        b = self.weight(name + ".bias", [outs])
        return self.op("add", a=self.op("matmul", a=x, b=w), b=b)

    def layer_norm(self, x, name, width, epsilon):
        mean = self.op("mean", a=x, axis=-1, keepdim=True)
        centered = self.op("sub", a=x, b=mean)
        variance = self.op("mean", a=self.op("mul", a=centered, b=centered), axis=-1, keepdim=True)
        denominator = self.op("sqrt", a=self.op("add", a=variance, b=self.scalar(epsilon)))
        normalized = self.op("div", a=centered, b=denominator)
        w = self.weight(name + ".weight", [width])
        b = self.weight(name + ".bias", [width])
        return self.op("add", a=self.op("mul", a=normalized, b=w), b=b)

    def finish(self, logits):
        return {"version": 1, "graph": {"version": 2, "nodes": self.nodes,
                "outputs": [{"name": "logits", "id": logits}]}, "bindings": self.bindings}
