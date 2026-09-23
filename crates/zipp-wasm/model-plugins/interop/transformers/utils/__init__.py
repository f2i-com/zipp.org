class TransformersKwargs(dict):
    """A kwargs marker; the modelling files only use it in annotations."""


def auto_docstring(function=None, **kwargs):
    if function is None:
        return lambda inner: inner
    return function


def can_return_tuple(function):
    return function


def is_torchdynamo_compiling():
    return False


def is_torch_flex_attn_available():
    """No flex attention here; files that guard on it take the eager path."""
    return False
