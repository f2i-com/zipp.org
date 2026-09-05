//! An array carrying named metadata (`a.meta = 1`) keeps its allocation-free
//! dense read path; only an element key layered over the dense storage — a
//! defineProperty'd index or a sparse element — routes reads through the
//! full `get_index` semantics.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

#[test]
fn named_metadata_reads_stay_exact_and_element_overrides_still_win() {
    let src = r#"
      var a = [];
      for (var i = 0; i < 64; i++) a.push(i * 3);
      a.meta = 'tag';
      a.count = 64;
      var s = 0;
      for (var r = 0; r < 50; r++) for (var j = 0; j < a.length; j++) s += a[j];
      var log = [s, a.meta, a.count, a[64], a[-1]];
      // A getter at an index must be consulted, metadata or not.
      Object.defineProperty(a, 5, { get: function () { return 'five'; }, configurable: true });
      log.push(a[5], a[4], a[6]);
      // A sparse element far past the dense storage.
      a[100000] = 'sparse';
      log.push(a[100000], a[99999], a[7], a.length);
      // Frozen arrays still read.
      var f = [1, 2, 3];
      f.note = 'n';
      Object.freeze(f);
      f[1] = 99;
      log.push(f[0], f[1], f[2], f.note);
      // Holes consult the prototype chain.
      var h = [1, , 3];
      h.meta = true;
      Array.prototype[1] = 'inherited';
      log.push(h[1]);
      delete Array.prototype[1];
      log.push(h[1]);
      console.log(log.join(','));
    "#;
    assert_eq!(
        run_ok(src),
        vec!["302400,tag,64,,,five,12,18,sparse,,21,100001,1,2,3,n,inherited,"]
    );
}
