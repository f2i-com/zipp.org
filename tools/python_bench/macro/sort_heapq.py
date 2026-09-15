"""Sorting and priority queues: `sorted` with keys, `list.sort`, a `heapq` k-way
merge, `heapq.nsmallest`/`nlargest` and `bisect.insort` over LCG-generated data."""
import bisect
import heapq
import sys
import time

SIZE = 6000


def make_data(size):
    state = 99
    data = []
    for i in range(size):
        state = (state * 1103515245 + 12345) % 2147483648
        data.append(state // 256 % 100000)
    return data


def bench(size):
    data = make_data(size)
    ascending = sorted(data)
    by_digits = sorted(data, key=lambda v: (v % 10, -v))
    records = [("r%d" % (v % 97), v) for v in data]
    records.sort(key=lambda rec: rec[0])
    chunks = [sorted(data[i:i + 1000]) for i in range(0, size, 1000)]
    heap = [(chunk[0], index, 0) for index, chunk in enumerate(chunks)]
    heapq.heapify(heap)
    merged = []
    while heap:
        value, index, pos = heapq.heappop(heap)
        merged.append(value)
        pos += 1
        if pos < len(chunks[index]):
            heapq.heappush(heap, (chunks[index][pos], index, pos))
    window = []
    for v in data[:3000]:
        bisect.insort(window, v)
    checksum = 0
    for i in range(0, size, 97):
        checksum += ascending[i] * (i + 1)
    return (
        merged == ascending,
        checksum,
        by_digits[0],
        by_digits[-1],
        records[0],
        records[-1],
        heapq.nsmallest(3, data),
        heapq.nlargest(3, data),
        window[1500],
    )


t0 = time.perf_counter()
result = bench(SIZE)
elapsed = time.perf_counter() - t0
print("sort_heapq", SIZE, result[0], result[1], result[2], result[3])
print("records", result[4], result[5])
print("extremes", result[6], result[7], result[8])
print("@bench-time %.6f" % elapsed, file=sys.stderr)
