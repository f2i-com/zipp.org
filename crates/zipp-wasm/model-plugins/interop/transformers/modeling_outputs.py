class ModelOutput:
    def __init__(self, **fields):
        self._fields = fields
        for name, value in fields.items():
            setattr(self, name, value)

    def __getattr__(self, name):
        # A field the caller did not set reads as absent, not as an error: the
        # real outputs are dataclasses with None defaults.
        if name.startswith("_"):
            raise AttributeError(name)
        return None

    def _present(self):
        # As in transformers: a field left as None is not a key, so `out[0]` is
        # the logits of an output built with `loss=None`, not the loss.
        return {name: value for name, value in self._fields.items() if value is not None}

    def __getitem__(self, key):
        if isinstance(key, str):
            return self._present()[key]
        return list(self._present().values())[key]

    def keys(self):
        return list(self._present().keys())

    def to_tuple(self):
        return tuple(self._present().values())


class BaseModelOutputWithPast(ModelOutput):
    pass


class CausalLMOutputWithPast(ModelOutput):
    pass
