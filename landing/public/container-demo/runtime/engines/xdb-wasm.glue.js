/* @ts-self-types="./xdb_wasm.d.ts" */

/**
 * A sandboxed, read-write SQLite 3 connection exposed to JavaScript / ZIPP hosts.
 *
 * Supports full DDL (`CREATE TABLE/INDEX/VIEW`, `ALTER TABLE`, `DROP`), DML (`INSERT`,
 * `UPSERT`, `UPDATE`, `DELETE`, `RETURNING`), transactions & nested savepoints, `PRAGMA`,
 * full SQLite 3.54.1 + XDB `SELECT`, deterministic sandbox metering, and exporting valid
 * SQLite 3 `.db` binary images via [`WasmConnection::export_bytes`].
 */
export class WasmConnection {
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        WasmConnectionFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_wasmconnection_free(ptr, 0);
    }
    /**
     * @returns {bigint}
     */
    changes() {
        const ret = wasm.wasmconnection_changes(this.__wbg_ptr);
        return ret;
    }
    close() {
        const ret = wasm.wasmconnection_close(this.__wbg_ptr);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Execute one or more SQLite statements (DDL, DML, transactions, PRAGMA, SELECT) and return
     * `{"columns":[...],"rows":[[...]],"changes":N,"lastInsertRowid":R}` as JSON.
     * @param {string} sql
     * @param {string | null} [params_json]
     * @returns {string}
     */
    execute_json(sql, params_json) {
        let deferred4_0;
        let deferred4_1;
        try {
            const ptr0 = passStringToWasm0(sql, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len0 = WASM_VECTOR_LEN;
            var ptr1 = isLikeNone(params_json) ? 0 : passStringToWasm0(params_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            var len1 = WASM_VECTOR_LEN;
            const ret = wasm.wasmconnection_execute_json(this.__wbg_ptr, ptr0, len0, ptr1, len1);
            var ptr3 = ret[0];
            var len3 = ret[1];
            if (ret[3]) {
                ptr3 = 0; len3 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred4_0 = ptr3;
            deferred4_1 = len3;
            return getStringFromWasm0(ptr3, len3);
        } finally {
            wasm.__wbindgen_free(deferred4_0, deferred4_1, 1);
        }
    }
    /**
     * Execute one or more XDB-dialect SQL statements (`FROM`-first, `GROUP BY ALL`, `ORDER BY ALL`,
     * `::`, `* EXCLUDE`/`REPLACE`, `ILIKE`, plus full DDL/DML/transactions).
     * @param {string} sql
     * @param {string | null} [params_json]
     * @returns {string}
     */
    execute_xdb_json(sql, params_json) {
        let deferred4_0;
        let deferred4_1;
        try {
            const ptr0 = passStringToWasm0(sql, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len0 = WASM_VECTOR_LEN;
            var ptr1 = isLikeNone(params_json) ? 0 : passStringToWasm0(params_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            var len1 = WASM_VECTOR_LEN;
            const ret = wasm.wasmconnection_execute_xdb_json(this.__wbg_ptr, ptr0, len0, ptr1, len1);
            var ptr3 = ret[0];
            var len3 = ret[1];
            if (ret[3]) {
                ptr3 = 0; len3 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred4_0 = ptr3;
            deferred4_1 = len3;
            return getStringFromWasm0(ptr3, len3);
        } finally {
            wasm.__wbindgen_free(deferred4_0, deferred4_1, 1);
        }
    }
    /**
     * Export the live database as a standalone SQLite 3 binary `.db` file (`Uint8Array`).
     * @returns {Uint8Array}
     */
    export_bytes() {
        const ret = wasm.wasmconnection_export_bytes(this.__wbg_ptr);
        var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        return v1;
    }
    /**
     * @returns {boolean}
     */
    in_transaction() {
        const ret = wasm.wasmconnection_in_transaction(this.__wbg_ptr);
        return ret !== 0;
    }
    /**
     * @returns {bigint}
     */
    last_insert_rowid() {
        const ret = wasm.wasmconnection_last_insert_rowid(this.__wbg_ptr);
        return ret;
    }
    /**
     * @returns {string}
     */
    meter_usage_json() {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.wasmconnection_meter_usage_json(this.__wbg_ptr);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * Open a sandboxed read-write SQLite connection.
     *
     * - Pass `initial_bytes = undefined/null` (or empty) to create a brand-new in-memory DB.
     * - Pass an existing SQLite `.db` `Uint8Array` (and optional `wal_bytes` / `journal_bytes`)
     *   to load it into a sandboxed writable copy.
     * - `page_size`: `0` uses SQLite's default `4096`.
     * - `max_steps`, `max_memory_bytes`, `max_page_reads`: `0` = unlimited.
     * @param {Uint8Array | null | undefined} initial_bytes
     * @param {Uint8Array | null | undefined} wal_bytes
     * @param {Uint8Array | null | undefined} journal_bytes
     * @param {number} page_size
     * @param {bigint} max_steps
     * @param {bigint} max_memory_bytes
     * @param {bigint} max_page_reads
     */
    constructor(initial_bytes, wal_bytes, journal_bytes, page_size, max_steps, max_memory_bytes, max_page_reads) {
        var ptr0 = isLikeNone(initial_bytes) ? 0 : passArray8ToWasm0(initial_bytes, wasm.__wbindgen_malloc);
        var len0 = WASM_VECTOR_LEN;
        var ptr1 = isLikeNone(wal_bytes) ? 0 : passArray8ToWasm0(wal_bytes, wasm.__wbindgen_malloc);
        var len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(journal_bytes) ? 0 : passArray8ToWasm0(journal_bytes, wasm.__wbindgen_malloc);
        var len2 = WASM_VECTOR_LEN;
        const ret = wasm.wasmconnection_open(ptr0, len0, ptr1, len1, ptr2, len2, page_size, max_steps, max_memory_bytes, max_page_reads);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        this.__wbg_ptr = ret[0];
        WasmConnectionFinalization.register(this, this.__wbg_ptr, this);
        return this;
    }
    /**
     * @returns {number}
     */
    page_count() {
        const ret = wasm.wasmconnection_page_count(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * @returns {number}
     */
    page_size() {
        const ret = wasm.wasmconnection_page_size(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Seed this database from a declarative JSON seed document (`{ schema, tables, statements, mode }`
     * or shorthand `{ tableName: [{ col: val }, ...] }` with automatic table creation).
     * Runs atomically in a transaction and returns a JSON report:
     * `{"tablesCreated":N,"tablesSeeded":M,"rowsInserted":R,"statementsExecuted":S}`.
     * @param {string} seed_json
     * @returns {string}
     */
    seed_json(seed_json) {
        let deferred3_0;
        let deferred3_1;
        try {
            const ptr0 = passStringToWasm0(seed_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len0 = WASM_VECTOR_LEN;
            const ret = wasm.wasmconnection_seed_json(this.__wbg_ptr, ptr0, len0);
            var ptr2 = ret[0];
            var len2 = ret[1];
            if (ret[3]) {
                ptr2 = 0; len2 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred3_0 = ptr2;
            deferred3_1 = len2;
            return getStringFromWasm0(ptr2, len2);
        } finally {
            wasm.__wbindgen_free(deferred3_0, deferred3_1, 1);
        }
    }
    /**
     * @returns {bigint}
     */
    total_changes() {
        const ret = wasm.wasmconnection_total_changes(this.__wbg_ptr);
        return ret;
    }
}
if (Symbol.dispose) WasmConnection.prototype[Symbol.dispose] = WasmConnection.prototype.free;

/**
 * A sandboxed, read-only SQLite 3 database handle exposed to JavaScript / ZIPP hosts.
 */
export class WasmDatabase {
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        WasmDatabaseFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_wasmdatabase_free(ptr, 0);
    }
    /**
     * Return parsed schema metadata (tables with columns, indexes, views, triggers) as JSON.
     * @returns {string}
     */
    catalog_json() {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.wasmdatabase_catalog_json(this.__wbg_ptr);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * Close the database and release cached page memory from the meter.
     */
    close() {
        const ret = wasm.wasmdatabase_close(this.__wbg_ptr);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Count entries in a named index.
     * @param {string} name
     * @returns {bigint}
     */
    count_index(name) {
        const ptr0 = passStringToWasm0(name, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmdatabase_count_index(this.__wbg_ptr, ptr0, len0);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return BigInt.asUintN(64, ret[0]);
    }
    /**
     * Count rows in a named table.
     * @param {string} name
     * @returns {bigint}
     */
    count_table(name) {
        const ptr0 = passStringToWasm0(name, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmdatabase_count_table(this.__wbg_ptr, ptr0, len0);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return BigInt.asUintN(64, ret[0]);
    }
    /**
     * Database text encoding (`"UTF-8"`, `"UTF-16le"`, or `"UTF-16be"`).
     * @returns {string}
     */
    encoding() {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.wasmdatabase_encoding(this.__wbg_ptr);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * Look up one row by rowid from a named rowid table as JSON (`null` if not found).
     * @param {string} table
     * @param {bigint} rowid
     * @returns {string}
     */
    get_row_json(table, rowid) {
        let deferred3_0;
        let deferred3_1;
        try {
            const ptr0 = passStringToWasm0(table, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len0 = WASM_VECTOR_LEN;
            const ret = wasm.wasmdatabase_get_row_json(this.__wbg_ptr, ptr0, len0, rowid);
            var ptr2 = ret[0];
            var len2 = ret[1];
            if (ret[3]) {
                ptr2 = 0; len2 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred3_0 = ptr2;
            deferred3_1 = len2;
            return getStringFromWasm0(ptr2, len2);
        } finally {
            wasm.__wbindgen_free(deferred3_0, deferred3_1, 1);
        }
    }
    /**
     * Current sandbox meter usage as JSON.
     * @returns {string}
     */
    meter_usage_json() {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.wasmdatabase_meter_usage_json(this.__wbg_ptr);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * Open a SQLite database from raw bytes (with optional `-wal` and `-journal` bytes)
     * under explicit sandbox step, memory, and page-read ceilings (`0` = unlimited).
     * @param {Uint8Array} main_bytes
     * @param {Uint8Array | null | undefined} wal_bytes
     * @param {Uint8Array | null | undefined} journal_bytes
     * @param {bigint} max_steps
     * @param {bigint} max_memory_bytes
     * @param {bigint} max_page_reads
     */
    constructor(main_bytes, wal_bytes, journal_bytes, max_steps, max_memory_bytes, max_page_reads) {
        const ptr0 = passArray8ToWasm0(main_bytes, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        var ptr1 = isLikeNone(wal_bytes) ? 0 : passArray8ToWasm0(wal_bytes, wasm.__wbindgen_malloc);
        var len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(journal_bytes) ? 0 : passArray8ToWasm0(journal_bytes, wasm.__wbindgen_malloc);
        var len2 = WASM_VECTOR_LEN;
        const ret = wasm.wasmdatabase_open(ptr0, len0, ptr1, len1, ptr2, len2, max_steps, max_memory_bytes, max_page_reads);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        this.__wbg_ptr = ret[0];
        WasmDatabaseFinalization.register(this, this.__wbg_ptr, this);
        return this;
    }
    /**
     * Effective page count in this snapshot.
     * @returns {number}
     */
    page_count() {
        const ret = wasm.wasmdatabase_page_count(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Database page size in bytes.
     * @returns {number}
     */
    page_size() {
        const ret = wasm.wasmdatabase_page_size(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Execute a SQLite `SELECT` / `VALUES` / `WITH` query and return
     * `{"columns":[...],"rows":[[...]]}` as JSON.
     *
     * Optional `params_json` may be a JSON array (`[1, "a"]` for positional `?`)
     * or JSON object (`{":id": 1}` / `{"id": 1}` for named parameters).
     * @param {string} sql
     * @param {string | null} [params_json]
     * @returns {string}
     */
    query_json(sql, params_json) {
        let deferred4_0;
        let deferred4_1;
        try {
            const ptr0 = passStringToWasm0(sql, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len0 = WASM_VECTOR_LEN;
            var ptr1 = isLikeNone(params_json) ? 0 : passStringToWasm0(params_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            var len1 = WASM_VECTOR_LEN;
            const ret = wasm.wasmdatabase_query_json(this.__wbg_ptr, ptr0, len0, ptr1, len1);
            var ptr3 = ret[0];
            var len3 = ret[1];
            if (ret[3]) {
                ptr3 = 0; len3 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred4_0 = ptr3;
            deferred4_1 = len3;
            return getStringFromWasm0(ptr3, len3);
        } finally {
            wasm.__wbindgen_free(deferred4_0, deferred4_1, 1);
        }
    }
    /**
     * Execute an XDB-dialect SQL query (`FROM`-first, `GROUP BY ALL`, `ORDER BY ALL`,
     * `::`, `* EXCLUDE`/`REPLACE`, `ILIKE`) and return `{"columns":[...],"rows":[[...]]}` as JSON.
     * @param {string} sql
     * @param {string | null} [params_json]
     * @returns {string}
     */
    query_xdb_json(sql, params_json) {
        let deferred4_0;
        let deferred4_1;
        try {
            const ptr0 = passStringToWasm0(sql, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len0 = WASM_VECTOR_LEN;
            var ptr1 = isLikeNone(params_json) ? 0 : passStringToWasm0(params_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            var len1 = WASM_VECTOR_LEN;
            const ret = wasm.wasmdatabase_query_xdb_json(this.__wbg_ptr, ptr0, len0, ptr1, len1);
            var ptr3 = ret[0];
            var len3 = ret[1];
            if (ret[3]) {
                ptr3 = 0; len3 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred4_0 = ptr3;
            deferred4_1 = len3;
            return getStringFromWasm0(ptr3, len3);
        } finally {
            wasm.__wbindgen_free(deferred4_0, deferred4_1, 1);
        }
    }
    /**
     * Scan up to `limit` entries (`0` = all) from a named index as JSON arrays.
     * @param {string} index
     * @param {number} limit
     * @returns {string}
     */
    scan_index_json(index, limit) {
        let deferred3_0;
        let deferred3_1;
        try {
            const ptr0 = passStringToWasm0(index, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len0 = WASM_VECTOR_LEN;
            const ret = wasm.wasmdatabase_scan_index_json(this.__wbg_ptr, ptr0, len0, limit);
            var ptr2 = ret[0];
            var len2 = ret[1];
            if (ret[3]) {
                ptr2 = 0; len2 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred3_0 = ptr2;
            deferred3_1 = len2;
            return getStringFromWasm0(ptr2, len2);
        } finally {
            wasm.__wbindgen_free(deferred3_0, deferred3_1, 1);
        }
    }
    /**
     * Scan up to `limit` logical rows (`0` = all) from a named table (stored columns) as JSON.
     * @param {string} table
     * @param {number} limit
     * @returns {string}
     */
    scan_table_json(table, limit) {
        let deferred3_0;
        let deferred3_1;
        try {
            const ptr0 = passStringToWasm0(table, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len0 = WASM_VECTOR_LEN;
            const ret = wasm.wasmdatabase_scan_table_json(this.__wbg_ptr, ptr0, len0, limit);
            var ptr2 = ret[0];
            var len2 = ret[1];
            if (ret[3]) {
                ptr2 = 0; len2 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred3_0 = ptr2;
            deferred3_1 = len2;
            return getStringFromWasm0(ptr2, len2);
        } finally {
            wasm.__wbindgen_free(deferred3_0, deferred3_1, 1);
        }
    }
    /**
     * Return `sqlite_schema` entries as JSON.
     * @returns {string}
     */
    schema_json() {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.wasmdatabase_schema_json(this.__wbg_ptr);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * Snapshot mode (`"empty"`, `"rollback"`, `"wal"`, or `"hot-journal-overlay"`).
     * @returns {string}
     */
    snapshot_mode() {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.wasmdatabase_snapshot_mode(this.__wbg_ptr);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
}
if (Symbol.dispose) WasmDatabase.prototype[Symbol.dispose] = WasmDatabase.prototype.free;
function __wbg_get_imports() {
    const import0 = {
        __proto__: null,
        __wbg___wbindgen_throw_344f42d3211c4765: function(arg0, arg1) {
            throw new Error(getStringFromWasm0(arg0, arg1));
        },
        __wbindgen_cast_0000000000000001: function(arg0, arg1) {
            // Cast intrinsic for `Ref(String) -> Externref`.
            const ret = getStringFromWasm0(arg0, arg1);
            return ret;
        },
        __wbindgen_init_externref_table: function() {
            const table = wasm.__wbindgen_externrefs;
            const offset = table.grow(4);
            table.set(0, undefined);
            table.set(offset + 0, undefined);
            table.set(offset + 1, null);
            table.set(offset + 2, true);
            table.set(offset + 3, false);
        },
    };
    return {
        __proto__: null,
        "./xdb_wasm_bg.js": import0,
    };
}

const WasmConnectionFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_wasmconnection_free(ptr, 1));
const WasmDatabaseFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_wasmdatabase_free(ptr, 1));

function getArrayU8FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getUint8ArrayMemory0().subarray(ptr / 1, ptr / 1 + len);
}

function getStringFromWasm0(ptr, len) {
    return decodeText(ptr >>> 0, len);
}

let cachedUint8ArrayMemory0 = null;
function getUint8ArrayMemory0() {
    if (cachedUint8ArrayMemory0 === null || cachedUint8ArrayMemory0.byteLength === 0) {
        cachedUint8ArrayMemory0 = new Uint8Array(wasm.memory.buffer);
    }
    return cachedUint8ArrayMemory0;
}

function isLikeNone(x) {
    return x === undefined || x === null;
}

function passArray8ToWasm0(arg, malloc) {
    const ptr = malloc(arg.length * 1, 1) >>> 0;
    getUint8ArrayMemory0().set(arg, ptr / 1);
    WASM_VECTOR_LEN = arg.length;
    return ptr;
}

function passStringToWasm0(arg, malloc, realloc) {
    if (realloc === undefined) {
        const buf = cachedTextEncoder.encode(arg);
        const ptr = malloc(buf.length, 1) >>> 0;
        getUint8ArrayMemory0().subarray(ptr, ptr + buf.length).set(buf);
        WASM_VECTOR_LEN = buf.length;
        return ptr;
    }

    let len = arg.length;
    let ptr = malloc(len, 1) >>> 0;

    const mem = getUint8ArrayMemory0();

    let offset = 0;

    for (; offset < len; offset++) {
        const code = arg.charCodeAt(offset);
        if (code > 0x7F) break;
        mem[ptr + offset] = code;
    }
    if (offset !== len) {
        if (offset !== 0) {
            arg = arg.slice(offset);
        }
        ptr = realloc(ptr, len, len = offset + arg.length * 3, 1) >>> 0;
        const view = getUint8ArrayMemory0().subarray(ptr + offset, ptr + len);
        const ret = cachedTextEncoder.encodeInto(arg, view);

        offset += ret.written;
        ptr = realloc(ptr, len, offset, 1) >>> 0;
    }

    WASM_VECTOR_LEN = offset;
    return ptr;
}

function takeFromExternrefTable0(idx) {
    const value = wasm.__wbindgen_externrefs.get(idx);
    wasm.__externref_table_dealloc(idx);
    return value;
}

let cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
cachedTextDecoder.decode();
const MAX_SAFARI_DECODE_BYTES = 2146435072;
let numBytesDecoded = 0;
function decodeText(ptr, len) {
    numBytesDecoded += len;
    if (numBytesDecoded >= MAX_SAFARI_DECODE_BYTES) {
        cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
        cachedTextDecoder.decode();
        numBytesDecoded = len;
    }
    return cachedTextDecoder.decode(getUint8ArrayMemory0().subarray(ptr, ptr + len));
}

const cachedTextEncoder = new TextEncoder();

if (!('encodeInto' in cachedTextEncoder)) {
    cachedTextEncoder.encodeInto = function (arg, view) {
        const buf = cachedTextEncoder.encode(arg);
        view.set(buf);
        return {
            read: arg.length,
            written: buf.length
        };
    };
}

let WASM_VECTOR_LEN = 0;

let wasmModule, wasmInstance, wasm;
function __wbg_finalize_init(instance, module) {
    wasmInstance = instance;
    wasm = instance.exports;
    wasmModule = module;
    cachedUint8ArrayMemory0 = null;
    wasm.__wbindgen_start();
    return wasm;
}

async function __wbg_load(module, imports) {
    if (typeof Response === 'function' && module instanceof Response) {
        if (typeof WebAssembly.instantiateStreaming === 'function') {
            try {
                return await WebAssembly.instantiateStreaming(module, imports);
            } catch (e) {
                const validResponse = module.ok && expectedResponseType(module.type);

                if (validResponse && module.headers.get('Content-Type') !== 'application/wasm') {
                    console.warn("`WebAssembly.instantiateStreaming` failed because your server does not serve Wasm with `application/wasm` MIME type. Falling back to `WebAssembly.instantiate` which is slower. Original error:\n", e);

                } else { throw e; }
            }
        }

        const bytes = await module.arrayBuffer();
        return await WebAssembly.instantiate(bytes, imports);
    } else {
        const instance = await WebAssembly.instantiate(module, imports);

        if (instance instanceof WebAssembly.Instance) {
            return { instance, module };
        } else {
            return instance;
        }
    }

    function expectedResponseType(type) {
        switch (type) {
            case 'basic': case 'cors': case 'default': return true;
        }
        return false;
    }
}

function initSync(module) {
    if (wasm !== undefined) return wasm;


    if (module !== undefined) {
        if (Object.getPrototypeOf(module) === Object.prototype) {
            ({module} = module)
        } else {
            console.warn('using deprecated parameters for `initSync()`; pass a single object instead')
        }
    }

    const imports = __wbg_get_imports();
    if (!(module instanceof WebAssembly.Module)) {
        module = new WebAssembly.Module(module);
    }
    const instance = new WebAssembly.Instance(module, imports);
    return __wbg_finalize_init(instance, module);
}

async function __wbg_init(module_or_path) {
    if (wasm !== undefined) return wasm;


    if (module_or_path !== undefined) {
        if (Object.getPrototypeOf(module_or_path) === Object.prototype) {
            ({module_or_path} = module_or_path)
        } else {
            console.warn('using deprecated parameters for the initialization function; pass a single object instead')
        }
    }

    if (module_or_path === undefined) {
        module_or_path = new URL('xdb_wasm_bg.wasm', import.meta.url);
    }
    const imports = __wbg_get_imports();

    if (typeof module_or_path === 'string' || (typeof Request === 'function' && module_or_path instanceof Request) || (typeof URL === 'function' && module_or_path instanceof URL)) {
        module_or_path = fetch(module_or_path);
    }

    const { instance, module } = await __wbg_load(await module_or_path, imports);

    return __wbg_finalize_init(instance, module);
}

export { initSync, __wbg_init as default };
