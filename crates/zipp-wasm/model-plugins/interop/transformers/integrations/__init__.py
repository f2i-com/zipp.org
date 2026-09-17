def use_kernel_forward_from_hub(name):
    """The real one swaps in a fused kernel downloaded from the hub. Nothing is
    downloaded here, so the class keeps its own forward."""
    def decorate(cls):
        return cls
    return decorate
