import json
from pathlib import Path
import sys
import unittest
sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "zipp-vm" / "src" / "frontend" / "python" / "lib" / "shared"))
from zipp_gpu import Graph, GraphError

class GraphTests(unittest.TestCase):
    def test_graph_operations_record_expected_nodes(self):
        g=Graph(); a=g.tensor([1,2]); b=g.tensor([3,4]); c=(a*b+4).relu()
        p=g.program(result=c,total=c.sum())
        self.assertEqual([n["op"] for n in p["nodes"]],["input","input","mul","input","add","relu","sum"])
        self.assertEqual(c.shape,(2,)); self.assertEqual(c.dtype,"float32")

    def test_gradient_kernels(self):
        from zipp_gpu import execute_locally
        g=Graph(); a=g.tensor([[-1,0,2],[3,-4,5]])
        out=execute_locally(g.program(result=a.transpose(), mask=a.positive()))
        self.assertEqual(out["outputs"]["result"]["shape"], [3,2])
        self.assertEqual(out["outputs"]["result"]["data"], [-1,3,0,-4,2,5])
        self.assertEqual(out["outputs"]["mask"]["data"], [0,0,1,1,0,1])
        with self.assertRaises(GraphError): g.tensor([1,2]).transpose()

    def test_matrix_multiply_shape(self):
        g=Graph(); a=g.tensor([[1,2,3],[4,5,6]]); b=g.tensor([[1],[2],[3]])
        self.assertEqual((a@b).shape,(2,1))

    def test_scalars_and_reverse_arithmetic(self):
        g=Graph(); a=g.tensor([1,2]); self.assertEqual((2-a).shape,(2,))
        self.assertEqual((3*a).shape,(2,)); self.assertEqual((1+a).shape,(2,))

    def test_invalid_data(self):
        for data in [[],[[1],[2,3]],[True],[float("inf")],[float("nan")],[1e100],["x"]]:
            with self.subTest(data=data),self.assertRaises(GraphError): Graph().tensor(data)

    def test_shape_limits(self):
        for shape in [(-1,), (0,), (True,), (1.5,), (4097,), (1,2,3)]:
            with self.subTest(shape=shape),self.assertRaises(GraphError):Graph().zeros(shape)

    def test_cross_graph_tensor_is_not_accepted(self):
        a=Graph().tensor([1]); b=Graph().tensor([2])
        with self.assertRaises(GraphError): _=a+b

    def test_symbolic_tensor_has_no_truth_value(self):
        with self.assertRaises(TypeError): bool(Graph().tensor([1]))

    def test_snapshot_is_owned(self):
        data=[1,2];g=Graph();a=g.tensor(data);data[0]=99;p=g.program(result=a);p["nodes"][0]["data"][0]=100
        self.assertEqual(g.program(result=a)["nodes"][0]["data"],[1,2])

    def test_empty_and_reserved_outputs(self):
        g=Graph();a=g.tensor([1])
        with self.assertRaises(GraphError):g.program()
        with self.assertRaises(GraphError):g.program(constructor=a)

    def test_json_is_roundtrippable(self):
        g=Graph();a=g.full((2,3),0.25)
        self.assertEqual(json.loads(g.to_json(result=a)),g.program(result=a))

    def test_submit_without_a_host_evaluates_locally(self):
        seen=[]
        g=Graph();a=g.tensor([1]);b=a*2+1
        g.submit(seen.append,result=b)
        self.assertEqual(len(seen),1);self.assertEqual(seen[0]["backend"],"cpu-python")
        self.assertEqual(seen[0]["outputs"]["result"]["data"],[3.0]);self.assertEqual(len(g.program(result=b)["nodes"]),5)

    def test_life_requires_matrix(self):
        with self.assertRaises(GraphError):Graph().tensor([1,0,1]).life()

if __name__=="__main__":unittest.main(verbosity=2)
