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

    def __getitem__(self, key):
        if isinstance(key, str):
            return self._fields[key]
        return list(self._fields.values())[key]

    def to_tuple(self):
        return tuple(self._fields.values())


class BaseModelOutputWithPast(ModelOutput):
    pass


class CausalLMOutputWithPast(ModelOutput):
    pass
