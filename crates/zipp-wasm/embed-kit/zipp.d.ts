// Types for zipp.mjs. The Engine class and the module-level functions are
// declared in zipp-engine.d.ts, generated from the complete JavaScript +
// Python engine; the Python-only members (initPythonProject, pythonCall,
// pythonHas, setPythonInput, takeHostRequests, takeUi, addPythonPackage,
// pythonPackages, prewarmPython) exist only in the javascript-python and
// javascript-python-base modules.

import type * as ZippEngine from "./zipp-engine.js";

export declare const ZIPP_LOADER_VERSION: string;

/** Where a module comes from: a URL or path (Node), a fetch Response, bytes or a compiled module. */
export type ZippSource =
  | string
  | URL
  | Response
  | Promise<Response>
  | ArrayBuffer
  | ArrayBufferView
  | Blob
  | WebAssembly.Module;

/** The `zipp.meta` record a release module carries. */
export interface ZippMeta {
  format: 1;
  variant: "javascript" | "lite" | "javascript-python" | "javascript-python-base" | "torch-package";
  version: string;
  commit: string | null;
  languages: string[];
  glueSha256?: string;
}

export interface ZippStoragePermission {
  /** Items the program's localStorage starts with. */
  initial?: Record<string, string>;
  /** Characters of keys plus values it may hold (default 5,000,000). */
  quota?: number;
  /** A trusted Storage (getItem/setItem/removeItem/key/length), seen only under `prefix`; isolate: false only. */
  backend?: Storage;
  /** The key prefix in `backend` (default "zipp:"). */
  prefix?: string;
}

export interface ZippFilesPermission {
  files: Record<string, string>;
  /** Allow fs.write and fs.remove (default false). */
  writable?: boolean;
  /** Characters of paths plus contents the files may hold (default 16,000,000). */
  quota?: number;
}

export interface ZippNetworkPermission {
  /** Origins ("https://api.example.com") or URL prefixes ("https://example.com/data/"), or "*". */
  allow: string[];
  /** Default GET, HEAD, POST, PUT, PATCH, DELETE. */
  methods?: string[];
  /** Default 1,000,000. */
  maxResponseBytes?: number;
  /** Per request; default 30,000. */
  timeoutMs?: number;
  /** Follow redirects to allowed URLs (default false: a redirect fails the request). */
  redirects?: boolean;
  /** A fetch to use instead of globalThis.fetch. */
  fetch?: typeof fetch;
}

export interface ZippSandboxOptions {
  /** "javascript" (default) or "python" (Python modules only). */
  language?: "javascript" | "python";
  /** Run in a Worker / worker thread (default true). */
  isolate?: boolean;
  limits?: {
    /** Steps the program may take in total (default 50,000,000). */
    instructions?: number;
    /** Milliseconds `run` or `call` may take (default 30,000); an isolated sandbox's Worker is ended past it. */
    timeoutMs?: number;
  };
  permissions?: {
    storage?: boolean | ZippStoragePermission;
    /** Paths to text, or { files, writable, quota }. For Python: its importable `name.py` modules. */
    files?: Record<string, string> | ZippFilesPermission;
    network?: boolean | string | string[] | ZippNetworkPermission;
  };
  /** Functions the program may call with host.call(kind, args, reply). */
  host?: Record<string, (...args: string[]) => unknown>;
  /** A served copy of zipp.mjs for the Worker, when zipp.mjs is bundled. */
  workerUrl?: string | URL;
}

export declare class ZippSandboxError extends Error {
  /** guest, source, resource, timeout, host, usage, conversion or closed. */
  readonly category: string;
  /** Whether the sandbox is finished. */
  readonly terminal: boolean;
  /** What the program printed before it failed. */
  readonly output: string[];
}

export interface ZippSandbox {
  /** Run the program (once); resolves when its host requests are done. */
  run(code: string): Promise<{ output: string[]; symbols: Record<string, { index: number; scope: string }> | null }>;
  /** Call one of its top-level functions. */
  call(name: string, args?: unknown[]): Promise<{ value: unknown; output: string[] }>;
  /** Its storage and files as they are now. */
  snapshot(): Promise<{ storage: Record<string, string> | null; files: Record<string, string> | null }>;
  /** The engine's resource account. */
  usage(): Promise<Record<string, unknown>>;
  close(): Promise<void>;
  readonly closed: boolean;
}

export interface Zipp {
  readonly meta: ZippMeta;
  /** The engine's own `zippProfile()`: version, source, variant, languages and limits. */
  readonly profile: { version: string; variant?: string; languages: string[]; source?: { sha: string | null }; limits?: Record<string, number> } & Record<string, unknown>;
  readonly module: WebAssembly.Module;
  readonly packages: readonly WebAssembly.Module[];
  /** The module's wasm-bindgen bindings, initialized. */
  readonly bindings: typeof ZippEngine;
  readonly Engine: typeof ZippEngine.Engine;
  readonly languages: readonly string[];
  createSandbox(options?: ZippSandboxOptions): Promise<ZippSandbox>;
  /** Run `code` here and now, with nothing permitted; throws on failure. */
  run(code: string, options?: { language?: "javascript" | "python"; instructionBudget?: number | null }): { output: string[] };
  /** Add the torch package to this javascript-python-base instance. */
  addTorch(source: ZippSource): Promise<unknown>;
}

export declare function compileZipp(source: ZippSource): Promise<WebAssembly.Module>;
export declare function readZippMeta(module: WebAssembly.Module): ZippMeta | null;
export declare function loadZipp(
  source: ZippSource | { engine: ZippSource; packages?: ZippSource[] },
  options?: { expectVersion?: string; packages?: ZippSource[] },
): Promise<Zipp>;
