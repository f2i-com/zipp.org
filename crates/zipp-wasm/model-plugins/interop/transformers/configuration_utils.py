class PretrainedConfig:
    model_type = ""
    sub_configs = {}
    # Alternative names for attributes, as transformers maps them: GPT-Neo's
    # `num_hidden_layers` is its `num_layers`, whichever the caller passes.
    attribute_map = {}

    def __setattr__(self, key, value):
        object.__setattr__(self, type(self).attribute_map.get(key, key), value)

    def __getattr__(self, key):
        mapped = type(self).attribute_map.get(key)
        if mapped is None:
            raise AttributeError(key)
        return getattr(self, mapped)

    @property
    def use_return_dict(self):
        return self.return_dict and not self.torchscript

    def __init__(self, **kwargs):
        self.output_attentions = kwargs.pop("output_attentions", False)
        self.output_hidden_states = kwargs.pop("output_hidden_states", False)
        self.return_dict = kwargs.pop("return_dict", True)
        self.torchscript = kwargs.pop("torchscript", False)
        self.pruned_heads = kwargs.pop("pruned_heads", {})
        self.tie_word_embeddings = kwargs.pop("tie_word_embeddings", True)
        self.chunk_size_feed_forward = kwargs.pop("chunk_size_feed_forward", 0)
        self.is_encoder_decoder = kwargs.pop("is_encoder_decoder", False)
        self.is_decoder = kwargs.pop("is_decoder", False)
        # Special-token ids a modelling file reads straight off the config.
        self.pad_token_id = kwargs.pop("pad_token_id", None)
        self.bos_token_id = kwargs.pop("bos_token_id", None)
        self.eos_token_id = kwargs.pop("eos_token_id", None)
        self.sep_token_id = kwargs.pop("sep_token_id", None)
        self.decoder_start_token_id = kwargs.pop("decoder_start_token_id", None)
        self.num_labels = kwargs.pop("num_labels", 2)
        self._attn_implementation = kwargs.pop("attn_implementation", "eager")
        for name, value in kwargs.items():
            setattr(self, name, value)

    def get_text_config(self, decoder=False):
        return self


def layer_type_validation(layer_types, num_hidden_layers=None):
    return None
