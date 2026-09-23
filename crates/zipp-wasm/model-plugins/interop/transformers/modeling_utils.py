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

    def get_head_mask(self, head_mask, num_hidden_layers, is_attention_chunked=False):
        """transformers' per-layer head mask: None for every layer, or a mask
        broadcast to [layers, batch, heads, seq, seq] as the package does."""
        if head_mask is None:
            return [None] * num_hidden_layers
        if head_mask.dim() == 1:
            head_mask = head_mask.unsqueeze(0).unsqueeze(0).unsqueeze(-1).unsqueeze(-1)
            head_mask = head_mask.expand(num_hidden_layers, -1, -1, -1, -1)
        elif head_mask.dim() == 2:
            head_mask = head_mask.unsqueeze(1).unsqueeze(-1).unsqueeze(-1)
        if is_attention_chunked:
            head_mask = head_mask.unsqueeze(-1)
        return head_mask

    def get_input_embeddings(self):
        return None

    def set_input_embeddings(self, value):
        return None
