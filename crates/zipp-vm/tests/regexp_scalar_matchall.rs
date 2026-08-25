//! Exact outer-region `matchAll` scalarization: mechanism, publication order,
//! protocol fallback, exclusion, GC/JIT-mode, and kill-switch pins.

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
        .expect("node on PATH (expected values come from node -e)");
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
        r#"var N = {n}, lines = new Array(N);
for (var j = 0; j < N; j++) lines[j] = "a=1 b=22 c=333";
var reKv = /([a-z]+)=(\d+)/g;
var kvCount = 0, kvSum = 0, km;
for (var i = 0; i < N; i++) {{
  for (var km of lines[i].matchAll(reKv)) {{
    kvCount++;
    kvSum = (kvSum + (+km[2])) | 0;
  }}
}}
console.log(kvCount + "|" + kvSum + "|" + reKv.lastIndex);
console.log(km[0] + "|" + km[1] + "|" + km[2] + "|" + km.index + "|" +
            km.input + "|" + km.groups + "|" + Object.keys(km).join(","));
console.log(RegExp.input + "|" + RegExp.lastMatch + "|" + RegExp.lastParen + "|" +
            RegExp.leftContext + "|" + RegExp.rightContext + "|" + RegExp.$1 + "|" + RegExp.$2);
"#,
    )
}

#[test]
fn pristine_final_result_last_index_and_annex_b_statics_match_node() {
    assert_matches_node(&core_src(HOT));
}

#[test]
fn copied_nonzero_last_index_nullable_matches_and_short_prefix_match_node() {
    let src = r#"var N=64,lines=new Array(N+3);
for(var j=0;j<lines.length;j++)lines[j]=j<N?"a1":"a999";
var reKv=/(a?)(\d*)/g,kvCount=0,kvSum=0,km;
reKv.lastIndex=1;
for(var i=0;i<N;i++){for(var km of lines[i].matchAll(reKv)){
  kvCount++;kvSum=(kvSum+(+km[2]))|0;
}}
console.log(kvCount+"|"+kvSum+"|"+reKv.lastIndex+"|"+i+"|"+
  km[0]+"|"+km[1]+"|"+km[2]+"|"+km.index+"|"+km.input);
"#;
    assert_matches_node(src);
}

#[test]
fn outer_reducer_survives_empty_final_iterations_and_keeps_the_last_result() {
    let src = r#"var N=96,lines=new Array(N);
for(var j=0;j<N;j++)lines[j]=(j%4===3)?"":"a=1 b=22 c=333";
var reKv=/([a-z]+)=(\d+)/g,kvCount=0,kvSum=0,km;
for(var i=0;i<N;i++){for(var km of lines[i].matchAll(reKv)){
  kvCount++;kvSum=(kvSum+(+km[2]))|0;
}}
console.log(kvCount+"|"+kvSum+"|"+km[0]+"|"+km[1]+"|"+km[2]+"|"+km.index);
"#;
    assert_matches_node(src);
}

#[test]
fn pending_result_flushes_before_an_array_getter_can_observe_km() {
    let src = r#"var N=48,lines=new Array(N);
for(var j=0;j<N;j++)lines[j]="a=1 b=22";
var reKv=/([a-z]+)=(\d+)/g,kvCount=0,kvSum=0,km,i=0,seen="";
function scan(){for(i=0;i<N;i++){for(km of lines[i].matchAll(reKv)){
  kvCount++;kvSum=(kvSum+(+km[2]))|0;
}}}
scan();
Object.defineProperty(lines,"2",{configurable:true,get:function(){
  seen=km[0]+"|"+km[1]+"|"+km[2];return "c=333";
}});
i=0;kvCount=0;kvSum=0;scan();
delete lines[2];
console.log(seen+"|"+kvCount+"|"+kvSum+"|"+km[0]);
"#;
    assert_matches_node(src);
}

#[test]
fn iterator_protocol_accessors_replacements_and_noncallable_values_match_node() {
    // Warm the exact region, then mutate the same intrinsic prototype before a
    // second invocation. The compiled guards must decline before observing the
    // property; the interpreter resumes the original Get exactly once.
    let next_accessor = r#"var N = 40, lines = new Array(N);
for (var j=0;j<N;j++) lines[j]="a=1 b=2";
var reKv=/([a-z]+)=(\d+)/g, kvCount=0,kvSum=0,km,i=0;
function scan(){ for(i=0;i<N;i++){ for(km of lines[i].matchAll(reKv)){
  kvCount++; kvSum=(kvSum+(+km[2]))|0;
}}}
scan();
var p=Object.getPrototypeOf("x=1".matchAll(/([a-z])=(\d+)/g));
var nd=Object.getOwnPropertyDescriptor(p,"next"), gets=0;
Object.defineProperty(p,"next",{configurable:true,get:function(){gets++;return nd.value;}});
i=0;kvCount=0;kvSum=0;scan();
Object.defineProperty(p,"next",nd);
console.log(kvCount+"|"+kvSum+"|"+gets+"|"+km[0]);"#;
    assert_matches_node(next_accessor);

    let iterator_accessor = r#"var N = 40, lines = new Array(N);
for (var j=0;j<N;j++) lines[j]="a=1 b=2";
var reKv=/([a-z]+)=(\d+)/g, kvCount=0,kvSum=0,km,i=0;
function scan(){ for(i=0;i<N;i++){ for(km of lines[i].matchAll(reKv)){
  kvCount++; kvSum=(kvSum+(+km[2]))|0;
}}}
scan();
var p=Object.getPrototypeOf("x=1".matchAll(/([a-z])=(\d+)/g));
var id=Object.getOwnPropertyDescriptor(p,Symbol.iterator), gets=0;
var iterMethod=p[Symbol.iterator];
Object.defineProperty(p,Symbol.iterator,{configurable:true,get:function(){gets++;return iterMethod;}});
i=0;kvCount=0;kvSum=0;scan();
if(id)Object.defineProperty(p,Symbol.iterator,id);else delete p[Symbol.iterator];
console.log(kvCount+"|"+kvSum+"|"+gets+"|"+km[0]);"#;
    assert_matches_node(iterator_accessor);

    let noncallable_next = r#"var N = 40, lines = new Array(N);
for (var j=0;j<N;j++) lines[j]="a=1 b=2";
var reKv=/([a-z]+)=(\d+)/g, kvCount=0,kvSum=0,km,i=0;
function scan(){ for(i=0;i<N;i++){ for(km of lines[i].matchAll(reKv)){
  kvCount++; kvSum=(kvSum+(+km[2]))|0;
}}}
scan();
var p=Object.getPrototypeOf("x=1".matchAll(/([a-z])=(\d+)/g));
var nd=Object.getOwnPropertyDescriptor(p,"next"), got="none";
Object.defineProperty(p,"next",{configurable:true,value:1,writable:true});
i=0;kvCount=0;kvSum=0;
try { scan(); } catch(e) { got=e.constructor.name; }
Object.defineProperty(p,"next",nd);
console.log(got+"|"+i+"|"+kvCount);"#;
    assert_matches_node(noncallable_next);
}

#[test]
fn late_string_matchall_replacement_is_called_once_per_outer_iteration() {
    let src = r#"var N=40,lines=new Array(N);
for(var j=0;j<N;j++)lines[j]="a=1 b=2";
var reKv=/([a-z]+)=(\d+)/g,kvCount=0,kvSum=0,km,i=0;
function scan(){for(i=0;i<N;i++){for(km of lines[i].matchAll(reKv)){
  kvCount++;kvSum=(kvSum+(+km[2]))|0;
}}}
scan();
var old=String.prototype.matchAll,calls=0;
String.prototype.matchAll=function(){calls++;return [["q=7","q","7"]];};
i=0;kvCount=0;kvSum=0;scan();
String.prototype.matchAll=old;
console.log(kvCount+"|"+kvSum+"|"+calls+"|"+km[0]);"#;
    assert_matches_node(src);
}

#[test]
fn late_species_getter_is_observed_once_per_outer_iteration() {
    let src = r#"var N=40,lines=new Array(N);
for(var j=0;j<N;j++)lines[j]="a=1 b=2";
var reKv=/([a-z]+)=(\d+)/g,kvCount=0,kvSum=0,km,i=0;
function scan(){for(i=0;i<N;i++){for(km of lines[i].matchAll(reKv)){
  kvCount++;kvSum=(kvSum+(+km[2]))|0;
}}}
scan();
var sd=Object.getOwnPropertyDescriptor(RegExp,Symbol.species),gets=0;
Object.defineProperty(RegExp,Symbol.species,{configurable:true,get:function(){gets++;return RegExp;}});
i=0;kvCount=0;kvSum=0;scan();
Object.defineProperty(RegExp,Symbol.species,sd);
console.log(kvCount+"|"+kvSum+"|"+gets+"|"+km[0]);"#;
    assert_matches_node(src);
}

#[test]
fn late_hole_prototype_getter_and_proxy_array_are_not_batched() {
    let hole = r#"var N=40,lines=new Array(N);
for(var j=0;j<N;j++)lines[j]="a=1 b=2";
var reKv=/([a-z]+)=(\d+)/g,kvCount=0,kvSum=0,km,i=0;
function scan(){for(i=0;i<N;i++){for(km of lines[i].matchAll(reKv)){
  kvCount++;kvSum=(kvSum+(+km[2]))|0;
}}}
scan();delete lines[2];var gets=0;
Object.defineProperty(Array.prototype,"2",{configurable:true,get:function(){gets++;return "c=7";}});
i=0;kvCount=0;kvSum=0;scan();delete Array.prototype[2];
console.log(kvCount+"|"+kvSum+"|"+gets+"|"+km[0]);"#;
    assert_matches_node(hole);

    let proxy = r#"var N=40,base=new Array(N);
for(var j=0;j<N;j++)base[j]="a=1 b=2";
var lines=base,reKv=/([a-z]+)=(\d+)/g,kvCount=0,kvSum=0,km,i=0;
function scan(){for(i=0;i<N;i++){for(km of lines[i].matchAll(reKv)){
  kvCount++;kvSum=(kvSum+(+km[2]))|0;
}}}
scan();var gets=0;lines=new Proxy(base,{get:function(t,k,r){if(k!=="length")gets++;return Reflect.get(t,k,r);}});
i=0;kvCount=0;kvSum=0;scan();
console.log(kvCount+"|"+kvSum+"|"+gets+"|"+km[0]);"#;
    assert_matches_node(proxy);
}

fn out_of_range_capture_src(n: usize) -> String {
    format!(
        r#"var N={n},lines=new Array(N);
for(var j=0;j<N;j++)lines[j]="a= b=";
var reKv=/([a-z])=/g,kvCount=0,kvSum=0,km;
var gets=0;
Object.defineProperty(Array.prototype,"2",{{configurable:true,get:function(){{
  gets++; return this[0]==="a=" ? "3" : "5";
}}}});
for(var i=0;i<N;i++){{for(var km of lines[i].matchAll(reKv)){{
  kvCount++;kvSum=(kvSum+(+km[2]))|0;
}}}}
delete Array.prototype[2];
console.log(kvCount+"|"+kvSum+"|"+gets+"|"+km[0]);"#,
    )
}

#[test]
fn out_of_length_capture_observes_array_prototype_and_current_materialized_result() {
    assert_matches_node(&out_of_range_capture_src(HOT));
}

fn slow_throw_src() -> &'static str {
    r#"var N=40,lines=new Array(N);
for(var j=0;j<N;j++)lines[j]="a=1 b=2 c=3";
var reKv=/([a-z]+)=(\d+)/g,kvCount=0,kvSum=0,km,i=0;
function scan(){for(i=0;i<N;i++){for(km of lines[i].matchAll(reKv)){
  kvCount++;kvSum=(kvSum+(+km[2]))|0;
}}}
scan();
var p=Object.getPrototypeOf("x=1".matchAll(/([a-z])=(\d+)/g));
var rd=Object.getOwnPropertyDescriptor(p,"return"),closes=0,seen="",caught="";
Object.defineProperty(p,"return",{configurable:true,value:function(){closes++;return {};}});
// Preserve the object-valued accumulator through the interpreted first outer
// trip; the already-compiled OSR region enters on the second, matching line.
lines[0]="no pairs here";lines[1]="a=1 b=2 c=3";
i=0;N=2;kvCount=0;
kvSum={valueOf:function(){
  seen=km[0]+"|"+km[1]+"|"+km[2]+"|"+km.index+"|"+Object.keys(km).join(",");
  throw new Error("boom");
}};
try{scan();}catch(e){caught=e.message;}
if(rd)Object.defineProperty(p,"return",rd);else delete p.return;
console.log(seen+"|"+caught+"|"+closes+"|"+kvCount);"#
}

#[test]
fn slow_add_flushes_before_valueof_throw_and_iterator_close() {
    assert_matches_node(slow_throw_src());
}

#[test]
fn unicode_indices_named_many_capture_nonascii_and_custom_exec_fall_back() {
    let src = r#"var N=24,lines, reKv, kvCount,kvSum,km,i;
function scan(){for(i=0;i<N;i++){for(km of lines[i].matchAll(reKv)){
  kvCount++;kvSum=(kvSum+(+km[2]))|0;
}}return kvCount+":"+kvSum+":"+km[0];}
var out=[];
function go(s,r){lines=new Array(N);for(var j=0;j<N;j++)lines[j]=s;
  reKv=r;kvCount=0;kvSum=0;i=0;out.push(scan());}
go("a=1 b=2",/([a-z]+)=(\d+)/gu);
go("a=1 b=2",/([a-z]+)=(\d+)/dg);
go("a=1 b=2",/(?<key>[a-z]+)=(\d+)/g);
go("a=1",/(a)(=)(1)(x?)(y?)/g);
go("α=9 a=2",/([a-z]+)=(\d+)/g);
var old=RegExp.prototype.exec,calls=0;
RegExp.prototype.exec=function(s){calls++;return old.call(this,s);};
go("a=1 b=2",/([a-z]+)=(\d+)/g);
RegExp.prototype.exec=old;
out.push("exec="+calls);
console.log(out.join("|"));"#;
    assert_matches_node(src);
}

fn alias_src(n: usize) -> String {
    format!(
        r#"var N={n},lines=new Array(N);
for(var j=0;j<N;j++)lines[j]="a=1 b=2";
var reKv=/([a-z]+)=(\d+)/g,km=0,kvSum=0;
for(var i=0;i<N;i++){{for(var km of lines[i].matchAll(reKv)){{
  km++;kvSum=(kvSum+(+km[1]))|0;
}}}}
console.log(String(km)+"|"+kvSum+"|"+i);"#,
    )
}

#[test]
fn result_global_alias_template_declines_and_matches_node() {
    assert_matches_node(&alias_src(HOT));
}

#[test]
fn scalar_counts_child() {
    let Some(mode) = std::env::var_os("ZIPP_RX_SCALAR_COUNTS_CHILD") else {
        return;
    };
    let mode = mode.to_string_lossy();
    let src = match mode.as_ref() {
        "on" | "off" | "array_off" => core_src(HOT),
        "out_of_range" => out_of_range_capture_src(HOT),
        "slow_throw" => slow_throw_src().to_owned(),
        "alias" => alias_src(HOT),
        other => panic!("unknown counts child {other}"),
    };
    assert_matches_node(&src);
    let scalar = zipp_vm::regexp_scalar_matchall_stats();
    let (_, _, jit_matchall, _, _) = zipp_vm::regexp_string_call_direct_stats();
    let (success, capture, materialized, elided, declines, slow) = scalar;
    match mode.as_ref() {
        "on" => {
            assert!(
                success > 0 && materialized > 0 && elided > 0,
                "vacuous: {scalar:?}"
            );
            assert_eq!(
                capture, success,
                "capture consumer did not serve every success"
            );
            assert_eq!(success, materialized + elided, "pending accounting drift");
            assert_eq!((declines, slow), (0, 0));
            let (compact, ..) = zipp_vm::regexp_result_stats();
            assert_eq!(
                compact,
                (3 * HOT) as u64 - elided,
                "allocation algebra drift"
            );
            assert!(
                materialized <= 2,
                "outer reducer did not collapse result materialization: {scalar:?}"
            );
            assert!(
                jit_matchall > 0 && jit_matchall < (HOT / 2) as u64,
                "outer reducer did not collapse matchAll calls: {jit_matchall}"
            );
        }
        "array_off" => {
            assert!(
                success > 0 && materialized > 0 && elided > 0,
                "vacuous: {scalar:?}"
            );
            assert_eq!(capture, success);
            assert_eq!(success, materialized + elided);
            assert!(
                jit_matchall > (HOT / 2) as u64,
                "array-reducer comparator did not restore per-subject calls: {jit_matchall}"
            );
        }
        "out_of_range" => {
            assert!(
                success > 0 && materialized > 0 && declines > 0,
                "vacuous: {scalar:?}"
            );
            assert_eq!(capture, 0, "out-of-length capture was incorrectly consumed");
            assert_eq!(elided, 0, "a declined capture left a result elided");
        }
        "slow_throw" => {
            assert!(
                success > 0 && slow > 0,
                "slow Add closure was vacuous: {scalar:?}"
            );
            assert_eq!(success, materialized + elided, "pending accounting drift");
        }
        "off" | "alias" => assert_eq!(scalar, (0, 0, 0, 0, 0, 0)),
        _ => unreachable!(),
    }
}

#[test]
fn zz_scalar_mechanism_guards_and_dependency_switches() {
    if std::env::var_os("ZIPP_RX_SCALAR_COUNTS_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test exe path");
    let cases: &[(&str, &[(&str, &str)])] = &[
        ("on", &[]),
        ("out_of_range", &[]),
        ("slow_throw", &[]),
        ("alias", &[]),
        ("array_off", &[("ZIPP_NO_RX_ARRAY_MATCHALL_REDUCE", "1")]),
        ("off", &[("ZIPP_NO_RX_SCALAR_MATCHALL", "1")]),
        ("off", &[("ZIPP_NO_MATCHALL_PRISTINE", "1")]),
        ("off", &[("ZIPP_NO_FASTOK_MEMO", "1")]),
        ("off", &[("ZIPP_NO_RX_STRING_CALL_DIRECT", "1")]),
        ("off", &[("ZIPP_NO_MATCHALL_STEP", "1")]),
        ("off", &[("ZIPP_NO_MATCHALL_BATCH", "1")]),
        ("off", &[("ZIPP_NO_SLIM_EXEC", "1")]),
        ("off", &[("ZIPP_NO_MATCH_VARIANT", "1")]),
        ("off", &[("ZIPP_NO_ITER_REGION", "1")]),
        ("off", &[("ZIPP_NO_TONUM_STR", "1")]),
    ];
    for (mode, extra) in cases {
        let mut cmd = Command::new(&exe);
        cmd.args(["scalar_counts_child", "--exact", "--nocapture"])
            .env("ZIPP_RX_SCALAR_COUNTS_CHILD", mode)
            .env("ZIPP_RXSTATS", "1")
            .env("ZIPP_JIT_THRESHOLD", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_NO_RX_SCALAR_MATCHALL")
            .env_remove("ZIPP_NO_RX_ARRAY_MATCHALL_REDUCE")
            .env_remove("ZIPP_NO_MATCHALL_PRISTINE")
            .env_remove("ZIPP_NO_FASTOK_MEMO")
            .env_remove("ZIPP_NO_RX_STRING_CALL_DIRECT")
            .env_remove("ZIPP_NO_MATCHALL_STEP")
            .env_remove("ZIPP_NO_MATCHALL_BATCH")
            .env_remove("ZIPP_NO_SLIM_EXEC")
            .env_remove("ZIPP_NO_MATCH_VARIANT")
            .env_remove("ZIPP_NO_ITER_REGION")
            .env_remove("ZIPP_NO_TONUM_STR");
        for &(key, value) in *extra {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("spawn scalar mechanism child");
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
    let Some(mode) = std::env::var_os("ZIPP_RX_SCALAR_MODE_CHILD") else {
        return;
    };
    assert_matches_node(&core_src(HOT));
    let scalar = zipp_vm::regexp_scalar_matchall_stats();
    if mode == "nojit" {
        assert_eq!(scalar, (0, 0, 0, 0, 0, 0));
    } else {
        assert!(
            scalar.0 > 0 && scalar.2 > 0 && scalar.3 > 0,
            "vacuous {mode:?}: {scalar:?}"
        );
        assert_eq!(scalar.0, scalar.2 + scalar.3);
    }
}

#[test]
fn zz_scalar_default_threshold1_nojit_and_gcstress_modes() {
    if std::env::var_os("ZIPP_RX_SCALAR_MODE_CHILD").is_some() {
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
    ];
    for (mode, envs) in modes {
        let mut cmd = Command::new(&exe);
        cmd.args(["scalar_mode_child", "--exact", "--nocapture"])
            .env("ZIPP_RX_SCALAR_MODE_CHILD", mode)
            .env("ZIPP_RXSTATS", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS")
            .env_remove("ZIPP_NO_RX_SCALAR_MATCHALL")
            .env_remove("ZIPP_NO_RX_ARRAY_MATCHALL_REDUCE");
        for &(key, value) in *envs {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("spawn scalar mode child");
        assert!(
            out.status.success()
                && !String::from_utf8_lossy(&out.stdout).contains("running 0 tests"),
            "{mode} child failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
