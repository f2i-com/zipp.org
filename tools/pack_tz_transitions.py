"""Lossless block encoding of the tz transitions, used by gen_tzdata.py (no external dependencies).

The transitions of all zones form one list, read in blocks of 32: a lookup
decodes at most one block. A transition is an instant (seconds since the epoch)
and the UTC offset in force after it, and the instant is stored as a day and a
time of day, because a zone's changes fall on a few times of day (02:00 local,
01:00 UTC, ...) whose days are a few months apart:

- TRANS_BLOCK_DAY, TRANS_BLOCK_TOD: the first transition of each block, as a
  day (floor(t / 86400)) and an index into TRANS_TOD. They are also what a
  search bisects before it reads one block.
- TRANS_TOD: every distinct UTC time of day (seconds), most frequent first.
- TRANS_BYTES: each later transition of a block, in order, as the days since
  the transition before it (one byte below 255; 255 and then a zigzag varint
  for the rest, which includes the step back where a block crosses from one
  zone into the next) and then its time of day: 0 when it is the time of the
  transition two before it in the same block (summer and winter alternate),
  else 1 + its TRANS_TOD index as a varint.
- TRANS_BLOCK_POS: where each block's second transition starts in TRANS_BYTES.
- TRANS_OFFSET: one byte per transition, its offset's index in TRANS_PALETTE
  (the distinct offsets, ascending), so an offset is read without decoding.
"""
from collections import Counter

BLOCK = 32
DAY = 86400


def _varint(data, n):
    while n >= 128:
        data.append((n & 127) | 128)
        n >>= 7
    data.append(n)


def encode_transitions(times, offsets):
    assert len(times) == len(offsets)
    palette = sorted(set(offsets))
    assert len(palette) <= 256, "widen TRANS_OFFSET before updating tzdb"
    pindex = {v: i for i, v in enumerate(palette)}
    tods = [t for t, _ in sorted(Counter(t % DAY for t in times).items(), key=lambda kv: (-kv[1], kv[0]))]
    tindex = {v: i for i, v in enumerate(tods)}
    assert len(tods) <= 65535
    data, block_day, block_tod, block_pos = [], [], [], []
    for i, time in enumerate(times):
        k = i % BLOCK
        if k == 0:
            block_day.append(time // DAY)
            block_tod.append(tindex[time % DAY])
            block_pos.append(len(data))
            continue
        days = time // DAY - times[i - 1] // DAY
        if 0 <= days < 255:
            data.append(days)
        else:
            data.append(255)
            _varint(data, days * 2 if days >= 0 else -days * 2 - 1)
        tod = time % DAY
        if k >= 2 and tod == times[i - 2] % DAY:
            data.append(0)
        else:
            _varint(data, tindex[tod] + 1)
    assert len(data) <= 65535, "widen TRANS_BLOCK_POS before updating tzdb"
    assert all(-(2**31) <= d < 2**31 for d in block_day)
    offset_index = [pindex[o] for o in offsets]
    assert decode_transitions(block_day, block_tod, block_pos, tods, data, len(times)) == list(times)
    return block_day, block_tod, block_pos, tods, palette, offset_index, data


def decode_transitions(block_day, block_tod, block_pos, tods, data, count):
    """The instants back from the encoded form, as tzdb.rs reads them."""
    out = []
    for block in range((count + BLOCK - 1) // BLOCK):
        pos, day = block_pos[block], block_day[block]
        tod = two_back = tods[block_tod[block]]
        out.append(day * DAY + tod)
        for _ in range(1, min(BLOCK, count - block * BLOCK)):
            b = data[pos]
            pos += 1
            if b == 255:
                n, shift = 0, 0
                while True:
                    c = data[pos]
                    pos += 1
                    n |= (c & 127) << shift
                    shift += 7
                    if c < 128:
                        break
                b = (n >> 1) ^ -(n & 1)
            day += b
            n, shift = 0, 0
            while True:
                c = data[pos]
                pos += 1
                n |= (c & 127) << shift
                shift += 7
                if c < 128:
                    break
            two_back, tod = tod, (two_back if n == 0 else tods[n - 1])
            out.append(day * DAY + tod)
    return out


def emit_packed_transitions(append, times, offsets):
    arrays = zip(
        ("TRANS_BLOCK_DAY", "TRANS_BLOCK_TOD", "TRANS_BLOCK_POS", "TRANS_TOD", "TRANS_PALETTE",
         "TRANS_OFFSET", "TRANS_BYTES"),
        ("i32", "u16", "u16", "u32", "i32", "u8", "u8"),
        encode_transitions(times, offsets),
    )
    append("// Blocks of 32 transitions: each block's first instant as a day and a time")
    append("// of day, the rest as day steps and times of day (see tzdb.rs and")
    append("// tools/pack_tz_transitions.py); one offset index per transition.")
    for name, ty, values in arrays:
        append(f"pub(crate) static {name}: &[{ty}] = &[")
        for i in range(0, len(values), 24):
            append("    " + " ".join(f"{n}," for n in values[i:i+24]))
        append("];")
    append("")
