//! Observable buffer length/coercion ordering and bit-preserving bulk copies.

fn run_lines(src: &str) -> Vec<String> {
    let result = zipp_vm::run(src).expect("source compiles");
    assert!(
        result.error.is_none(),
        "{:?}; output: {:?}",
        result.error,
        result.output
    );
    result.output
}

#[test]
fn buffer_slice_keeps_requested_length_when_argument_coercion_shrinks_source() {
    assert_eq!(
        run_lines(
            r#"
        for (var phase = 0; phase < 2; phase++) {
            var b = new ArrayBuffer(8, {maxByteLength: 16});
            new Uint8Array(b).set([1,2,3,4,5,6,7,8]);
            var start = phase === 0 ? {valueOf() {b.resize(3); return 1;}} : 1;
            var end = phase === 1 ? {valueOf() {b.resize(3); return 7;}} : 7;
            var r = b.slice(start, end);
            console.log(r.byteLength + '/' + new Uint8Array(r).join(','));
        }
    "#
        ),
        ["6/2,3,0,0,0,0", "6/2,3,0,0,0,0"]
    );
}

#[test]
fn buffer_slice_species_shrink_copies_only_live_prefix_and_preserves_result_tail() {
    assert_eq!(
        run_lines(
            r#"
        for (var live of [3, 1, 0]) {
            var b = new ArrayBuffer(8, {maxByteLength: 16});
            new Uint8Array(b).set([1,2,3,4,5,6,7,8]);
            b.constructor = {[Symbol.species]: function(n) {
                b.resize(live);
                var result = new ArrayBuffer(n + 1);
                new Uint8Array(result).fill(9);
                return result;
            }};
            var r = b.slice(1, 7);
            console.log(r.byteLength + '/' + new Uint8Array(r).join(','));
        }
    "#
        ),
        ["7/2,3,9,9,9,9,9", "7/9,9,9,9,9,9,9", "7/9,9,9,9,9,9,9"]
    );
}

#[test]
fn buffer_slice_constructs_species_before_rechecking_argument_detachment() {
    assert_eq!(
        run_lines(
            r#"
        var b = new ArrayBuffer(8);
        var log = '';
        b.constructor = {get [Symbol.species]() {
            log += 'species,';
            return function(n) {log += 'construct:' + n + ','; return new ArrayBuffer(n);};
        }};
        try {b.slice({valueOf() {b.transfer(); return 1;}}, 7);}
        catch(e) {log += e.name;}
        console.log(log);
    "#
        ),
        ["species,construct:6,TypeError"]
    );
}

#[test]
fn shared_buffer_slice_species_must_return_shared_memory() {
    assert_eq!(
        run_lines(
            r#"
        if (typeof SharedArrayBuffer === 'function') {
            for (var size of [0, 4]) {
                var b = new SharedArrayBuffer(size);
                b.constructor = {[Symbol.species]: ArrayBuffer};
                try {b.slice(); throw new Error('accepted nonshared species');}
                catch(e) {if (!(e instanceof TypeError)) throw e;}
            }
        }
        console.log('correct brand');
    "#
        ),
        ["correct brand"]
    );
}

#[test]
fn shared_grow_revalidates_live_length_after_argument_coercion() {
    assert_eq!(
        run_lines(
            r#"
        if (typeof SharedArrayBuffer === 'function') {
            var b = new SharedArrayBuffer(4, {maxByteLength: 16});
            try {
                b.grow({valueOf() {b.grow(12); new Uint8Array(b)[10] = 77; return 8;}});
                throw new Error('grow shrank');
            } catch(e) {if (!(e instanceof RangeError)) throw e;}
            if (b.byteLength !== 12 || new Uint8Array(b)[10] !== 77) throw new Error('lost growth');
            b.grow(12);
            b.grow(16);
            if (new Uint8Array(b)[10] !== 77 || new Uint8Array(b)[15] !== 0) throw new Error('bytes');
        }
        console.log('monotonic');
    "#
        ),
        ["monotonic"]
    );
}

#[test]
fn typed_array_set_observes_target_length_after_offset_but_before_source_getters() {
    assert_eq!(
        run_lines(
            r#"
        for (var typed of [false, true]) {
            var b = new ArrayBuffer(8, {maxByteLength: 16});
            var a = new Uint8Array(b);
            var source = typed ? new Uint8Array([1,2,3,4]) : [1,2,3,4];
            try {a.set(source, {valueOf() {b.resize(2); return 0;}});}
            catch(e) {console.log(e.name + '/' + a.join(','));}
            b.resize(2);
            a.set(source, {valueOf() {b.resize(8); return 4;}});
            console.log(a.join(','));
        }
        var b = new ArrayBuffer(4, {maxByteLength: 8}), a = new Uint8Array(b);
        a.set({get length() {b.resize(2); return 4;}, 0: 5, 1: 6, 2: 7, 3: 8});
        console.log(a.join(','));
    "#
        ),
        [
            "RangeError/0,0",
            "0,0,0,0,1,2,3,4",
            "RangeError/0,0",
            "0,0,0,0,1,2,3,4",
            "5,6"
        ]
    );
}

#[test]
fn typed_array_set_and_copywithin_preserve_float_bits_with_overlapping_views() {
    assert_eq!(
        run_lines(
            r#"
        function check(TA, Bits, first, second) {
            var modes = typeof SharedArrayBuffer === 'function' ? 3 : 2;
            for (var mode = 0; mode < modes; mode++) {
                for (var method of ['set', 'copyWithin']) {
                    var size = TA.BYTES_PER_ELEMENT;
                    var b = mode === 0 ? new ArrayBuffer(4 * size) : mode === 1 ?
                        new ArrayBuffer(4 * size, {maxByteLength: 8 * size}) : new SharedArrayBuffer(4 * size);
                    var bits = new Bits(b), a = new TA(b);
                    bits[0] = first; bits[1] = second;
                    if (method === 'set') a.set(new TA(b, 0, 2), 1);
                    else a.copyWithin(1, 0, 2);
                    if (bits[0] !== first || bits[1] !== first || bits[2] !== second) throw new Error('forward ' + method);
                    if (method === 'set') a.set(new TA(b, size, 2));
                    else a.copyWithin(0, 1, 3);
                    if (bits[0] !== first || bits[1] !== second || bits[2] !== second) throw new Error('backward ' + method);
                    var separate = new TA(2);
                    separate.set(new TA(b, 0, 2));
                    var copied = new Bits(separate.buffer);
                    if (copied[0] !== first || copied[1] !== second) throw new Error('separate buffer');
                }
            }
        }
        check(Float16Array, Uint16Array, 0x7c01, 0x7e25);
        check(Float32Array, Uint32Array, 0x7f800001, 0x7fc12345);
        check(Float64Array, BigUint64Array, 0x7ff0000000000001n, 0x7ff8123456789abcn);
        console.log('raw copies');
    "#
        ),
        ["raw copies"]
    );
}

#[test]
fn copywithin_only_revalidates_buffers_when_original_copy_count_is_positive() {
    assert_eq!(
        run_lines(
            r#"
        for (var target of [0, 4]) {
            var b = new ArrayBuffer(4), a = new Uint8Array(b);
            var start = {valueOf() {b.transfer(); return target === 0 ? 4 : 0;}};
            console.log(a.copyWithin(target, start) === a);
        }
        var b = new ArrayBuffer(4), a = new Uint8Array(b);
        try {a.copyWithin(0, {valueOf() {b.transfer(); return 0;}});}
        catch(e) {console.log(e.name);}
        var b = new ArrayBuffer(4, {maxByteLength: 8}), a = new Uint8Array(b);
        a.set([1,2,3,4]);
        a.copyWithin(1, {valueOf() {b.resize(8); return 0;}});
        console.log(a.join(','));
        a.copyWithin(0, {valueOf() {b.resize(2); return 1;}});
        console.log(a.join(','));
    "#
        ),
        ["true", "true", "TypeError", "1,1,2,3,0,0,0,0", "1,1"]
    );
}

#[test]
fn dataview_numeric_writes_reject_bigint_before_bounds_checks() {
    assert_eq!(
        run_lines(
            r#"
        var view = new DataView(new ArrayBuffer(8));
        for (var name of ['setInt8','setUint8','setInt16','setUint16','setInt32','setUint32','setFloat16','setFloat32','setFloat64']) {
            for (var value of [1n, Object(1n), {valueOf() {return 1n;}}]) {
                for (var offset of [0, 100]) {
                    try {view[name](offset, value); throw new Error('accepted BigInt: ' + name);}
                    catch(e) {if (!(e instanceof TypeError)) throw e;}
                }
            }
        }
        var date = new Date(5);
        date.valueOf = function() {return 17;};
        view.setInt8(0, date);
        console.log(view.getInt8(0));
    "#
        ),
        ["17"]
    );
}

#[test]
fn typed_buffer_numeric_arguments_use_strict_tonumber_including_object_results() {
    assert_eq!(
        run_lines(
            r#"
        function rejects(fn, label) {
            try {fn(); throw new Error('accepted BigInt: ' + label);}
            catch(e) {if (!(e instanceof TypeError)) throw e;}
        }
        for (var value of [1n, Object(1n), {valueOf() {return 1n;}}]) {
            var a = new Int8Array([1, 2]);
            rejects(function() {a[0] = value;}, 'element');
            rejects(function() {a[100] = value;}, 'absent element');
            rejects(function() {a.at(value);}, 'at');
            rejects(function() {a.includes(1, value);}, 'includes');
            rejects(function() {a.indexOf(1, value);}, 'indexOf');
            rejects(function() {a.lastIndexOf(1, value);}, 'lastIndexOf');
            rejects(function() {a.fill(value);}, 'fill value');
            rejects(function() {a.fill(0, value);}, 'fill start');
            rejects(function() {a.copyWithin(value, 0);}, 'copyWithin');
            rejects(function() {a.slice(value);}, 'slice');
            rejects(function() {a.subarray(value);}, 'subarray');
            rejects(function() {a.set([], value);}, 'set offset');
            rejects(function() {a.set({length: value});}, 'set source length');
            rejects(function() {a.with(value, 0);}, 'with index');
            rejects(function() {a.with(0, value);}, 'with value');
            rejects(function() {a.sort(function() {return value;});}, 'sort result');
            rejects(function() {a.toSorted(function() {return value;});}, 'toSorted result');
            var b = new ArrayBuffer(4, {maxByteLength: 8});
            rejects(function() {b.resize(value);}, 'resize');
            rejects(function() {b.slice(value);}, 'buffer slice');
            if (typeof SharedArrayBuffer === 'function') {
                var shared = new SharedArrayBuffer(0, {maxByteLength: 8});
                rejects(function() {shared.grow(value);}, 'grow');
            }
        }
        console.log('strict coercion');
    "#
        ),
        ["strict coercion"]
    );
}
