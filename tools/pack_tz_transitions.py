"""Lossless block encoding used by gen_tzdata.py (no external dependencies)."""
BLOCK = 32

def encode_transitions(times, offsets):
    assert len(times) == len(offsets)
    palette = sorted(set(offsets))
    assert len(palette) <= 256, "widen the offset index before updating tzdb"
    indices = {v: i for i, v in enumerate(palette)}
    data, starts, positions = [], [], []
    for i, (time, offset) in enumerate(zip(times, offsets)):
        if i % BLOCK == 0:
            starts.append(time)
            positions.append(len(data))
        else:
            delta = time - times[i - 1]
            n = delta * 2 if delta >= 0 else -delta * 2 - 1
            while n >= 128:
                data.append((n & 127) | 128)
                n >>= 7
            data.append(n)
        data.append(indices[offset])
    return starts, positions, palette, data

def emit_packed_transitions(append, times, offsets):
    arrays = zip(("TRANS_BLOCK_AT", "TRANS_BLOCK_POS", "TRANS_PALETTE", "TRANS_BYTES"),
                 ("i64", "u32", "i32", "u8"), encode_transitions(times, offsets))
    append("// Lossless blocks of 32 transitions; signed delta varints and offset indices.")
    append("// The signed delta also covers a block crossing from one zone to the next.")
    for name, ty, values in arrays:
        append(f"pub(crate) static {name}: &[{ty}] = &[")
        for i in range(0, len(values), 24):
            append("    " + " ".join(f"{n}," for n in values[i:i+24]))
        append("];")
    append("")
