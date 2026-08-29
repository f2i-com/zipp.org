//! Tier-C activation roots for native entries without an interpreter Frame.
//! Nested native/re-entrant calls must keep every suspended callable identity
//! alive, and a bounded root stack must decline before entering native code.

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn run_ok(source: &str) -> Vec<String> {
    let out = zipp_vm::run(source).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

// `detached` clears its only JS-visible callable edge, then invokes a compiled
// getter that allocates under GC stress. The outer method has no interpreter
// Frame while suspended, so its closure/cells survive only through the Tier-C
// activation-root stack.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
const DETACHED_SUSPENDED_GC_SOURCE: &str = r#"
  "use strict";
  function invoke(holder,n) {
    let x=(n+3)|0; x=Math.imul(x,3); x=(x+11)|0;
    x=(x^85)|0; x=(x-9)|0; x=Math.imul(x,5);
    x=x>>>1; x=(x<<1)|0;
    return (holder.random()+(x^x))|0;
  }
  // Keep one explicit native-to-native layer above `invoke`.  The enclosing
  // loop is intentionally too effectful for a native region, so call-routing
  // changes can otherwise leave `invoke` frame-backed and make the
  // activation-root probe vacuous.  These stateless wrappers are globals so
  // the inner call has a stable route; ZIPP_NO_TIERC_LEAF keeps it as a real
  // cross entry rather than a bytecode splice.
  function invokeCross(holder,n) {
    const value=invoke(holder,n);
    return (value+0)|0;
  }
  (function () {
    let getterRound=0;
    const warmTarget={};
    function allocatingGetter() {
        const round=getterRound++|0;
        let sum=0;
        for(let i=0;i<48;i++) {
          const value=(round+i)|0;
          const obj={value:value,text:"slot"+i};
          sum=(sum+value+(obj===warmTarget?1:0))|0;
        }
        return sum;
    }
    Object.defineProperty(warmTarget,"x",{get:allocatingGetter});
    let getterWarm=0;
    for(let i=0;i<96;i++) getterWarm=(getterWarm+warmTarget.x)|0;

    const target={x:0};
    // Once this factory returns, `holder.random` is the only JavaScript-visible
    // strong edge to `detached` before the method clears it.
    function makeHolder(seed, observed) {
      let p0=0,p1=1,p2=2,p3=3,p4=4,p5=5,p6=6,p7=7;
      let state=seed|0;
      const detached=function detached() {
        this.random=null;
        const garbage=observed.x;
        state ^= state << 13;
        state ^= state >>> 17;
        state ^= state << 5;
        state=(state+1+(garbage^garbage))|0;
        const pads=(p0+p1+p2+p3+p4+p5+p6+p7)|0;
        let x=(state+3)|0; x=Math.imul(x,3); x=(x+11)|0;
        x=(x^85)|0; x=(x-9)|0; x=Math.imul(x,5);
        x=x>>>1; x=(x<<1)|0;
        return (state+pads+(x^x))|0;
      };
      return {random:detached};
    }
    let warm=getterWarm^getterWarm;
    for(let i=0;i<1200;i++) {
      const holder=makeHolder((i*3+1)|0,target);
      warm=(warm+invokeCross(holder,(i&7)+1))|0;
      if(holder.random!==null) throw new Error("not detached during warmup");
    }
    Object.defineProperty(target,"x",{get:allocatingGetter});
    let sum=warm^warm;
    for(let i=0;i<1200;i++) {
      const holder=makeHolder((i*3+1)|0,target);
      sum=(sum+invokeCross(holder,(i&7)+1))|0;
      if(holder.random!==null) throw new Error("not detached");
    }
    console.log("detached:"+sum);
  })();
"#;

// 65 total activations of one same-proto closure body. The final native cross
// call reaches the activation-root stack cap while still below the generic
// native-call depth guard and must fall back to a real interpreter Frame.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
const DEEP_CAP_SOURCE: &str = r#"
  "use strict";
  (function () {
    function make(seed) {
      let state=seed|0;
      let next=null;
      function link(value) { next=value; }
      function run(depth) {
        state=(state+1)|0;
        let nested=0;
        if(depth>0) nested=next((depth-1)|0)|0;
        state=(state+(nested&1))|0;
        let x=(depth+3)|0; x=Math.imul(x,3); x=(x+11)|0;
        x=(x^85)|0; x=(x-9)|0; x=Math.imul(x,5);
        x=x>>>1; x=(x<<1)|0;
        return (state+(x^x))|0;
      }
      return [run,link];
    }
    const packs=[];
    for(let i=0;i<70;i++) packs.push(make((i*5+1)|0));
    for(let i=0;i<69;i++) packs[i][1](packs[i+1][0]);
    let warm=0;
    for(let i=0;i<200;i++) warm=(warm+packs[0][0](4))|0;
    const deep=packs[0][0](64);
    console.log("deep:"+warm+":"+deep);
  })();
"#;

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn child_result(case: &str, env: &[(&str, &str)]) -> (String, String) {
    let exe = std::env::current_exe().expect("test binary path");
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["tierc_activation_roots_child", "--exact", "--nocapture"])
        .env("ZIPP_TIERC_ACTIVATION_ROOT_CHILD", case)
        .env("ZIPP_ICSTATS", "1")
        .env("ZIPP_JITLOG", "1")
        .env("ZIPP_JIT_THRESHOLD", "1")
        // Keep the caller from replacing a native callee entry with leaf-spliced
        // bytecode: these tests specifically exercise nested activation roots.
        .env("ZIPP_NO_TIERC_LEAF", "1");
    for key in ["ZIPP_NOJIT", "ZIPP_NO_TIERC_UPVAL", "ZIPP_GC_STRESS"] {
        cmd.env_remove(key);
    }
    cmd.envs(env.iter().copied());
    let out = cmd.output().expect("spawn activation-root child");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "{case} child failed:\n{stdout}\n{stderr}"
    );
    let result = stdout
        .lines()
        .find_map(|line| line.strip_prefix("activation-root-result:"))
        .unwrap_or_else(|| panic!("{case} child emitted no result marker:\n{stdout}\n{stderr}"))
        .to_owned();
    (result, stderr)
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[test]
fn tierc_activation_roots_child() {
    let Some(case) = std::env::var("ZIPP_TIERC_ACTIVATION_ROOT_CHILD").ok() else {
        return;
    };
    let source = match case.as_str() {
        "detached" => DETACHED_SUSPENDED_GC_SOURCE,
        "deep" => DEEP_CAP_SOURCE,
        _ => panic!("unknown activation-root child case {case}"),
    };
    let output = run_ok(source);
    println!("activation-root-result:{}", output.join("|"));
    if case == "detached" && std::env::var_os("ZIPP_NOJIT").is_none() {
        let nested = zipp_vm::tierc_activation_root_stats();
        eprintln!("activation-root-stats nested_frame_free={nested}");
        assert!(
            nested > 1000,
            "detached method and nested allocator did not exercise suspended frame-free roots: {nested}"
        );
    }
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[test]
fn tierc_activation_roots_detached_method_survives_nested_allocating_getter_gc() {
    let env = &[("ZIPP_JIT_THRESHOLD", "32")];
    let (default, stderr) = child_result("detached", env);
    let detached_fid = stderr
        .lines()
        .find_map(|line| {
            line.strip_prefix("[jit] fn")
                .and_then(|rest| rest.split_once(" Tier-C upval-xorshift chains="))
                .map(|(fid, _)| fid)
        })
        .expect("detached body emitted no unique xorshift marker");
    assert!(
        stderr.contains(&format!("Tier C fn{detached_fid} compiled")),
        "detached method was not compiled through Tier C:\n{stderr}"
    );
    let mut compiled_fids: Vec<&str> = stderr
        .lines()
        .filter_map(|line| {
            line.strip_prefix("[jit] Tier C fn")
                .and_then(|rest| rest.split_once(" compiled"))
                .map(|(fid, _)| fid)
        })
        .collect();
    compiled_fids.sort_unstable();
    compiled_fids.dedup();
    assert!(
        compiled_fids.len() >= 3
            && stderr.contains("activation-root-stats nested_frame_free="),
        "detached method did not suspend across its compiled allocating getter: compiled={compiled_fids:?}\n{stderr}"
    );
    let (gc_stress, _) = child_result(
        "detached",
        &[("ZIPP_JIT_THRESHOLD", "32"), ("ZIPP_GC_STRESS", "1")],
    );
    let (interpreter, _) = child_result("detached", &[("ZIPP_NOJIT", "1")]);
    assert_eq!(default, gc_stress);
    assert_eq!(default, interpreter);
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[test]
fn tierc_activation_root_stack_cap_falls_back_before_native_entry() {
    let (default, stderr) = child_result("deep", &[("ZIPP_JIT_THRESHOLD", "32")]);
    assert!(
        stderr.contains("Tier-C activation-root stack full depth=62 frame_free=true"),
        "deep call did not exercise the frame-free fail-before-native root cap:\n{stderr}"
    );
    let (gc_stress, _) = child_result(
        "deep",
        &[("ZIPP_JIT_THRESHOLD", "32"), ("ZIPP_GC_STRESS", "1")],
    );
    let (interpreter, _) = child_result("deep", &[("ZIPP_NOJIT", "1")]);
    assert_eq!(default, gc_stress);
    assert_eq!(default, interpreter);
}
