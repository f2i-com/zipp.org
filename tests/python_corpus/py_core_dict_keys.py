# Pending divergences split out of tests/python_corpus/dict_keys.py.
# 1. dict keys/items views compare equal to sets with the same elements.
a = {"x": 1, "y": 2}
print("view-eq", a.keys() == {"x", "y"}, {"a": 1}.items() == {("a", 1)}, a.keys() != {"x"}, {"x", "y"} == a.keys(), a.keys() == frozenset("xy"), a.keys() == a.keys(), a.items() == a.items(), {"k": 1}.keys() == {"k": 2}.keys())
# 2. set intersection iterates the smaller operand, so equal-but-different-type elements come from it.
print("set-and", sorted({1, 2, 3} & {2.0, 3.0}), sorted({2.0, 3.0} & {1, 2, 3}), {1, 2, 3}.intersection([2.0]), sorted({True, 2} & {1, 2, 3, 4}))
