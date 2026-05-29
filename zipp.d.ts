// ZIPP ambient declarations — make `.ts` programs written for ZIPP type-check
// under `tsc` and in your editor. Reference it from a project's tsconfig
// (`"include": ["zipp.d.ts", "**/*.ts"]`) or with `/// <reference path="zipp.d.ts" />`.
//
// The ZIPP numeric types are aliased to `number` here so ordinary arithmetic
// (`a + b`) still type-checks. That means tsc treats `i64`/`i32`/`u32`/`u64`/`f64`
// all as `number` and will NOT catch width/signedness mixing — ZIPP's own sound
// checker does that. tsc still gives you the rest: autocomplete, undefined-name
// errors, wrong arity, missing/mismatched returns, interface field checks, etc.

// ── ZIPP integer/float types (tsc sees them as `number`) ──
type i64 = number;
type i32 = number;
type u32 = number;
type u64 = number;
type f64 = number;

// ── numeric casts (runtime conversions in ZIPP; `i64(x)` truncates, etc.) ──
declare function i64(x: number): i64;
declare function i32(x: number): i32;
declare function u32(x: number): u32;
declare function u64(x: number): u64;
declare function f64(x: number): f64;

// ── math builtins ──
declare function abs(x: number): number;
declare function min(a: number, b: number): number;
declare function max(a: number, b: number): number;
declare function pow(base: i64, exp: i64): i64;
declare function sqrt(x: f64): f64;
declare function floor(x: f64): f64;
declare function ceil(x: f64): f64;

// ── length of an array or string ──
declare function len(x: ArrayLike<unknown> | string): i64;

// ── output (ZIPP's `print`, plus a minimal `console.log`) ──
declare function print(x: number | string): void;
declare const console: { log(x: number | string): void };
