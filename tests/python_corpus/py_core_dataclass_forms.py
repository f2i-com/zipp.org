# Pending: dataclasses.astuple must recurse into dataclass instances nested in lists/dicts
# (asdict does; split out of tests/python_corpus/dataclass_forms.py).
from dataclasses import dataclass, field, astuple, asdict
@dataclass
class Item:
    name: str
    qty: int = 1
@dataclass
class Nested:
    items: list
    meta: dict = field(default_factory=dict)
    inner: Item = None
nd = Nested([Item("a"), Item("b", 2)], {"k": Item("m")}, Item("c"))
print("astuple-nested", astuple(nd))
print("asdict-nested", asdict(nd))
