import torch


def _causal(length, offset, window=None):
    rows = []
    for row in range(length):
        position = row + offset
        rows.append([0.0 if (column <= position and (window is None or column > position - window))
                     else -1e9 for column in range(offset + length)])
    return torch.tensor(rows).reshape(1, 1, length, offset + length)


def create_causal_mask(config=None, input_embeds=None, attention_mask=None,
                       cache_position=None, past_key_values=None, position_ids=None, **kwargs):
    length = input_embeds.shape[1]
    offset = past_key_values.get_seq_length() if past_key_values is not None else 0
    return _causal(length, offset)


def create_sliding_window_causal_mask(config=None, input_embeds=None, attention_mask=None,
                                      cache_position=None, past_key_values=None, position_ids=None, **kwargs):
    length = input_embeds.shape[1]
    offset = past_key_values.get_seq_length() if past_key_values is not None else 0
    return _causal(length, offset, getattr(config, "sliding_window", None))
