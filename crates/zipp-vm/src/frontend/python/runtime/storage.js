/* ZIPP Python runtime — what the base runtime needs from tensor storages,
 * with or without the torch package. Apache-2.0.
 *
 * The host transport (entry.js, native_gpu.js) moves float32 data as a
 * `_zipp_tensor._Storage` of dtype float32 that owns a Float32Array, and
 * `zipfile` checksums its entries with a table-driven CRC-32. Both used to
 * live in tensor.js, which now ships in the torch package (built in, or added
 * by the host); they are defined here so the base runtime has them either
 * way. tensor.js, compiled right after this file, reuses `rt.StorageType`,
 * so the type is created at the same point it always was.
 */
(function (R) {
    "use strict";
    const rt = R.__rt;
    const Storage = rt.newType("_Storage", [rt.ObjectType], new Map(), "_zipp_tensor");
    rt.StorageType = Storage;
    // The host transport (entry.js): a float32 storage leaves as its
    // Float32Array, and a Float32Array the host sends arrives as a storage
    // that owns it (the VM made it from the host's copy).
    rt.float32Storage = (data) => ({ cls: Storage, dtype: "float32", data: data, version: 0, untracked: 0 });
    rt.isFloat32Storage = (v) => v !== null && typeof v === "object" && v.cls === Storage && v.dtype === "float32";
    // zlib's CRC-32 of a byte list, continuing from `start`.
    let CRC_TABLE = null;
    rt.crc32 = function (items, start) {
        if (CRC_TABLE === null) {
            CRC_TABLE = new Int32Array(256);
            for (let n = 0; n < 256; n++) { let c = n; for (let k = 0; k < 8; k++) c = (c & 1) ? 0xEDB88320 ^ (c >>> 1) : c >>> 1; CRC_TABLE[n] = c; }
        }
        let c = start ^ 0xFFFFFFFF;
        for (let i = 0; i < items.length; i++) c = CRC_TABLE[(c ^ items[i]) & 0xFF] ^ (c >>> 8);
        return (c ^ 0xFFFFFFFF) >>> 0;
    };
    // `_zipp_crc.crc32(data, start=0)`, for zipfile (torch.save checkpoints).
    rt.defineModule("_zipp_crc", (g) => {
        g.set("crc32", rt.builtin("crc32", 2, (a) => BigInt(rt.crc32(a[0].items, a[1] === undefined ? 0 : Number(a[1]))), 1));
    });
})(__zipp_py);
