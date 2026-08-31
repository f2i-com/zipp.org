/// <reference types="vite/client" />

// A short hash of the wasm-bindgen glue and the .wasm together, injected by
// vite.config.ts. Both URLs carry it so a cache can never pair a new .wasm with
// the previous glue — their import names are hash-matched and a mismatch fails
// instantiation outright.
declare const __ZIPP_WASM_BUILD__: string
