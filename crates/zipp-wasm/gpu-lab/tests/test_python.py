import json
import math
from pathlib import Path
import shutil
import subprocess
import sys
import unittest
sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "zipp-vm" / "src" / "frontend" / "python" / "lib" / "shared"))
from zipp_gpu import Graph, GraphError, ComputeError
LAB = Path(__file__).resolve().parents[1]

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
        for shape in [(-1,), (0,), (True,), (1.5,), (65537,), (1,1,1,1,1), (4096,4096)]:
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


def run(**outputs):
    from zipp_gpu import execute_locally
    graph = next(iter(outputs.values()))._graph
    return {k: v["data"] for k, v in execute_locally(graph.program(**outputs))["outputs"].items()}


class GraphV2Tests(unittest.TestCase):
    """Graph IR v2 operations in the Python builder and its float32 reference."""

    def test_broadcasting_shapes_and_errors(self):
        g=Graph(); a=g.tensor([[1],[2],[3]]); b=g.tensor([10,20])
        self.assertEqual((a+b).shape,(3,2)); self.assertEqual((a/2).shape,(3,1)); self.assertEqual((1/b).shape,(2,))
        self.assertEqual(run(r=a*b-b)["r"],[0,0,10,20,20,40])
        self.assertEqual((a+g.tensor([1,2,3,4],shape=(4,1)).reshape(4)).shape,(3,4))
        with self.assertRaises(GraphError): a+g.tensor([1,2,3,4],shape=(2,2))
        with self.assertRaises(GraphError): g.tensor([1,2])+g.tensor([1,2,3])

    def test_unary_family(self):
        g=Graph(); x=g.tensor([-2,0,1,4])
        out=run(neg=-x, exp=x.exp(), relu=x.relu(), sig=x.sigmoid(), tanh=x.tanh(), gelu=x.gelu(), gg=x.gelu_grad(), sqrt=(x*x).sqrt())
        self.assertEqual(out["neg"],[2,-0.0,-1,-4]); self.assertEqual(out["relu"],[0,0,1,4]); self.assertEqual(out["sqrt"],[2,0,1,4])
        self.assertAlmostEqual(out["exp"][2],2.7182817459106445); self.assertEqual(out["sig"][1],0.5); self.assertEqual(out["tanh"][1],0)
        self.assertAlmostEqual(out["gelu"][2],0.8413447141647339,places=6); self.assertAlmostEqual(out["gg"][2],1.0833154916763306,places=6)
        self.assertEqual(run(r=g.tensor([1,4]).log())["r"][0],0)

    def test_reductions_softmax_and_views(self):
        g=Graph(); a=g.tensor([1,2,3,4,5,6],shape=(2,3))
        out=run(s0=a.sum(0), s1=a.sum(axis=-1,keepdim=True), m=a.mean(), sm=a.softmax(), ls=a.log_softmax(), p=a.permute(1,0), t=a.transpose(0,1), r=a.reshape(3,-1), T=a.T)
        self.assertEqual(out["s0"],[5,7,9]); self.assertEqual(out["s1"],[6,15]); self.assertEqual(out["m"],[3.5])
        self.assertAlmostEqual(sum(out["sm"][:3]),1.0,places=6); self.assertAlmostEqual(out["ls"][2],-0.40760597586631775,places=6)
        self.assertEqual(out["p"],[1,4,2,5,3,6]); self.assertEqual(out["t"],out["p"]); self.assertEqual(out["T"],out["p"]); self.assertEqual(out["r"],[1,2,3,4,5,6])
        self.assertEqual(a.sum(0,keepdim=True).shape,(1,3)); self.assertEqual(a.reshape(3,-1).shape,(3,2))
        for bad in [lambda: a.sum(2), lambda: a.softmax(0), lambda: a.permute(0,0), lambda: a.reshape(4,2), lambda: a.sum(keepdim=1)]:
            with self.assertRaises(GraphError): bad()

    def test_batched_matmul(self):
        g=Graph(); a=g.tensor(list(range(12)),shape=(2,2,3)); w=g.tensor([1,0,0,1,1,1],shape=(3,2))
        self.assertEqual((a@w).shape,(2,2,2)); self.assertEqual(run(r=a@w)["r"],[2,3,8,9,14,15,20,21])
        b=g.tensor(list(range(12)),shape=(2,3,2)); self.assertEqual(run(r=a@b)["r"],[10,13,28,40,172,193,244,274])
        with self.assertRaises(GraphError): a@g.tensor(list(range(18)),shape=(3,3,2))

    def test_cross_entropy_and_gradient(self):
        g=Graph(); logits=g.tensor([[0,0,0],[1,2,3]]); out=run(loss=logits.cross_entropy([2,0]), grad=logits.cross_entropy_grad([2,0]))
        expected=(math.log(3)+(math.log(math.exp(1)+math.exp(2)+math.exp(3))-1))/2
        self.assertAlmostEqual(out["loss"][0],expected,places=6)
        self.assertAlmostEqual(out["grad"][2],(1/3-1)/2,places=6); self.assertAlmostEqual(sum(out["grad"]),0,places=6)
        for bad in [[0,3],[0,0.5]]:
            with self.assertRaises(GraphError): logits.cross_entropy(bad)
        with self.assertRaises(GraphError): logits.cross_entropy(g.tensor([0,1])+0)

    def test_optimizer_steps_follow_torch_order(self):
        g=Graph(); p=g.tensor([1.0,-2.0]); grad=g.tensor([0.5,0.25])
        p1,m1,v1=g.adam(p,grad,g.zeros((2,)),g.zeros((2,)),lr=0.1,step=1)
        buf=g.momentum_update(g.tensor([1.0,1.0]),grad,0.9,dampening=0.5)
        out=run(p=p1,m=m1,v=v1,sgd=g.sgd_update(p,grad,0.1),buf=buf)
        self.assertAlmostEqual(out["p"][0],0.9,places=6); self.assertAlmostEqual(out["p"][1],-2.1,places=6)
        self.assertAlmostEqual(out["m"][0],0.05,places=7); self.assertAlmostEqual(out["v"][1],0.0000625,places=10)
        self.assertEqual(out["sgd"],[0.949999988079071,-2.0250000953674316]); self.assertEqual(out["buf"],[1.149999976158142,1.024999976158142])
        with self.assertRaises(GraphError): g.adam(p,grad,g.zeros((2,)),g.zeros((2,)),betas=(1.0,0.9))
        with self.assertRaises(GraphError): g.sgd_update(p,g.zeros((3,)),0.1)

    def test_single_graph_training_steps_reduce_the_loss(self):
        import random
        rnd=random.Random(3); xs=[[rnd.uniform(-0.2,0.2)+(1 if j%2==i%2 else 0) for j in range(4)] for i in range(8)]; ys=[i%2 for i in range(8)]
        params=[[[rnd.uniform(-0.5,0.5) for _ in range(6)] for _ in range(4)],[0.0]*6,[[rnd.uniform(-0.5,0.5) for _ in range(2)] for _ in range(6)],[0.0]*2]
        state=None; losses=[]
        for step in range(1,9):
            g=Graph(); x=g.tensor(xs); y=g.tensor(ys); W1,b1,W2,b2=[g.tensor(v) for v in params]
            z=x@W1+b1; h=z.tanh(); logits=h@W2+b2
            d=logits.cross_entropy_grad(y); dh=(d@W2.T)*(1-h*h)
            grads=[x.T@dh, dh.sum(0), h.T@d, d.sum(0)]
            ms=[g.zeros(t.shape) if state is None else g.tensor(state[0][i],shape=t.shape) for i,t in enumerate(grads)]
            vs=[g.zeros(t.shape) if state is None else g.tensor(state[1][i],shape=t.shape) for i,t in enumerate(grads)]
            new=[g.adam(p,gr,m,v,lr=0.1,step=step) for p,gr,m,v in zip([W1,b1,W2,b2],grads,ms,vs)]
            outs={"loss":logits.cross_entropy(y)}
            for i,(p,m,v) in enumerate(new): outs.update({"p%d"%i:p,"m%d"%i:m,"v%d"%i:v})
            seen=[]; g.submit(seen.append,**outs); out=seen[0]["outputs"]
            losses.append(out["loss"]["data"][0])
            shape=lambda t,data: [data[r*t.shape[1]:(r+1)*t.shape[1]] for r in range(t.shape[0])] if len(t.shape)==2 else data
            params=[shape(t,out["p%d"%i]["data"]) for i,t in enumerate([W1,b1,W2,b2])]
            state=([out["m%d"%i]["data"] for i in range(4)],[out["v%d"%i]["data"] for i in range(4)])
        self.assertLess(losses[-1],losses[0]*0.5,losses)

    def test_version_one_programs_keep_working(self):
        from zipp_gpu import execute_locally
        g=Graph(); a=g.tensor([1,2]); p=g.program(r=(a*3).relu())
        self.assertEqual(p["version"],1); self.assertEqual(execute_locally(dict(p,version=2))["outputs"]["r"]["data"],[3,6])
        with self.assertRaises(ComputeError): execute_locally({"version":3,"nodes":[],"outputs":[]})

    def test_the_program_version_follows_what_the_graph_actually_uses(self):
        """Version 1 while a version-1 host would read the graph the same way; 2 as soon as it would not."""
        def version(build):
            g=Graph(); return g.program(r=build(g))["version"]
        # Still version 1: the original ten operations, rank <= 2, scalar broadcast, whole-tensor sum.
        self.assertEqual(version(lambda g: (g.tensor([[1,2],[3,4]])@g.tensor([[1],[1]])).relu()),1)
        self.assertEqual(version(lambda g: (g.tensor([1,2])*g.tensor([3,4])+1).sum()),1)
        self.assertEqual(version(lambda g: g.tensor([[1,2],[3,4]]).transpose()),1)
        self.assertEqual(version(lambda g: g.tensor([[1,2],[3,4]]).positive()),1)
        # Version 2: a new operation, rank 3, real broadcasting, an axis or keepdim reduction,
        # a batched matmul, a permutation, a reshape or an optimizer step.
        for build in [lambda g: g.tensor([1.0,2.0]).exp(),
                      lambda g: g.tensor([1.0,2.0])/2,
                      lambda g: -g.tensor([1.0,2.0]),
                      lambda g: g.full((2,2,2),1.0).relu(),
                      lambda g: g.tensor([[1,2],[3,4]])+g.tensor([1,2]),
                      lambda g: g.tensor([[1,2],[3,4]]).sum(axis=0),
                      lambda g: g.tensor([1,2]).sum(keepdim=True),
                      lambda g: g.tensor([[1,2],[3,4]]).mean(),
                      lambda g: g.full((2,2,3),1.0)@g.full((2,3,2),1.0),
                      lambda g: g.tensor([[1,2],[3,4]]).permute(1,0),
                      lambda g: g.tensor([[1,2],[3,4]]).reshape(4),
                      lambda g: g.tensor([[1.0,2.0]]).softmax(),
                      lambda g: g.sgd_update(g.tensor([1.0]),g.tensor([1.0]),lr=0.1)]:
            self.assertEqual(version(build),2,build)

    @unittest.skipUnless(shutil.which("node"),"node is not installed")
    def test_python_reference_matches_the_javascript_backends(self):
        """The exact operations agree bit for bit with cpu-js and the WASM kernels; the rest to about one ulp."""
        g=Graph(); x=g.tensor([[0.3,-1.7,2.5],[4.0,0.1,-0.6]]); w=g.tensor([[0.5,-1.0],[0.25,2.0],[-0.75,0.125]]); t=g.tensor([1,0])
        exact={"mm":x@w,"bc":x-g.tensor([1.5,-2.0,0.25]),"div":x/g.tensor([[3.0],[7.0]]),"s":x.sum(0),"m":x.mean(-1,keepdim=True),
            "whole":x.mean(),"p":x.T,"sgd":g.sgd_update(x,x*x,0.01)}
        exact.update(zip(["pa","ma","va"],g.adam(x,x,x*0.1,x*x,lr=0.01,step=4)))
        close={"sm":x.softmax(),"ls":x.log_softmax(),"ce":(x@w).cross_entropy(t),"cg":(x@w).cross_entropy_grad(t),
            "gelu":x.gelu(),"gg":x.gelu_grad(),"tanh":x.tanh(),"sig":x.sigmoid(),"exp":x.exp()}
        program=g.program(**exact,**close)
        from zipp_gpu import execute_locally
        mine=execute_locally(program)["outputs"]
        script="""import {readFile} from 'node:fs/promises';import {createRuntime} from './src/runtime.mjs';
const program=JSON.parse(await new Promise(r=>{let s='';process.stdin.on('data',d=>s+=d);process.stdin.on('end',()=>r(s));}));
const wasmBytes=await readFile('./wasm/kernels.wasm');const out={};
for(const backend of ['cpu-js','wasm']){const rt=await createRuntime({backend,wasmBytes});out[backend]=(await rt.execute(program)).outputs;rt.dispose();}
console.log(JSON.stringify(out));"""
        result=subprocess.run(["node","--input-type=module","-e",script],input=json.dumps(program),capture_output=True,text=True,cwd=str(LAB),timeout=120)
        self.assertEqual(result.returncode,0,result.stderr)
        for backend,outs in json.loads(result.stdout).items():
            for name,value in mine.items():
                with self.subTest(backend=backend,output=name):
                    if name in exact: self.assertEqual(outs[name]["data"],value["data"])
                    else:
                        for a,b in zip(outs[name]["data"],value["data"]): self.assertLessEqual(abs(a-b),2.5e-7*max(1,abs(b)))

if __name__=="__main__":unittest.main(verbosity=2)
