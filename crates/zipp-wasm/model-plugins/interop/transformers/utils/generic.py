def check_model_inputs(function):
    """Normalise the kwargs the wrapped forward expects.

    The real decorator also records intermediate outputs through the model's
    `_can_record_outputs` map; nothing here asks for those, so it settles
    `use_cache` and drops `return_dict` and passes the call through.
    """
    def wrapper(self, *args, **kwargs):
        if kwargs.get("use_cache") is None:
            kwargs["use_cache"] = getattr(self.config, "use_cache", False)
        kwargs.pop("return_dict", None)
        for name in ("output_attentions", "output_hidden_states"):
            kwargs.pop(name, None)
        return function(self, *args, **kwargs)
    return wrapper
