"""JSON round trip: build nested records, `json.dumps` and `json.loads` them."""
import json
import sys
import time

RECORDS = 300
ROUNDS = 8


def make_records(count):
    records = []
    for i in range(count):
        records.append({
            "id": i,
            "name": "user%d" % i,
            "active": i % 3 != 0,
            # Never integral: zipp 0.0.18's json.dumps prints 1.0 as "1" (CPython: "1.0").
            "score": i * 0.25 + 0.125,
            "parent": None if i % 5 == 0 else i // 5,
            "tags": ["t%d" % (i % 7), "g%d" % (i % 11), "quote\"and\\slash"],
            "geo": {"x": i % 17, "y": -(i % 13), "label": "zone-%d" % (i % 4)},
        })
    return {"version": 2, "records": records, "empty": [], "meta": {"source": "bench"}}


def bench(count, rounds):
    doc = make_records(count)
    total_len = 0
    compact_len = 0
    same = 0
    for r in range(rounds):
        text = json.dumps(doc, sort_keys=True)
        back = json.loads(text)
        if back == doc:
            same += 1
        compact = json.dumps(back, separators=(",", ":"))
        total_len += len(text)
        compact_len += len(compact)
        doc = back
    pretty = json.dumps(doc["records"][3], indent=2, sort_keys=True)
    return total_len, compact_len, same, pretty


t0 = time.perf_counter()
result = bench(RECORDS, ROUNDS)
elapsed = time.perf_counter() - t0
print("json_roundtrip", RECORDS, ROUNDS, result[0], result[1], result[2])
print(result[3])
print("@bench-time %.6f" % elapsed, file=sys.stderr)
