//! torch.fft checked against CPU PyTorch 2.11.
//! `fixtures/torch_fft/fft_cases.py` runs unchanged under both; `gen.py`
//! writes its PyTorch results to `fft_expected.json` and
//! `text_expected.txt`. Every transform (fft/ifft/rfft/irfft/hfft/ihfft and
//! their 2-D and N-D forms) is compared over lengths made of 2, 3 and 5
//! and over other primes (Bluestein), with each norm, n/s and dim, as are
//! the helpers, torch.stft/istft with the window functions, and the
//! gradients of real losses through the transforms (second order
//! included); errors, dtypes and grad_fn names as text.
#![cfg(feature = "python")]
use zipp_vm::frontend::compile_python_program;

fn run_with(source: &str, files: Vec<(String, Vec<u8>)>) -> Result<Vec<String>, String> {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            let modules = vec![("main".to_owned(), source)];
            let mut compiled = compile_python_program("main", &modules, &files, &[], false)?;
            let state = compiled.state_mut();
            state.set_limits(4_000_000_000, None);
            state.run_init()?;
            Ok(state.take_output())
        })
        .expect("spawn")
        .join()
        .expect("test thread")
}

fn run(source: &str) -> Result<Vec<String>, String> {
    run_with(source, Vec::new())
}

fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_owned).collect()
}

/// Each case's values agree to 1e-12 (float64) or 2e-6 (float32) of the
/// case's largest magnitude: Zipp transforms in doubles and rounds once,
/// PyTorch's pocketfft computes float32 transforms in float.
const COMPARE: &str = r#"
got = results()
bad = []
for name in sorted(expected):
    e = expected[name]
    if name not in got:
        bad.append("%s: missing" % name)
        continue
    g = got[name]
    if len(e) != len(g):
        bad.append("%s: %d values, expected %d" % (name, len(g), len(e)))
        continue
    tol = 2e-6 if name.startswith("f32") else 1e-12
    scale = max([1.0] + [abs(v) for v in e])
    worst = 0.0
    at = -1
    for i in range(len(e)):
        d = abs(g[i] - e[i]) / scale
        if d > worst:
            worst = d
            at = i
    if not worst <= tol:
        bad.append("%s: off by %.3g at %d (%r vs %r)" % (name, worst, at, g[at], e[at]))
extra = sorted(set(got) - set(expected))
print("compared", len(expected), bad, extra)
"#;

#[test]
fn fft_values_and_gradients_match_pytorch() {
    let source = format!(
        "{}\nimport json\nexpected = json.loads(r'''{}''')\n{}",
        include_str!("fixtures/torch_fft/fft_cases.py"),
        include_str!("fixtures/torch_fft/fft_expected.json"),
        COMPARE
    );
    let out = run(&source).unwrap();
    assert_eq!(out.len(), 1, "{out:#?}");
    assert!(out[0].starts_with("compared ") && out[0].ends_with(" [] []"), "{}", out[0]);
}

#[test]
fn fft_errors_dtypes_and_printing_match_pytorch() {
    let source = format!(
        "{}\nfor line in text():\n    print(line)\n",
        include_str!("fixtures/torch_fft/fft_cases.py")
    );
    let out = lines(&run(&source).unwrap().join("\n"));
    let expected = lines(include_str!("fixtures/torch_fft/text_expected.txt"));
    for (i, (g, e)) in out.iter().zip(&expected).enumerate() {
        assert_eq!(g, e, "line {i}");
    }
    assert_eq!(out.len(), expected.len(), "{out:#?}");
}
