"""An alias module: PyTorch keeps the same objects under several names
(torch.quantization for torch.ao.quantization, torch.nn.quantized for
torch.ao.nn.quantized, torch.ao.quantization.observer and its siblings for
the pieces of torch.ao.quantization, torch.distributed.distributed_c10d for
torch.distributed, ...). Every such module is this file; it re-exports the
objects (the same objects, so isinstance checks agree) of the module its
own name maps to."""
if __name__ == "torch.nn.quantized":
    from torch.ao.nn.quantized import *
    from torch.ao.nn.quantized import LinearPackedParams, WeightedQuantizedModule, _quantize_weight
elif __name__ in ("torch.nn.quantized.dynamic",):
    from torch.ao.nn.quantized.dynamic import *
elif __name__ == "torch.nn.quantized.functional":
    from torch.ao.nn.quantized.functional import *
elif __name__ == "torch.nn.intrinsic":
    from torch.ao.nn.intrinsic import *
    from torch.ao.nn.intrinsic import _FusedModule
elif __name__ == "torch.nn.intrinsic.quantized":
    from torch.ao.nn.intrinsic.quantized import *
elif __name__ == "torch.nn.intrinsic.qat":
    from torch.ao.nn.intrinsic.qat import *
elif __name__ == "torch.nn.qat":
    from torch.ao.nn.qat import *
elif __name__ == "torch.ao.nn.intrinsic.quantized.dynamic":
    from torch.ao.nn.quantized.dynamic import _LinearReLU as LinearReLU
    __all__ = ["LinearReLU"]
elif __name__ == "torch.distributed.distributed_c10d":
    from torch.distributed import *
    from torch.distributed import _get_default_group, _state
else:
    # torch.quantization and the parts of torch.ao.quantization
    # (observer, fake_quantize, qconfig, quantize, fuse_modules, stubs,
    # utils, quantize_fx).
    from torch.ao.quantization import *
    from torch.ao.quantization import (_PartialWrapper, _with_args, _with_callable_args, _is_activation_post_process, _ObserverBase,
                                       _convert, _remove_qconfig, _add_observer_, _propagate_qconfig_helper, _observer_forward_hook,
                                       _observer_forward_pre_hook, is_activation_post_process, calculate_qmin_qmax, check_min_max_valid,
                                       get_combined_dict, is_per_tensor, is_per_channel, validate_qmin_qmax, get_qparam_dict,
                                       has_no_children_ignoring_parametrizations, activation_dtype, weight_dtype,
                                       activation_is_statically_quantized, QuantType, DEFAULT_STATIC_QUANT_MODULE_MAPPINGS,
                                       DEFAULT_QAT_MODULE_MAPPINGS, DEFAULT_DYNAMIC_QUANT_MODULE_MAPPINGS, DEFAULT_MODULE_TO_ACT_POST_PROCESS,
                                       get_default_custom_config_dict, fuse_known_modules, prepare_fx, convert_fx, prepare_qat_fx, fuse_fx,
                                       get_default_qconfig_mapping, get_default_qat_qconfig_mapping, _is_symmetric_quant, _is_float_qparams,
                                       default_embedding_fake_quant, get_default_static_quant_reference_module_mappings,
                                       get_default_float_to_quantized_operator_mappings, fuse_convtranspose_bn)
