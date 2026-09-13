# GPU integration maintenance checklist

The Python integration is implemented in this repository. Keep the generic GPU
runtime independent of any particular research model or native training service.

When changing graphs or transport, test the Python-enabled WASM artifact, the
portable JS/WASM backends, adapter lifetime behavior and real browser hardware.
Keep guest permissions explicit; invalidate pending work before disposing engines.
Preserve versioned file-change semantics and paired browser artifact fingerprints.

Future work includes RGBA scalar packing, tiled matrix multiplication, kernel
fusion and persistent graph state. Measure each optimization with numerical
parity and real adapter reports. Texture budgets must include padding and scratch;
logical graph accounting alone is not a physical GPU allocation ceiling.
