from torch import nn


class GradientCheckpointingLayer(nn.Module):
    """Checkpointing is a training memory trade; inference needs none of it."""
    gradient_checkpointing = False


class _GenericHead(nn.Module):
    """The task heads a modelling file defines beside the causal model. They are
    imported to be subclassed; nothing here builds one."""


class GenericForQuestionAnswering(_GenericHead):
    pass


class GenericForSequenceClassification(_GenericHead):
    pass


class GenericForTokenClassification(_GenericHead):
    pass
