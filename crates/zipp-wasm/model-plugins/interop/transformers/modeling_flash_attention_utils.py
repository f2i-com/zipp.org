class FlashAttentionKwargs(dict):
    pass


def is_flash_attn_available():
    """No fused attention kernel here; the eager path is the one that runs."""
    return False


def flash_attn_supports_top_left_mask():
    return False
