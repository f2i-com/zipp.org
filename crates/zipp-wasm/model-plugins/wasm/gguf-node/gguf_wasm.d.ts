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
