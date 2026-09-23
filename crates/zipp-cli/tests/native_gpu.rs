//! `zipp py` on the native GPU (feature `gpu`). A Python program's semantics
//! must not depend on whether a GPU is present: every program here runs
//! twice through the real executable, on whatever adapter this machine has
//! and on the CPU evaluator (`ZIPP_GPU=0`), and the two outputs must be the
//! same but for the backend's name and float deviations the Torch guide
//! allows. The drivers in `tests/native_gpu/` are ordinary synchronous code
//! (callbacks run before `submit` returns) and assert their own agreement
//! with PyTorch 2.11.
//!
//! Where there is no hardware adapter (CI runners) the first run uses the
//! CPU evaluator too, so the fallback is exercised everywhere and the GPU
//! wherever one exists. `ZIPP_GPU_LOG` makes the executable name the adapter
//! when it starts one, which is how a run knows which to expect.
#![cfg(feature = "gpu")]
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../zipp-vm/tests/fixtures");
const DRIVERS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/native_gpu");

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Project(PathBuf);
impl Project {
    fn new(main: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "zipp-native-gpu-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("main.py"), main).unwrap();
        Project(path)
    }
    fn run(&self, env: &[(&str, &str)], args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_zipp"));
        command
            .current_dir(&self.0)
            .arg("py")
            .args(args)
            .arg("main.py");
        // ZIPP_GPU_BACKEND passes through: `ZIPP_GPU_BACKEND=metal` on a machine
        // without Metal runs these as a runner with no adapter would.
        command.env_remove("ZIPP_GPU");
        for (k, v) in env {
            command.env(k, v);
        }
        command.output().unwrap()
    }
}
impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn read(dir: &str, name: &str) -> String {
    std::fs::read_to_string(format!("{dir}/{name}")).unwrap()
}

/// The two runs of one program: stdout lines, and whether a GPU ran the first.
struct Runs {
    gpu: Vec<String>,
    hosted: bool,
    cpu: Vec<String>,
}

fn lines(output: &Output, what: &str) -> Vec<String> {
    assert!(
        output.status.success(),
        "{what} failed ({:?}):\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Whether a run's stderr (with `ZIPP_GPU_LOG`) says it started a GPU.
fn started_gpu(output: &Output) -> bool {
    String::from_utf8_lossy(&output.stderr)
        .lines()
        .any(|l| l.starts_with("zipp: GPU: ") && !l.contains("no hardware adapter"))
}

fn both(main: &str) -> Runs {
    let project = Project::new(main);
    let gpu = project.run(&[("ZIPP_GPU_LOG", "1")], &[]);
    let cpu = project.run(&[("ZIPP_GPU", "0")], &[]);
    Runs {
        hosted: started_gpu(&gpu),
        gpu: lines(&gpu, "GPU run"),
        cpu: lines(&cpu, "CPU run"),
    }
}

/// The backend a GPU run reports, or the fallback's.
fn expected_backend(hosted: bool) -> &'static str {
    if hosted {
        "webgpu"
    } else {
        eprintln!("no hardware GPU adapter: the first run exercised the CPU fallback");
        "cpu-python"
    }
}

/// The GPU run's lines equal the CPU run's, but for the backend's name and
/// numbers within `tolerance` (the printed deviations from PyTorch).
fn same_output(runs: &Runs, tolerance: f64) {
    let backend = expected_backend(runs.hosted);
    let gpu: Vec<String> = runs.gpu.iter().map(|l| l.replace(backend, "*")).collect();
    let cpu: Vec<String> = runs
        .cpu
        .iter()
        .map(|l| l.replace("cpu-python", "*"))
        .collect();
    assert_eq!(
        gpu.len(),
        cpu.len(),
        "GPU {:?}\nCPU {:?}",
        runs.gpu,
        runs.cpu
    );
    for (g, c) in gpu.iter().zip(&cpu) {
        let (gw, cw): (Vec<&str>, Vec<&str>) = (g.split(' ').collect(), c.split(' ').collect());
        assert_eq!(gw.len(), cw.len(), "\n{g}\n{c}");
        for (a, b) in gw.iter().zip(&cw) {
            match (a.parse::<f64>(), b.parse::<f64>()) {
                (Ok(x), Ok(y)) => assert!((x - y).abs() <= tolerance, "{x} vs {y}:\n{g}\n{c}"),
                _ => assert_eq!(a, b, "\n{g}\n{c}"),
            }
        }
    }
    // Each run names what it ran on.
    assert!(
        runs.gpu.iter().any(|l| l.contains(backend)),
        "{:?}",
        runs.gpu
    );
    assert!(
        runs.cpu.iter().any(|l| l.contains("cpu-python")),
        "{:?}",
        runs.cpu
    );
}

fn training() -> String {
    format!(
        "{}\nexpected_text = '''{}'''\n{}",
        read(FIXTURES, "torch_training.py"),
        read(FIXTURES, "torch_training_expected.json"),
        read(DRIVERS, "training.py")
    )
}

fn prepared(case: &str) -> String {
    format!(
        "case = {case:?}\n{}\nexpected = {}\nexpected = expected[case]\n{}",
        read(FIXTURES, "torch_prepared.py"),
        read(FIXTURES, "torch_prepared_expected.json"),
        read(DRIVERS, "prepared.py")
    )
}

fn family(fixture: &str, expected: &str, case: &str) -> String {
    format!(
        "case = {case:?}\n{}\nexpected = {}\nexpected = expected[case]\n{}",
        read(FIXTURES, fixture),
        read(FIXTURES, expected),
        read(DRIVERS, "families.py")
    )
}

fn check_prepared(case: &str, runs: &Runs) {
    same_output(runs, 2e-6);
    assert!(
        runs.gpu[0].starts_with(&format!("{case} losses 6 ")),
        "{:?}",
        runs.gpu
    );
}

fn check_family(case: &str, runs: &Runs) {
    // The driver asserts every deviation from PyTorch is below 1e-6; the two
    // booleans (prepared equals chained, bit for bit) hold on both.
    same_output(runs, 1e-6);
    assert!(
        runs.gpu[0].starts_with(&format!("{case} compiled ")),
        "{:?}",
        runs.gpu
    );
    assert!(
        runs.gpu[0].contains(" True True backend "),
        "{:?}",
        runs.gpu
    );
}

#[test]
fn training_steps_errors_and_inference_match_pytorch() {
    let runs = both(&training());
    same_output(&runs, 2e-6);
    assert!(
        runs.gpu[0].starts_with("training steps 5 "),
        "{:?}",
        runs.gpu
    );
    assert_eq!(
        runs.gpu[1],
        "non-finite readback ComputeError NUMBER model unchanged True"
    );
}

#[test]
fn prepared_sessions_match_pytorch() {
    for case in ["gelu_ce_adam", "sigmoid_ce_momentum"] {
        check_prepared(case, &both(&prepared(case)));
    }
}

#[test]
fn feature_mask_and_selection_families_match_pytorch() {
    for (fixture, expected, case) in [
        (
            "torch_gpu/features.py",
            "torch_gpu/features_expected.json",
            "layernorm",
        ),
        (
            "torch_gpu2/masks.py",
            "torch_gpu2/masks_expected.json",
            "edges",
        ),
        (
            "torch_gpu3/selection.py",
            "torch_gpu3/selection_expected.json",
            "transformer",
        ),
    ] {
        check_family(case, &both(&family(fixture, expected, case)));
    }
}

#[test]
fn dropout_random_feeds_and_failures_are_the_same_on_both_paths() {
    let runs = both(&read(DRIVERS, "dropout.py"));
    same_output(&runs, 0.0);
    assert_eq!(runs.gpu.len(), 13, "{:?}", runs.gpu);
    assert!(
        runs.gpu[0].contains("values [0.0, 2.0]") && runs.gpu[0].contains("grad matches mask True")
    );
    assert!(runs.gpu[2].contains("distinct True"));
    assert!(runs.gpu[4].starts_with("fed draws == eager True"));
    assert_eq!(runs.gpu[6], "non-finite step ComputeError NUMBER");
    assert_eq!(runs.gpu[7], "step after failure RuntimeError None");
    assert_eq!(runs.gpu[8], "non-finite first step ComputeError NUMBER");
    assert_eq!(runs.gpu[12], "done");
}

#[test]
fn zipp_gpu_list_graphs_and_sessions() {
    let runs = both(&read(DRIVERS, "lists.py"));
    same_output(&runs, 0.0);
    assert!(runs.cpu[0]
        .ends_with("[14.0, 44.0, 94.0, 164.0] [316.0] [19.0, 22.0, 43.0, 50.0] [2, 2] list"));
}

const SEMANTICS: &str = r#"import sys, time
import torch
w = torch.nn.Parameter(torch.tensor([1.0, 2.0]))
opt = torch.optim.SGD([w], lr=0.5)
def step(x):
    opt.zero_grad()
    loss = ((w * x) ** 2).sum()
    loss.backward()
    opt.step()
    return loss
compiled = torch.compile(step, training=True)
got = []
for _ in range(3):
    p = compiled(torch.tensor([1.0, 1.0]))
    p.submit(lambda loss: got.append(loss.item()))
    print("after submit", len(got))
print("losses", got, "weights", w.tolist())
prepared = compiled.prepare(torch.tensor([1.0, 1.0]))
prepared.sync(lambda s: print("sync before any step", s is prepared))
prepared.steps(lambda ls: print("steps", [l.item() for l in ls]), [(torch.tensor([1.0, 1.0]),)] * 2)
prepared.sync(lambda s: print("synced", w.tolist()))
prepared.dispose()
start = time.perf_counter()
time.sleep(0.05)
print("slept", time.perf_counter() - start >= 0.04)
print("adapter" in (p.stats or {}))
torch.compile(lambda x: x + 1)(torch.tensor([1.0])).submit(lambda y: sys.exit(3))
print("not reached")
"#;

#[test]
fn semantics_do_not_depend_on_the_gpu() {
    // Callbacks run before submit() returns, a plain loop of compiled calls
    // trains, sync() works anywhere, time.sleep blocks, and sys.exit in a
    // callback ends the program with its status. Only the stats differ: a
    // GPU result names its adapter.
    let project = Project::new(SEMANTICS);
    let gpu = project.run(&[("ZIPP_GPU_LOG", "1")], &[]);
    let cpu = project.run(&[("ZIPP_GPU", "0")], &[]);
    assert_eq!(
        gpu.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&gpu.stderr)
    );
    assert_eq!(
        cpu.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&cpu.stderr)
    );
    let text = |o: &Output| {
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let (g, c) = (text(&gpu), text(&cpu));
    assert_eq!(g.len(), c.len(), "GPU {g:?}\nCPU {c:?}");
    assert_eq!(g[..g.len() - 1], c[..c.len() - 1], "GPU {g:?}\nCPU {c:?}");
    assert_eq!(c[0], "after submit 1");
    assert_eq!(c[c.len() - 1], "False");
    assert_eq!(
        g[g.len() - 1],
        if started_gpu(&gpu) { "True" } else { "False" }
    );
}

#[test]
fn an_unhandled_failure_ends_the_run_the_same_way() {
    let project = Project::new(
        "import torch\nprint('before')\ntorch.compile(lambda x: torch.log(x))(torch.tensor([-1.0, 2.0])).submit(print)\nprint('not reached')\n",
    );
    let (gpu, cpu) = (
        project.run(&[], &[]),
        project.run(&[("ZIPP_GPU", "0")], &[]),
    );
    assert_eq!(cpu.status.code(), Some(1));
    assert_eq!(gpu.status.code(), Some(1));
    assert_eq!(gpu.stdout, cpu.stdout);
    assert_eq!(
        String::from_utf8_lossy(&gpu.stderr),
        String::from_utf8_lossy(&cpu.stderr)
    );
}

#[test]
fn opting_out_and_a_bad_backend_name_keep_the_cpu_evaluator() {
    let project = Project::new(
        "import torch\np = torch.compile(lambda x: x * 2.0)(torch.tensor([1.0, 2.0]))\np.submit(lambda y: print(p.backend, y.tolist()))\n",
    );
    let flag = project.run(&[], &["--no-gpu"]);
    assert_eq!(lines(&flag, "--no-gpu"), ["cpu-python [2.0, 4.0]"]);
    let off = project.run(&[("ZIPP_GPU", "off")], &[]);
    assert_eq!(lines(&off, "ZIPP_GPU=off"), ["cpu-python [2.0, 4.0]"]);
    let bad = project.run(&[("ZIPP_GPU_BACKEND", "cuda")], &[]);
    assert_eq!(
        lines(&bad, "ZIPP_GPU_BACKEND=cuda"),
        ["cpu-python [2.0, 4.0]"]
    );
    assert!(String::from_utf8_lossy(&bad.stderr).contains("ZIPP_GPU_BACKEND=cuda"));
}

#[test]
fn the_gpu_starts_only_when_a_graph_is_submitted() {
    // A program that imports torch but never compiles never opens the GPU.
    let project = Project::new("import torch\nprint(torch.tensor([1.0, 2.0]).sum().item())\n");
    let output = project.run(&[("ZIPP_GPU_LOG", "1")], &[]);
    assert_eq!(lines(&output, "no graph"), ["3.0"]);
    assert!(!String::from_utf8_lossy(&output.stderr).contains("zipp: GPU"));
}

#[test]
fn a_program_on_standard_input_uses_the_gpu_too() {
    use std::io::Write;
    let mut child = Command::new(env!("CARGO_BIN_EXE_zipp"))
        .args(["--lang=python", "-"])
        .env_remove("ZIPP_GPU")
        .env("ZIPP_GPU_LOG", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"import torch\np = torch.compile(lambda x: x * 2.0)(torch.tensor([1.0, 2.0]))\np.submit(lambda y: print(p.backend, y.tolist()))\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let backend = expected_backend(started_gpu(&output));
    assert_eq!(lines(&output, "stdin"), [format!("{backend} [2.0, 4.0]")]);
}

/// Every case of every fixture (30 programs, run twice each): slow in a
/// debug build, so opt-in with `--ignored`.
#[test]
#[ignore]
fn every_fixture_case_matches_pytorch() {
    for case in [
        "relu_mse_sgd",
        "gelu_ce_adam",
        "tanh_ce_adamw",
        "sigmoid_ce_momentum",
        "relu_mse_nesterov",
        "gelu_mse_adam",
    ] {
        check_prepared(case, &both(&prepared(case)));
    }
    let families: [(&str, &str, &[&str]); 3] = [
        (
            "torch_gpu/features.py",
            "torch_gpu/features_expected.json",
            &[
                "l2reg",
                "tied",
                "eagerops",
                "addparam",
                "detach",
                "shapes",
                "layernorm",
                "arith",
                "softmaxdim",
                "vector",
            ],
        ),
        (
            "torch_gpu2/masks.py",
            "torch_gpu2/masks_expected.json",
            &[
                "masks",
                "where",
                "clamp",
                "activations",
                "maxmin",
                "edges",
                "tensorclamp",
                "logical",
                "evaldropout",
            ],
        ),
        (
            "torch_gpu3/selection.py",
            "torch_gpu3/selection_expected.json",
            &["transformer", "gather", "cat", "strided", "params"],
        ),
    ];
    for (fixture, expected, cases) in families {
        for case in cases {
            check_family(case, &both(&family(fixture, expected, case)));
        }
    }
}
