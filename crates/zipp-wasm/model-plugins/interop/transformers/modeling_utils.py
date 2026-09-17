from torch import nn

ALL_ATTENTION_FUNCTIONS = {}


class PreTrainedModel(nn.Module):
    """What a modelling file needs from the base class: somewhere to keep the
    config, and a post_init hook. Loading, sharding, device maps, hub access and
    quantisation are the package's business, not a model definition's."""

    config_class = None
    base_model_prefix = ""
    supports_gradient_checkpointing = False
    _supports_sdpa = False
    _supports_flash_attn = False

    def __init__(self, config, *args, **kwargs):
        super().__init__()
        self.config = config

    def post_init(self):
        return None

    def get_input_embeddings(self):
        return None

    def set_input_embeddings(self, value):
        return None
