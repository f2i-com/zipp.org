class OnnxConfig:
    """ONNX export description. Exporting is the package's business, not a
    model definition's, but older configuration files import it to subclass."""

    def __init__(self, config=None, task="default", patching_specs=None):
        self.config = config
        self.task = task


class OnnxConfigWithPast(OnnxConfig):
    def __init__(self, config=None, task="default", patching_specs=None, use_past=False):
        super().__init__(config, task, patching_specs)
        self.use_past = use_past


class OnnxSeq2SeqConfigWithPast(OnnxConfigWithPast):
    pass
