"""A transformers-shaped surface, small enough to read."""


class PreTrainedTokenizer:
    """Tokenization is the host's business here; configuration files import
    this name only to annotate arguments they never receive."""


class TensorType:
    PYTORCH = "pt"
    NUMPY = "np"


def is_torch_available():
    return True


def is_tf_available():
    return False
