/* tslint:disable */
/* eslint-disable */

/**
 * A GGUF file's header: its metadata and where every tensor lives.
 */
export class GgufHeader {
    free(): void;
    [Symbol.dispose](): void;
    /**
     * Where tensor data begins in the file.
     */
    data_start(): number;
    /**
     * The metadata as JSON. An array longer than 64 entries appears as
     * `{"strings": count}` or `{"numbers": count}`; fetch those by name with
     * `strings` or `numbers`.
     */
    metadata(): string;
    /**
     * Parse from the beginning of a file. `head` needs only to reach the end
     * of the tensor table; nothing after that is read here, and a buffer that
     * stops short fails with an end-of-input error the caller can answer by
     * reading more.
     */
    constructor(head: Uint8Array);
    /**
     * One metadata numeric array by key, as float64: per-token types, scores,
     * or any other table the metadata summarised rather than inlined.
     */
    numbers(key: string): Float64Array;
    /**
     * Where `count` rows of a tensor live, as JSON `{offset, bytes, elements,
     * dtype}`. A row is the fastest-varying axis, which for an embedding table
     * is one token's vector: reading those rows costs a few kilobytes where
     * the whole table would cost a gigabyte.
     */
    row_range(name: string, start: number, count: number): string;
    /**
     * One metadata string array by key: a tokenizer's vocabulary or its merges.
     */
    strings(key: string): any[];
    /**
     * Every tensor as JSON: name, shape, dtype, and the byte range holding it.
     * GGUF writes a shape fastest-varying first and it is reported as stored,
     * so a caller reading the format's own documentation sees these numbers.
     */
    tensors(): string;
    /**
     * The tokenizer this checkpoint was trained with.
     *
     * Built here, from the file's own metadata, and never handed out as data:
     * a vocabulary is 151,936 strings and a merge table 151,387, and moving
     * those across the boundary costs more than tokenizing with them.
     */
    tokenizer(): Tokenizer;
    version(): number;
}

/**
 * Turn one tensor's bytes into float32. `bytes` is exactly the range the
 * header described, read by the caller; this module never sees the file.
 * A checkpoint's own tokenizer. Text in, ids out.
 */
export class Tokenizer {
    private constructor();
    free(): void;
    [Symbol.dispose](): void;
    decode(ids: Uint32Array): string;
    encode(text: string): Uint32Array;
    id_of(piece: string): number | undefined;
    /**
     * One token's text, in the byte-level alphabet the vocabulary uses.
     */
    token(id: number): string | undefined;
    readonly bos: number | undefined;
    readonly eos: number | undefined;
    /**
     * Which pre-tokenizer pattern this vocabulary was trained with.
     */
    readonly pre: string;
    readonly vocab_size: number;
}

export function dequantize(dtype: string, bytes: Uint8Array, elements: number): Float32Array;

/**
 * Whether reading more of the file could make this header parse.
 *
 * A header read starts small and doubles, because how large a header is
 * depends on the tokenizer vocabulary inside it. That loop needs to tell a
 * short read from a file that will never be a header -- otherwise opening
 * something that is not a GGUF file at all costs the whole doubling schedule
 * before it is refused.
 *
 * This is a predicate rather than a property of the thrown error because an
 * error crosses this boundary as a string, and a caller branching on a
 * message is a caller that breaks when the message is reworded.
 */
export function header_needs_more_bytes(head: Uint8Array): boolean;

/**
 * The dtypes this build decodes, so a host can say so before it starts.
 */
export function supported_dtypes(): any[];

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_ggufheader_free: (a: number, b: number) => void;
    readonly __wbg_tokenizer_free: (a: number, b: number) => void;
    readonly dequantize: (a: number, b: number, c: number, d: number, e: number) => [number, number, number, number];
    readonly ggufheader_data_start: (a: number) => number;
    readonly ggufheader_metadata: (a: number) => [number, number];
    readonly ggufheader_new: (a: number, b: number) => [number, number, number];
    readonly ggufheader_numbers: (a: number, b: number, c: number) => [number, number, number, number];
    readonly ggufheader_row_range: (a: number, b: number, c: number, d: number, e: number) => [number, number, number, number];
    readonly ggufheader_strings: (a: number, b: number, c: number) => [number, number, number, number];
    readonly ggufheader_tensors: (a: number) => [number, number];
    readonly ggufheader_tokenizer: (a: number) => [number, number, number];
    readonly ggufheader_version: (a: number) => number;
    readonly header_needs_more_bytes: (a: number, b: number) => number;
    readonly supported_dtypes: () => [number, number];
    readonly tokenizer_bos: (a: number) => number;
    readonly tokenizer_decode: (a: number, b: number, c: number) => [number, number];
    readonly tokenizer_encode: (a: number, b: number, c: number) => [number, number, number, number];
    readonly tokenizer_eos: (a: number) => number;
    readonly tokenizer_id_of: (a: number, b: number, c: number) => number;
    readonly tokenizer_pre: (a: number) => [number, number];
    readonly tokenizer_token: (a: number, b: number) => [number, number];
    readonly tokenizer_vocab_size: (a: number) => number;
    readonly __wbindgen_externrefs: WebAssembly.Table;
    readonly __wbindgen_malloc: (a: number, b: number) => number;
    readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
    readonly __externref_table_dealloc: (a: number) => void;
    readonly __wbindgen_free: (a: number, b: number, c: number) => void;
    readonly __externref_drop_slice: (a: number, b: number) => void;
    readonly __wbindgen_start: () => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
