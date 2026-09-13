"""Write a graph: python examples/vector.py > my-program.json"""
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "zipp-vm" / "src" / "frontend" / "python" / "lib" / "shared"))
from zipp_gpu import Graph

gpu = Graph()
a = gpu.tensor([1, 2, 3, 4])
b = gpu.tensor([10, 20, 30, 40])
c = (a * b + 4).relu()
print(gpu.to_json(result=c, total=c.sum()))
