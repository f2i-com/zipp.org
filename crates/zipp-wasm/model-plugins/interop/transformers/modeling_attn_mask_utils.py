import torch


class AttentionMaskConverter:
    """The older mask helper. Only the causal 4-D form is built here, which is
    what a decoder asks for; the padding and sliding variants belong with a
    tokenizer and a batch this host does not have."""

    def __init__(self, is_causal=True, sliding_window=None):
        self.is_causal = is_causal
        self.sliding_window = sliding_window

    @staticmethod
    def _make_causal_mask(input_ids_shape, dtype=None, device=None, past_key_values_length=0,
                          sliding_window=None):
        batch, length = input_ids_shape
        total = length + past_key_values_length
        rows = []
        for row in range(length):
            position = row + past_key_values_length
            rows.append([0.0 if (column <= position and
                                 (sliding_window is None or column > position - sliding_window))
                         else -1e9 for column in range(total)])
        return torch.tensor(rows).reshape(1, 1, length, total)


def _prepare_4d_causal_attention_mask(attention_mask, input_shape, inputs_embeds,
                                      past_key_values_length=0, sliding_window=None):
    return AttentionMaskConverter._make_causal_mask(
        input_shape, past_key_values_length=past_key_values_length, sliding_window=sliding_window)
