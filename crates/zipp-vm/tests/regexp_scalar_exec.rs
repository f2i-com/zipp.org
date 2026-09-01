//! Exact MEM-only non-global exec scalarization: mechanism, publication,
//! pin/re-entry closure, exclusions, GC/JIT modes, and kill switches.

//! Pins x86-64 JIT mechanisms from the engine's logs and counters, which the interpreter-only profiles never emit; compiled only where that tier exists, like the other tier-pinning suites.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

use std::process::Command;

const HOT: usize = 64;

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}\nsource:\n{src}",
        out.error
    );
    out.output
}

fn node_output(src: &str) -> Vec<String> {
    let out = Command::new("node")
        .arg("-e")
        .arg(src)
        .output()
        .expect("node on PATH");
    assert!(
        out.status.success(),
        "node failed: {}\nsource:\n{src}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

fn assert_matches_node(src: &str) {
    assert_eq!(run_ok(src), node_output(src), "zipp != node for:\n{src}");
}

fn core_src(n: usize) -> String {
    format!(
        r#"var N={n},lines=new Array(N);
for(var j=0;j<N;j++)lines[j]="pre "+(j%200)+".02.003.4 post";
var reIp=/(\d{{1,3}})\.(\d{{1,3}})\.(\d{{1,3}})\.(\d{{1,3}})/;
reIp.lastIndex=-0;
var octSum=0,ipMatches=0,m;
for(var i=0;i<N;i++){{
  m=reIp.exec(lines[i]);
  if(m){{ipMatches++;octSum=(octSum+(+m[1])+(+m[2])+(+m[3])+(+m[4]))|0;}}
}}
console.log(ipMatches+"|"+octSum+"|"+Object.is(reIp.lastIndex,-0));
console.log(m[0]+"|"+m[1]+"|"+m[2]+"|"+m[3]+"|"+m[4]+"|"+
  m.index+"|"+m.input+"|"+m.groups+"|"+Object.keys(m).join(","));
m[1]="changed";m.extra=9;
console.log(m[1]+"|"+m.extra+"|"+JSON.stringify(m));
var md=Object.getOwnPropertyDescriptor(m,"index");
console.log(md.value+"|"+md.writable+"|"+md.enumerable+"|"+md.configurable);
console.log(RegExp.input+"|"+RegExp.lastMatch+"|"+RegExp.lastParen+"|"+
  RegExp.leftContext+"|"+RegExp.rightContext+"|"+RegExp.$1+"|"+RegExp.$4);"#,
    )
}

fn final_miss_src(n: usize) -> String {
    format!(
        r#"var N={n},lines=new Array(N);
for(var j=0;j<N;j++)lines[j]="1.2.3.4";
lines[N-1]="no address";
var reIp=/(\d+).(\d+).(\d+).(\d+)/,octSum=0,ipMatches=0,m;
for(var i=0;i<N;i++){{m=reIp.exec(lines[i]);if(m){{
  ipMatches++;octSum=(octSum+(+m[1])+(+m[2])+(+m[3])+(+m[4]))|0;
}}}}
console.log((m===null)+"|"+ipMatches+"|"+octSum+"|"+RegExp.lastMatch+"|"+RegExp.$4);"#,
    )
}

fn hole_src(n: usize) -> String {
    format!(
        r#"var N={n},lines=new Array(N);
for(var j=0;j<N;j++)if(j!==20)lines[j]=j+".2.3.4";
var reIp=/(\d+).(\d+).(\d+).(\d+)/,octSum=0,ipMatches=0,m,seen="";
Object.defineProperty(Array.prototype,"20",{{configurable:true,get:function(){{
  seen=m[0]+"|"+m[1]+"|"+m.index;return "20.2.3.4";
}}}});
for(var i=0;i<N;i++){{m=reIp.exec(lines[i]);if(m){{
  ipMatches++;octSum=(octSum+(+m[1])+(+m[2])+(+m[3])+(+m[4]))|0;
}}}}
delete Array.prototype[20];
console.log(seen+"|"+ipMatches+"|"+octSum+"|"+m[0]);"#,
    )
}

fn slow_add_src() -> &'static str {
    r#"var N=48,lines=new Array(N);
for(var j=0;j<N;j++)lines[j]="1.2.3.4";
var reIp=/(\d+).(\d+).(\d+).(\d+)/,octSum=0,ipMatches=0,m,i=0;
function scan(){for(i=0;i<N;i++){m=reIp.exec(lines[i]);if(m){
  ipMatches++;octSum=(octSum+(+m[1])+(+m[2])+(+m[3])+(+m[4]))|0;
}}}
scan();
N=2;lines=["no address","9.8.7.6"];i=0;ipMatches=0;var seen="",caught="";
octSum={valueOf:function(){
  seen=m[0]+"|"+m[1]+"|"+m[2]+"|"+m[3]+"|"+m[4]+"|"+m.index+"|"+Object.keys(m).join(",");
  throw new Error("boom");
}};
try{scan();}catch(e){caught=e.message;}
console.log(seen+"|"+caught+"|"+ipMatches);"#
}

fn alias_src(n: usize) -> String {
    // `m` is both the skipped result binding and the count binding. The exact
    // bytecode shape remains hot, but suppressing StoreGlobal(m) would make the
    // following m++ and capture loads observe the previous result.
    format!(
        r#"var N={n},lines=new Array(N);
for(var j=0;j<N;j++)lines[j]="1.2.3.4";
var reIp=/(\d+).(\d+).(\d+).(\d+)/,octSum=0,m=0;
for(var i=0;i<N;i++){{m=reIp.exec(lines[i]);if(m){{
  m++;octSum=(octSum+(+m[1])+(+m[2])+(+m[3])+(+m[4]))|0;
}}}}
console.log(m+"|"+octSum+"|"+i);"#,
    )
}

fn captured_exec_accessor_src(n: usize) -> String {
    format!(
        r#"var N={n},lines=new Array(N);
for(var j=0;j<N;j++)lines[j]="1.2.3.4";
var reIp=/(\d+).(\d+).(\d+).(\d+)/,octSum=0,ipMatches=0,m,i=0;
function scan(){{for(i=0;i<N;i++){{m=reIp.exec(lines[i]);if(m){{
  ipMatches++;octSum=(octSum+(+m[1])+(+m[2])+(+m[3])+(+m[4]))|0;
}}}}}}
scan();
i=0;octSum=0;ipMatches=0;var gets=0,calls=0;
var custom=function(){{calls++;return ["custom","10","20","30","40"];}};
Object.defineProperty(reIp,"exec",{{configurable:true,get:function(){{
  gets++;delete reIp.exec;return custom;
}}}});
scan();
console.log(ipMatches+"|"+octSum+"|"+gets+"|"+calls+"|"+m[0]);"#,
    )
}

#[test]
fn pristine_final_result_lastindex_statics_and_mutation_match_node() {
    assert_matches_node(&core_src(HOT));
}

#[test]
fn semantic_miss_elides_previous_pending_but_preserves_last_success_statics() {
    assert_matches_node(&final_miss_src(HOT));
}

#[test]
fn dense_pin_hole_flushes_before_inherited_getter() {
    assert_matches_node(&hole_src(HOT));
}

#[test]
fn slow_add_flushes_before_valueof_throw() {
    assert_matches_node(slow_add_src());
}

#[test]
fn optional_captures_shadow_array_prototype_and_use_exact_unary_plus() {
    let src = r#"var N=48,lines=new Array(N);
for(var j=0;j<N;j++)lines[j]=(j&1)?"a":"abcd";
var reIp=/(a)(b)?(c)?(d)?/,octSum=0,ipMatches=0,m,gets=0;
for(var k=1;k<=4;k++)Object.defineProperty(Array.prototype,String(k),{
  configurable:true,get:function(){gets++;return "99";}
});
for(var i=0;i<N;i++){m=reIp.exec(lines[i]);if(m){
  ipMatches++;octSum=(octSum+(+m[1])+(+m[2])+(+m[3])+(+m[4]))|0;
}}
for(var k=1;k<=4;k++)delete Array.prototype[k];
console.log(ipMatches+"|"+octSum+"|"+gets+"|"+String(m[2])+"|"+Object.keys(m).join(","));"#;
    assert_matches_node(src);
}

#[test]
fn capture_string_numeric_grammar_is_shared_with_unary_plus() {
    let src = r#"var N=48,lines=new Array(N);
for(var j=0;j<N;j++)lines[j]="0x10|0b11|1e2|-0";
var reIp=/(0x10)\|(0b11)\|(1e2)\|(-0)/,octSum=0,ipMatches=0,m;
for(var i=0;i<N;i++){m=reIp.exec(lines[i]);if(m){
  ipMatches++;octSum=(octSum+(+m[1])+(+m[2])+(+m[3])+(+m[4]))|0;
}}
console.log(ipMatches+"|"+octSum+"|"+Object.is(+m[4],-0)+"|"+m.join(","));"#;
    assert_matches_node(src);
}

#[test]
fn protocol_mutations_and_excluded_regex_or_input_shapes_match_node() {
    let src = r#"var N=32,lines,reIp,octSum,ipMatches,m,i,out=[];
function scan(){for(i=0;i<N;i++){m=reIp.exec(lines[i]);if(m){
  ipMatches++;octSum=(octSum+(+m[1])+(+m[2])+(+m[3])+(+m[4]))|0;
}}return ipMatches+":"+octSum+":"+(m===null?"null":m[0]);}
function go(s,r){lines=new Array(N);for(var j=0;j<N;j++)lines[j]=s;
  reIp=r;octSum=0;ipMatches=0;i=0;out.push(scan());}
go("1.2.3.4",/(\d+).(\d+).(\d+).(\d+)/u);
go("1.2.3.4",/(\d+).(\d+).(\d+).(\d+)/d);
go("1.2.3.4",/(?<a>\d+).(\d+).(\d+).(\d+)/);
go("1.2.3.4",/(\d+).(\d+).(\d+).(\d+)(x?)/);
go("α 1.2.3.4",/(\d+).(\d+).(\d+).(\d+)/);
var old=RegExp.prototype.exec,calls=0;
RegExp.prototype.exec=function(s){calls++;return old.call(this,s);};
go("1.2.3.4",/(\d+).(\d+).(\d+).(\d+)/);
RegExp.prototype.exec=old;
var execDesc=Object.getOwnPropertyDescriptor(RegExp.prototype,"exec"),gets=0;
Object.defineProperty(RegExp.prototype,"exec",{configurable:true,get:function(){
  gets++;return old;
}});
go("1.2.3.4",/(\d+).(\d+).(\d+).(\d+)/);
Object.defineProperty(RegExp.prototype,"exec",execDesc);
var liCalls=0,r=/(\d+).(\d+).(\d+).(\d+)/;
r.lastIndex={valueOf:function(){liCalls++;return 7;}};
go("1.2.3.4",r);
out.push("calls="+calls+":"+gets+":"+liCalls+":"+(typeof r.lastIndex));
console.log(out.join("|"));"#;
    assert_matches_node(src);
}

#[test]
fn result_global_alias_declines_and_matches_node() {
    assert_matches_node(&alias_src(HOT));
}

#[test]
fn captured_exec_accessor_value_survives_self_deletion_before_scalar_guard() {
    // The observable Get returns `custom`, then exposes the pristine prototype
    // method before argument evaluation completes. Scalarization must validate
    // the captured Value, not re-read the now-intrinsic `reIp.exec` property.
    assert_matches_node(&captured_exec_accessor_src(HOT));
}

#[test]
fn scalar_counts_child() {
    let Some(mode) = std::env::var_os("ZIPP_RX_EXEC_COUNTS_CHILD") else {
        return;
    };
    let mode = mode.to_string_lossy();
    let src = match mode.as_ref() {
        "on" | "off" => core_src(HOT),
        "miss" => final_miss_src(HOT),
        "hole" => hole_src(HOT),
        "slow" => slow_add_src().to_owned(),
        "alias" => alias_src(HOT),
        "capture_delete" => captured_exec_accessor_src(HOT),
        other => panic!("unknown counts child {other}"),
    };
    assert_matches_node(&src);
    let stats = zipp_vm::regexp_scalar_exec_stats();
    let (success, misses, captures, materialized, elided, declines, pins, slow) = stats;
    match mode.as_ref() {
        "on" => {
            assert!(
                success > 0 && materialized > 0 && elided > 0,
                "vacuous {stats:?}"
            );
            assert_eq!(captures, 4 * success);
            assert_eq!(success, materialized + elided);
            assert_eq!((misses, declines, pins, slow), (0, 0, 0, 0));
            let (compact, ..) = zipp_vm::regexp_result_stats();
            assert_eq!(compact, HOT as u64 - elided, "allocation algebra drift");
        }
        "miss" => {
            assert!(success > 0 && misses > 0 && elided > 0, "vacuous {stats:?}");
            assert_eq!(captures, 4 * success);
            assert_eq!(success, materialized + elided);
        }
        "hole" => assert!(success > 0 && pins > 0, "pin closure vacuous {stats:?}"),
        "slow" => assert!(
            success > 0 && materialized > 0 && slow > 0,
            "slow closure vacuous {stats:?}"
        ),
        // The accessor Get itself may deopt before the scalar helper. The
        // source assertion proves the captured custom value was called, while
        // later pristine iterations keep the scalar mechanism non-vacuous.
        "capture_delete" => assert!(success > 0, "later scalar path was vacuous {stats:?}"),
        "off" | "alias" => assert_eq!(stats, (0, 0, 0, 0, 0, 0, 0, 0)),
        _ => unreachable!(),
    }
}

#[test]
fn zz_mechanism_and_dependency_switches() {
    if std::env::var_os("ZIPP_RX_EXEC_COUNTS_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test exe path");
    let cases: &[(&str, Option<(&str, &str)>)] = &[
        ("on", None),
        ("miss", None),
        ("hole", None),
        ("slow", None),
        ("capture_delete", None),
        ("alias", None),
        ("off", Some(("ZIPP_NO_RX_SCALAR_EXEC", "1"))),
        ("off", Some(("ZIPP_NO_RX_CALL_DIRECT", "1"))),
        ("off", Some(("ZIPP_NO_SLIM_EXEC", "1"))),
        ("off", Some(("ZIPP_NO_MATCH_VARIANT", "1"))),
        ("off", Some(("ZIPP_NO_TONUM_STR", "1"))),
    ];
    for (mode, extra) in cases {
        let mut cmd = Command::new(&exe);
        cmd.args(["scalar_counts_child", "--exact", "--nocapture"])
            .env("ZIPP_RX_EXEC_COUNTS_CHILD", mode)
            .env("ZIPP_RXSTATS", "1")
            .env("ZIPP_JIT_THRESHOLD", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_NO_RX_SCALAR_EXEC")
            .env_remove("ZIPP_NO_RX_CALL_DIRECT")
            .env_remove("ZIPP_NO_SLIM_EXEC")
            .env_remove("ZIPP_NO_MATCH_VARIANT")
            .env_remove("ZIPP_NO_TONUM_STR");
        if let Some((key, value)) = extra {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("spawn scalar exec mechanism child");
        assert!(
            out.status.success()
                && !String::from_utf8_lossy(&out.stdout).contains("running 0 tests"),
            "{mode}/{extra:?} child failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn scalar_mode_child() {
    let Some(mode) = std::env::var_os("ZIPP_RX_EXEC_MODE_CHILD") else {
        return;
    };
    let src = if mode == "capture_gcstress" {
        captured_exec_accessor_src(HOT)
    } else {
        core_src(HOT)
    };
    assert_matches_node(&src);
    let stats = zipp_vm::regexp_scalar_exec_stats();
    if mode == "nojit" {
        assert_eq!(stats, (0, 0, 0, 0, 0, 0, 0, 0));
    } else {
        assert!(
            stats.0 > 0 && stats.3 > 0 && stats.4 > 0,
            "vacuous {mode:?}: {stats:?}"
        );
        assert_eq!(stats.0, stats.3 + stats.4);
        assert_eq!(stats.2, 4 * stats.0);
        if mode == "capture_gcstress" {
            assert!(
                stats.0 > 0,
                "post-accessor GC-stress scalar path was vacuous: {stats:?}"
            );
        }
    }
}

#[test]
fn zz_default_threshold1_nojit_and_gcstress_modes() {
    if std::env::var_os("ZIPP_RX_EXEC_MODE_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test exe path");
    let modes: &[(&str, &[(&str, &str)])] = &[
        ("default", &[]),
        ("threshold1", &[("ZIPP_JIT_THRESHOLD", "1")]),
        ("nojit", &[("ZIPP_NOJIT", "1")]),
        (
            "gcstress",
            &[("ZIPP_GC_STRESS", "1"), ("ZIPP_JIT_THRESHOLD", "1")],
        ),
        (
            "capture_gcstress",
            &[("ZIPP_GC_STRESS", "1"), ("ZIPP_JIT_THRESHOLD", "1")],
        ),
    ];
    for (mode, envs) in modes {
        let mut cmd = Command::new(&exe);
        cmd.args(["scalar_mode_child", "--exact", "--nocapture"])
            .env("ZIPP_RX_EXEC_MODE_CHILD", mode)
            .env("ZIPP_RXSTATS", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS")
            .env_remove("ZIPP_NO_RX_SCALAR_EXEC");
        for &(key, value) in *envs {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("spawn scalar exec mode child");
        assert!(
            out.status.success()
                && !String::from_utf8_lossy(&out.stdout).contains("running 0 tests"),
            "{mode} child failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
