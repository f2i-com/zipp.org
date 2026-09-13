# Design references

Primary references consulted on 13 September 2026. These establish API semantics;
they do not establish that this package's GPU implementations passed device tests.

- Public ZIPP WASM host queue and lifecycle contract:
  https://raw.githubusercontent.com/f2i-com/zipp.org/main/crates/zipp-wasm/README.md
- Public ZIPP WASM Engine implementation:
  https://raw.githubusercontent.com/f2i-com/zipp.org/main/crates/zipp-wasm/src/lib.rs
- W3C WebGPU specification series and computation scope:
  https://www.w3.org/TR/webgpu/all/
- GPU for the Web WGSL specification:
  https://gpuweb.github.io/gpuweb/wgsl/
- Khronos WebGL 2 specification:
  https://registry.khronos.org/webgl/specs/latest/2.0/
- Khronos EXT_color_buffer_float specification, including floating-point render
  targets and RGBA/FLOAT readback:
  https://registry.khronos.org/webgl/extensions/EXT_color_buffer_float/

Current integration contracts are implemented in this checkout's Python runtime,
WASM Engine boundary and playground Worker. The references below document the
browser APIs; original standalone observations are retained as historical context.