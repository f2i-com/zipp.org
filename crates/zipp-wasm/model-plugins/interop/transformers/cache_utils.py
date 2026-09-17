import torch


class Cache:
    """A per-layer key/value cache, concatenated on the sequence axis."""

    def __init__(self, config=None):
        self.layers = {}

    def update(self, key_states, value_states, layer_idx, cache_kwargs=None):
        if layer_idx in self.layers:
            keys, values = self.layers[layer_idx]
            key_states = torch.cat([keys, key_states], dim=-2)
            value_states = torch.cat([values, value_states], dim=-2)
        self.layers[layer_idx] = (key_states, value_states)
        return key_states, value_states

    def get_seq_length(self, layer_idx=0):
        if layer_idx not in self.layers:
            return 0
        return self.layers[layer_idx][0].shape[-2]

    def __len__(self):
        return len(self.layers)


class DynamicCache(Cache):
    pass
