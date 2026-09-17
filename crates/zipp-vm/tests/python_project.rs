//! Python projects: modules and imports, the built-in `ui` module, and the
//! host hooks an embedder drives through global slots.
#![cfg(feature = "python")]
use zipp_vm::embed::{HostValue, ScriptState};
use zipp_vm::frontend::{compile_python_project, CompiledSource};

fn project(entry: &str, files: &[(&str, &str)]) -> Result<CompiledSource, String> {
    let modules: Vec<(String, String)> = files
        .iter()
        .map(|(n, s)| (n.to_string(), s.to_string()))
        .collect();
    compile_python_project(entry, &modules)
}

/// Runs on the CLI's stack size: dunder dispatch re-enters the interpreter
/// natively, and debug frames are large.
fn big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(f)
        .expect("spawn")
        .join()
        .expect("test thread")
}

fn run_project(entry: &str, files: &[(&str, &str)]) -> Result<Vec<String>, String> {
    let entry = entry.to_owned();
    let files: Vec<(String, String)> = files
        .iter()
        .map(|(n, s)| (n.to_string(), s.to_string()))
        .collect();
    big_stack(move || {
        let mut compiled = compile_python_project(&entry, &files)?;
        let state = compiled.state_mut();
        state.set_limits(20_000_000, None);
        state.run_init()?;
        Ok(state.take_output())
    })
}

fn slot(state: &ScriptState, name: &str) -> u32 {
    state
        .symbols()
        .into_iter()
        .find(|s| s.name == name)
        .map(|s| s.index)
        .unwrap_or_else(|| panic!("runtime hook {name} has no global slot"))
}

fn s(v: &str) -> HostValue {
    HostValue::String(v.to_string())
}

fn n(v: f64) -> HostValue {
    HostValue::Number(v)
}

#[test]
fn modules_import_each_other_with_separate_namespaces() {
    let main = "import util\nfrom util import double, NAME as label\nimport util as u\nx = 1\n\
                print(util.double(20), double(4), label, u.x, x)\nutil.bump()\nutil.bump()\n\
                print(util.count, util.tally())\nprint(util)\n";
    let util = "x = 99\nNAME = 'util-name'\ncount = 0\ndef double(n):\n    return n * 2\n\
                def bump():\n    count = 1\n    return None\ndef tally():\n    return count\n\
                print('util loaded')\n";
    // `count` is read through the module's own namespace by `tally`, and a
    // function body's assignment is a local that never leaks anywhere.
    assert_eq!(
        run_project("main", &[("main", main), ("util", util)]).unwrap(),
        vec![
            "util loaded",
            "40 8 util-name 99 1",
            "0 0",
            "<module 'util' from 'util.py'>"
        ]
    );
}

#[test]
fn a_module_runs_once_and_cycles_resolve_partially_like_cpython() {
    let files = [
        ("main", "import a\nimport b\nprint(a.value, b.value)\n"),
        (
            "a",
            "print('a start')\nimport b\nvalue = 1\nprint('a end', b.value)\n",
        ),
        (
            "b",
            "print('b start')\nimport a\nvalue = 2\nprint('b end')\n",
        ),
    ];
    assert_eq!(
        run_project("main", &files).unwrap(),
        vec!["a start", "b start", "b end", "a end 2", "1 2"]
    );
    // Reading a name the partially initialised module has not bound yet is an
    // AttributeError, not a silent None.
    let files = [
        ("main", "import a\n"),
        ("a", "import b\nvalue = 1\n"),
        ("b", "import a\nprint(a.value)\n"),
    ];
    let err = run_project("main", &files).err().unwrap();
    assert!(err.contains("AttributeError"), "{err}");
}

#[test]
fn unknown_and_unsupported_imports_are_compile_errors() {
    for (source, needle) in [
        ("import nowhere\n", "No module named 'nowhere'"),
        ("from nowhere import x\n", "No module named 'nowhere'"),
        ("import a.b\n", "No module named 'a.b'"),
        (
            "def f():\n    from util import *\n",
            "import * only allowed at module level",
        ),
        ("from . import util\n", "relative"),
    ] {
        let err = project("main", &[("main", source), ("util", "x = 1\n")])
            .err()
            .unwrap();
        assert!(err.contains(needle), "{source:?}: {err}");
    }
    let err = project("main", &[("main", "x = 1\n"), ("bad-name", "")])
        .err()
        .unwrap();
    assert!(err.contains("not a valid module name"), "{err}");
    let err = project("app", &[("main", "x = 1\n")]).err().unwrap();
    assert!(err.contains("entry module"), "{err}");
    let err = project("main", &[("main", "x = 1\n"), ("main", "")])
        .err()
        .unwrap();
    assert!(err.contains("duplicate"), "{err}");
    // Diagnostics name the file (a module is compiled once something
    // imports it; an unrelated file with a syntax error costs nothing).
    assert!(project("main", &[("main", "x = 1\n"), ("util", "x = = 2\n")]).is_ok());
    let err = project("main", &[("main", "import util\n"), ("util", "x = = 2\n")])
        .err()
        .unwrap();
    assert!(err.contains("util.py:1:5"), "{err}");
    // A module may import from another and from the stdlib inside a function.
    assert_eq!(
        run_project(
            "main",
            &[
                (
                    "main",
                    "def f():\n    import util\n    from math import sqrt\n    return util.x + sqrt(16)\nprint(f())\n"
                ),
                ("util", "x = 1\n")
            ]
        )
        .unwrap(),
        vec!["5.0"]
    );
}

#[test]
fn ui_module_records_commands_and_host_hooks_call_the_entry() {
    big_stack(ui_module_body);
}

fn ui_module_body() {
    let source = "import ui\nfrom ui import rect\n\nhits = [0]\n\n\
                  def draw():\n    ui.clear('#000')\n    rect(1, 2, 3, 4, 'red')\n    \
                  ui.text(5, 6, 'hi ' + str(len(hits)), 'white')\n    return len(hits)\n\n\
                  def add(a, b):\n    hits[0] = hits[0] + 1\n    return [a + b, 'ok', True, None]\n\n\
                  def on_click(x, y):\n    return ui.button(0, 0, 10, 10, 'go')\n\n\
                  print(ui.mouse(), ui.width(), ui.key('ArrowUp'))\nui.circle(9, 9, 2, 'blue')\n";
    let mut compiled = project("main", &[("main", source)]).unwrap();
    let state = compiled.state_mut();
    state.set_limits(20_000_000, None);
    state.run_init().unwrap();
    assert_eq!(state.take_output(), vec!["(0, 0, False) 640 False"]);
    let (has, call, take_ui, set_input) = (
        slot(state, "__zipp_py_has"),
        slot(state, "__zipp_py_call"),
        slot(state, "__zipp_py_take_ui"),
        slot(state, "__zipp_py_set_input"),
    );
    // Top-level commands are buffered until the host drains them.
    let drained = state.call_slot(take_ui, &[]).unwrap();
    assert_eq!(
        drained,
        HostValue::Array(vec![HostValue::Array(vec![
            s("circle"),
            n(9.0),
            n(9.0),
            n(2.0),
            s("blue")
        ])])
    );
    assert_eq!(
        state.call_slot(take_ui, &[]).unwrap(),
        HostValue::Array(vec![])
    );
    assert_eq!(
        state.call_slot(has, &[s("draw")]).unwrap(),
        HostValue::Bool(true)
    );
    assert_eq!(
        state.call_slot(has, &[s("update")]).unwrap(),
        HostValue::Bool(false)
    );
    assert_eq!(
        state.call_slot(has, &[s("hits")]).unwrap(),
        HostValue::Bool(false)
    );
    // Host numbers arrive as Python ints and results cross back as host data.
    let result = state
        .call_slot(call, &[s("add"), HostValue::Array(vec![n(2.0), n(40.0)])])
        .unwrap();
    assert_eq!(
        result,
        HostValue::Array(vec![
            n(42.0),
            s("ok"),
            HostValue::Bool(true),
            HostValue::Null
        ])
    );
    assert_eq!(
        state
            .call_slot(call, &[s("draw"), HostValue::Array(vec![])])
            .unwrap(),
        n(1.0)
    );
    let frame = state.call_slot(take_ui, &[]).unwrap();
    let HostValue::Array(commands) = frame else {
        panic!("{frame:?}")
    };
    assert_eq!(commands.len(), 3);
    assert_eq!(
        commands[2],
        HostValue::Array(vec![s("text"), n(5.0), n(6.0), s("hi 1"), s("white")])
    );
    // Input drives `button`, `mouse` and `key`: a click inside the button rect.
    let input = "{\"mx\":4,\"my\":5,\"down\":true,\"clicked\":true,\
                 \"keys\":{\"ArrowUp\":true},\"w\":320,\"h\":200}";
    state.call_slot(set_input, &[s(input)]).unwrap();
    let clicked = state
        .call_slot(
            call,
            &[s("on_click"), HostValue::Array(vec![n(4.0), n(5.0)])],
        )
        .unwrap();
    assert_eq!(clicked, HostValue::Bool(true));
    // Errors from the hooks are ordinary throws with Python error names.
    let err = state
        .call_slot(call, &[s("missing"), HostValue::Array(vec![])])
        .err()
        .unwrap();
    assert!(err.contains("NameError"), "{err}");
    // Host floats arrive as Python floats.
    assert_eq!(
        state
            .call_slot(call, &[s("add"), HostValue::Array(vec![n(1.5), n(1.0)])])
            .unwrap(),
        HostValue::Array(vec![
            n(2.5),
            s("ok"),
            HostValue::Bool(true),
            HostValue::Null
        ])
    );
    let err = state
        .call_slot(call, &[s("add"), HostValue::Array(vec![n(1.0)])])
        .err()
        .unwrap();
    assert!(err.contains("positional argument"), "{err}");
}

#[test]
fn guest_python_cannot_reach_the_host_hooks_or_other_modules_privately() {
    for source in [
        "print(__zipp_py_ui)",
        "print(__zipp_py_call)",
        "__zipp_py_take_ui()",
        "print(util.x)",
        "print(main.x)",
    ] {
        assert!(
            run_project("main", &[("main", source), ("util", "x = 1\n")]).is_err(),
            "{source}"
        );
    }
}

#[test]
fn new_builtins_and_list_pop() {
    assert_eq!(
        run_project(
            "main",
            &[(
                "main",
                "a = [1, 2, 3]\nprint(a.pop(), a, min(3, 9), max('b', 'a'), int('42') + int(True))\n"
            )]
        )
        .unwrap(),
        vec!["3 [1, 2] 3 b 43"]
    );
    assert!(run_project("main", &[("main", "[].pop()")])
        .err()
        .unwrap()
        .contains("IndexError"));
    assert!(run_project("main", &[("main", "int('x')")])
        .err()
        .unwrap()
        .contains("ValueError"));
}

/// `compile_python_program`: packages by dotted name, the virtual filesystem
/// and `sys.argv`, as the CLI and the playground hand a folder over.
fn run_program(
    entry: &str,
    modules: &[(&str, &str)],
    files: &[(&str, &[u8])],
    argv: &[&str],
) -> Result<Vec<String>, String> {
    use zipp_vm::frontend::compile_python_program;
    let entry = entry.to_owned();
    let modules: Vec<(String, String)> = modules
        .iter()
        .map(|(n, s)| (n.to_string(), s.to_string()))
        .collect();
    let files: Vec<(String, Vec<u8>)> = files
        .iter()
        .map(|(n, b)| (n.to_string(), b.to_vec()))
        .collect();
    let argv: Vec<String> = argv.iter().map(|a| a.to_string()).collect();
    big_stack(move || {
        let mut compiled = compile_python_program(&entry, &modules, &files, &argv, false)?;
        let state = compiled.state_mut();
        state.set_limits(50_000_000, None);
        state.run_init()?;
        Ok(state.take_output())
    })
}

#[test]
fn packages_files_and_argv_reach_the_program() {
    let main = "import sys, os, json\nfrom legacy.fast_memory import Memory\nimport legacy\n\
                from pkg import helper\nimport pkg.sub.deep as deep\nimport pkg\n\
                print(sys.argv)\nprint(Memory().name, legacy.fast_memory.Memory is Memory)\n\
                print(helper.answer(), deep.LEVEL, pkg.sub.deep.LEVEL)\n\
                print(sorted(os.listdir('data')), os.path.exists('data/cfg.json'), os.path.isdir('legacy'))\n\
                with open('data/cfg.json') as f:\n    cfg = json.load(f)\nprint(cfg['steps'] * 2)\n\
                with open('data/blob.bin', 'rb') as f:\n    blob = f.read()\nprint(len(blob), blob[:2], blob[-1])\n\
                with open('out/result.txt', 'w') as f:\n    f.write('done ' + sys.argv[2])\n\
                print(open('out/result.txt').read())\n";
    let modules = [
        ("main", main),
        ("legacy.fast_memory", "class Memory:\n    name = 'fast'\n"),
        ("pkg", "print('pkg init')\n"),
        ("pkg.helper", "def answer():\n    return 42\n"),
        ("pkg.sub.deep", "LEVEL = 3\n"),
        ("unused", "raise RuntimeError('never compiled')\n"),
    ];
    let blob: Vec<u8> = (0..=255u8).collect();
    let files: [(&str, &[u8]); 4] = [
        ("data/cfg.json", b"{\"steps\": 21}"),
        ("data/blob.bin", &blob),
        ("legacy/fast_memory.py", b"class Memory:\n    name = 'fast'\n"),
        ("main.py", main.as_bytes()),
    ];
    let out = run_program("main", &modules, &files, &["--steps", "7"]).unwrap();
    assert_eq!(
        out,
        vec![
            "pkg init",
            "['main.py', '--steps', '7']",
            "fast True",
            "42 3 3",
            "['blob.bin', 'cfg.json'] True True",
            "42",
            "256 b'\\x00\\x01' 255",
            "done 7",
        ]
    );
    // A missing file is a FileNotFoundError, and a folder is not a file.
    let err = run_program("main", &[("main", "open('nope.txt')\n")], &[], &[])
        .err()
        .unwrap();
    assert!(err.contains("FileNotFoundError"), "{err}");
    let err = run_program(
        "main",
        &[("main", "open('data')\n")],
        &[("data/x.txt", b"x")],
        &[],
    )
    .err()
    .unwrap();
    assert!(err.contains("IsADirectoryError"), "{err}");
}

#[test]
fn a_test_module_entry_runs_its_tests() {
    let source = "import pytest\n\ndef test_ok():\n    assert 1 + 1 == 2\n\n\
                  @pytest.mark.parametrize('n', [1, 2])\ndef test_param(n):\n    assert n > 0\n";
    let out = run_program("test_lab", &[("test_lab", source)], &[], &[]).unwrap();
    assert!(out.iter().any(|l| l.contains("3 passed")), "{out:?}");
    let failing = "def test_bad():\n    assert 1 == 2\n";
    let err = run_program("test_lab", &[("test_lab", failing)], &[], &[])
        .err()
        .unwrap();
    assert!(err.contains("SystemExit"), "{err}");
}

/// Relative imports, which a ported package is written with. A module that has
/// submodules counts from itself; anything else counts from its parent, and
/// each extra dot climbs one more level -- CPython's rule.
#[test]
fn relative_imports_resolve_against_the_current_package() {
    let out = run_project(
        "main",
        &[
            ("main", "import pkg.sub.leaf
print(pkg.sub.leaf.report())
"),
            ("pkg", "VALUE = 'package'
"),
            ("pkg.shared", "NAME = 'shared'
"),
            ("pkg.sub", "DEPTH = 'sub'
"),
            (
                "pkg.sub.leaf",
                concat!(
                    // one dot: this module's package, pkg.sub
                    "from .  import sibling
",
                    "from . import DEPTH
",
                    // two dots: pkg
                    "from ..shared import NAME
",
                    "from .. import VALUE
",
                    "def report():
",
                    "    return ' '.join([sibling.WHO, DEPTH, NAME, VALUE])
",
                ),
            ),
            ("pkg.sub.sibling", "WHO = 'sibling'
"),
        ],
    )
    .expect("run");
    assert_eq!(out, vec!["sibling sub shared package"]);
}

/// A package's own `__init__` counts from itself, not from its parent.
#[test]
fn a_package_module_counts_relative_imports_from_itself() {
    let out = run_project(
        "main",
        &["main", "import pkg
print(pkg.HELLO)
"]
            .chunks(2)
            .map(|c| (c[0], c[1]))
            .collect::<Vec<_>>()
            .as_slice()
            .iter()
            .copied()
            .chain([("pkg", "from .inner import HELLO
"), ("pkg.inner", "HELLO = 'from inner'
")])
            .collect::<Vec<_>>()
            .as_slice(),
    )
    .expect("run");
    assert_eq!(out, vec!["from inner"]);
}

/// Climbing past the top of the project is an error, not a silent miss.
#[test]
fn a_relative_import_beyond_the_top_level_is_refused() {
    let error = run_project(
        "main",
        &[
            ("main", "import pkg.leaf
"),
            ("pkg", "
"),
            ("pkg.leaf", "from ... import nothing
"),
        ],
    )
    .expect_err("should refuse");
    assert!(error.contains("beyond top-level package"), "{error}");
}

/// A module with no package above it cannot climb at all.
#[test]
fn a_relative_import_with_no_parent_package_is_refused() {
    let error = run_project("main", &[("main", "from . import anything
")])
        .expect_err("should refuse");
    assert!(error.contains("no known parent package"), "{error}");
}
