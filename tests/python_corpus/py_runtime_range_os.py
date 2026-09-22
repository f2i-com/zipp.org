# range hashing agrees with range equality, range slices keep CPython's stop,
# and os.rmdir removes empty directories.
import os
print({range(0, 3, 2): "hit"}.get(range(0, 4, 2)), range(0, 3, 2) == range(0, 4, 2))
print(hash(range(0)) == hash(range(5, 5)), hash(range(3, 4)) == hash(range(3, 9, 7)), hash(range(1, 5)) == hash(range(1, 5, 1)))
print(len({range(0), range(4, 2), range(2, 3), range(2, 5, 9), range(0, 3), range(0, 3, 1)}))
print(range(10)[::-3], range(10)[1:8:3], range(0, 20, 2)[::-2], range(10)[5:1:-2], range(10)[8:2], range(10)[:-11:-1])
print(list(range(10)[::-3]), list(range(3, 30, 4)[-2:0:-3]), range(10, 0, -1)[2:9:2])
os.mkdir("zipp_rmdir_probe")
print(os.path.exists("zipp_rmdir_probe"), os.path.isdir("zipp_rmdir_probe"))
os.mkdir("zipp_rmdir_probe/inner")
try:
    os.rmdir("zipp_rmdir_probe")
except OSError as e:
    print("non-empty:", isinstance(e, OSError), type(e) in (OSError,))
os.rmdir("zipp_rmdir_probe/inner")
os.rmdir("zipp_rmdir_probe")
print(os.path.exists("zipp_rmdir_probe"))
try:
    os.rmdir("zipp_rmdir_probe")
except FileNotFoundError as e:
    print("missing:", type(e).__name__)
