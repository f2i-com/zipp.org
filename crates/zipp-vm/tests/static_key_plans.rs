//! Compiler-planned object-literal key semantics and runtime non-vacuity.
//! Child processes isolate the process-latched comparator and exercise the
//! interpreter, Tier C, GC stress, and the exact pre-experiment owned path.

const SOURCE: &str = r#"
  "use strict";
  function build(i) {
    return { 2:(i+2)|0, a:i, b:(i+1)|0, c:(i+3)|0 };
  }
  let sum=0, last=null;
  for(let i=0;i<1800;i++) {
    const o=build(i);
    sum=(sum+o.a+o.b+o.c+o[2])|0;
    if((i&63)===0) {
      o.a=(o.a+10)|0;                    // overwrite: no structural change
      o.extra=i;                         // materialize completed plan
      delete o.b;
      Object.defineProperty(o,"hidden",{value:i,enumerable:false});
      if((i&127)===0) Object.seal(o);
    }
    last=o;
  }

  const key="x";
  const dynamic={base:1,[key]:2};
  const duplicate={q:1,q:2};
  const accessor={get z(){return 7}};
  const spread={left:3,...{right:4}};
  const nullProto={__proto__:null,own:5};
  console.log("sum:"+sum);
  console.log("last:"+Object.keys(last).join(",")+":"+JSON.stringify(last));
  console.log("cold:"+(dynamic.x+duplicate.q+accessor.z+spread.right+nullProto.own)
    +":"+(Object.getPrototypeOf(nullProto)===null));
"#;

// `outer` is allocated and its first planned key is appended before `yield`.
// Calls made while the generator is parked force it through a collection, so
// it is old when the generator resumes.  The comma expression then allocates
// `inner` in the resumed frame and immediately commits it through the second
// AppendDataProp, with no intervening GC safe point.  In holder-grain remset
// mode a wrong/missing holder barrier is caught by NURSERY_VERIFY at the next
// minor, rather than being masked by recording only the incoming value.
const SUSPEND_SOURCE: &str = r#"
  "use strict";
  function trash(i) { return { n:i, next:(i+1)|0 }; }
  function* suspended() {
    return {
      anchor: 1,
      kept: (yield "pause", { marker:"alive" })
    };
  }

  let checksum=0;
  let verdict=[];
  for(let round=0;round<12;round++) {
    const it=suspended();
    const first=it.next();
    // Vary the number of stress safe points so the collection immediately
    // after the old->young store cannot always land on the periodic major.
    for(let i=0;i<5+(round&3);i++) checksum=(checksum+trash(i).next)|0;
    const done=it.next();
    trash(round); // the verifier observes the committed edge here
    verdict.push(first.value+":"+done.done+":"+done.value.kept.marker
      +":"+Object.keys(done.value).join(","));
  }
  console.log("suspend:"+verdict.join("|")+":"+(checksum>0));
"#;

fn run_source(source: &str) -> Vec<String> {
    let out = zipp_vm::run(source).expect("source compiles");
    assert!(out.error.is_none(), "runtime error: {:?}", out.error);
    out.output
}

#[test]
fn static_key_plans_child() {
    let Some(mode) = std::env::var("ZIPP_STATIC_KEY_CHILD").ok() else {
        return;
    };
    let source = if mode == "suspend-nursery" {
        SUSPEND_SOURCE
    } else {
        SOURCE
    };
    let output = run_source(source);
    let stats = zipp_vm::static_key_plan_stats();
    println!("static-key-result:{}", output.join("|"));
    println!(
        "static-key-stats:{},{},{},{}",
        stats.0, stats.1, stats.2, stats.3
    );

    if mode == "suspend-nursery" {
        assert!(
            stats.0 >= 12 * 3,
            "planned suspended/inner/trash allocations were vacuous: {stats:?}"
        );
        assert!(
            stats.1 >= 12 * 6,
            "planned append path was vacuous in suspended literals: {stats:?}"
        );
        assert_eq!(stats.3, 0, "nursery regression must stay interpreted");
        return;
    }

    if std::env::var_os("ZIPP_NO_STATIC_KEY_PLANS").is_some() {
        assert_eq!(
            stats,
            (0, 0, 0, 0),
            "the compiler-side comparator must retain no plans or helpers"
        );
    } else {
        assert!(
            stats.0 >= 1800,
            "planned allocation path was vacuous: {stats:?}"
        );
        assert!(
            stats.1 >= 1800 * 4,
            "planned appends did not cover the four-key hot literal: {stats:?}"
        );
        assert!(
            stats.2 > 20,
            "structural mutation never materialized: {stats:?}"
        );
    }
    if mode == "nojit" || mode == "off" || mode == "off-nojit" {
        assert_eq!(stats.3, 0, "interpreter child executed a Tier-C helper");
    } else {
        assert!(
            stats.3 > 100,
            "Tier C planned allocation was vacuous: {stats:?}"
        );
    }
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn child(mode: &str, vars: &[(&str, &str)]) -> (String, String) {
    let exe = std::env::current_exe().expect("test binary path");
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["static_key_plans_child", "--exact", "--nocapture"])
        .env("ZIPP_STATIC_KEY_CHILD", mode)
        .env("ZIPP_STATIC_KEY_STATS", "1")
        .env("ZIPP_JIT_THRESHOLD", "1")
        .env("ZIPP_JITLOG", "1");
    for key in [
        "ZIPP_NO_STATIC_KEY_PLANS",
        "ZIPP_NO_TIERC_PLANNED_APPEND_PROBE",
        "ZIPP_NOJIT",
        "ZIPP_GC_STRESS",
        "ZIPP_NURSERY_VERIFY",
        "ZIPP_NURSERY_YOUNG_BUDGET",
        "ZIPP_NO_NURSERY_ADAPT",
        "ZIPP_NO_NURSERY",
        "ZIPP_NO_VALGRAIN_REMSET",
        "ZIPP_NO_PRETENURE",
    ] {
        cmd.env_remove(key);
    }
    cmd.envs(vars.iter().copied());
    let out = cmd.output().expect("spawn static-key child");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{mode} failed:\n{stdout}\n{stderr}");
    let result = stdout
        .lines()
        .find_map(|line| line.strip_prefix("static-key-result:"))
        .unwrap_or_else(|| panic!("{mode} emitted no result marker:\n{stdout}\n{stderr}"))
        .to_owned();
    (result, stderr)
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[test]
fn planned_keys_match_owned_interpreter_jit_and_gc_paths() {
    let (jit, jit_log) = child("jit", &[]);
    assert!(
        jit_log.contains("Tier C fn") && jit_log.contains(" compiled"),
        "hot planned builder never compiled through Tier C:\n{jit_log}"
    );
    let (nojit, _) = child("nojit", &[("ZIPP_NOJIT", "1")]);
    let (gc, _) = child("gc", &[("ZIPP_GC_STRESS", "1")]);
    let (off, _) = child("off", &[("ZIPP_NO_STATIC_KEY_PLANS", "1")]);
    let (off_nojit, _) = child(
        "off-nojit",
        &[("ZIPP_NO_STATIC_KEY_PLANS", "1"), ("ZIPP_NOJIT", "1")],
    );
    assert_eq!(jit, nojit);
    assert_eq!(jit, gc);
    assert_eq!(jit, off);
    assert_eq!(jit, off_nojit);
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[test]
fn suspended_planned_literal_barriers_exact_old_holder_before_resume_append() {
    let (result, _) = child(
        "suspend-nursery",
        &[
            ("ZIPP_NOJIT", "1"),
            ("ZIPP_GC_STRESS", "1"),
            ("ZIPP_NURSERY_VERIFY", "1"),
            // Holder-grain mode proves the barrier dirties the literal itself;
            // a wrong holder cannot accidentally preserve only the value.
            ("ZIPP_NO_VALGRAIN_REMSET", "1"),
            ("ZIPP_NO_PRETENURE", "1"),
        ],
    );
    let one = "pause:true:alive:anchor,kept";
    assert_eq!(result, format!("suspend:{}:true", vec![one; 12].join("|")));
}

#[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
#[test]
fn planned_keys_match_interpreter_semantics_without_tier_c() {
    assert_eq!(
        run_source(SOURCE),
        vec![
            "sum:6487200".to_string(),
            "last:2,a,b,c:{\"2\":1801,\"a\":1799,\"b\":1800,\"c\":1802}".to_string(),
            "cold:20:true".to_string(),
        ]
    );
}
