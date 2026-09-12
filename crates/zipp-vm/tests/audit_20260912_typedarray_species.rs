//! TypedArray species invariants and slice's observable bytewise aliasing.
//! Normative references: ECMA-262 TypedArraySpeciesCreate,
//! TypedArrayCreateFromConstructor, and %TypedArray%.prototype.slice/subarray.

fn run_lines(src: &str) -> Vec<String> {
    let result = zipp_vm::run(src).expect("source compiles");
    assert!(
        result.error.is_none(),
        "unexpected runtime error: {:?}; output: {:?}",
        result.error,
        result.output
    );
    result.output
}

#[test]
fn slice_preserves_nan_bits_and_forward_aliasing_for_every_float_width() {
    assert_eq!(
        run_lines(
            r#"
            function check(TA, Bits, pattern) {
                // The hardened profile deliberately omits shared-memory APIs.
                var modes = typeof SharedArrayBuffer === 'function' ? 3 : 2;
                for (var mode = 0; mode < modes; mode++) {
                    var size = TA.BYTES_PER_ELEMENT;
                    var buffer = mode === 0 ? new ArrayBuffer(4 * size) :
                        mode === 1 ? new ArrayBuffer(4 * size, {maxByteLength: 8 * size}) :
                        new SharedArrayBuffer(4 * size);
                    var bits = new Bits(buffer);
                    bits[0] = pattern;
                    var source = new TA(buffer);
                    source.constructor = {
                        [Symbol.species]: function() { return new TA(buffer, size); }
                    };
                    var result = source.slice(0, 3);
                    if (result.byteOffset !== size || bits[1] !== pattern ||
                        bits[2] !== pattern || bits[3] !== pattern) throw new Error('NaN bits');
                }
            }
            check(Float16Array, Uint16Array, 0x7c01);
            check(Float32Array, Uint32Array, 0x7f800001);
            check(Float64Array, BigUint64Array, 0x7ff0000000000001n);
            console.log('preserved');
            "#
        ),
        ["preserved"]
    );
}

#[test]
fn slice_aliasing_covers_exact_backward_disjoint_and_different_kinds() {
    assert_eq!(
        run_lines(
            r#"
            function copy(from, to, count) {
                var buffer = new ArrayBuffer(16);
                var bits = new Uint32Array(buffer);
                bits.set([0x7f800001, 0x7fc12345, 0x7fc23456, 0x3f800000]);
                var source = new Float32Array(buffer);
                source.constructor = {
                    [Symbol.species]: function() { return new Float32Array(buffer, to * 4); }
                };
                source.slice(from, from + count);
                return Array.from(bits).map(function(x) { return x.toString(16); }).join(',');
            }
            console.log(copy(0, 0, 3));
            console.log(copy(1, 0, 3));
            console.log(copy(0, 3, 1));
            var source = new Int8Array([10, 20, 30, 40, 50, 60]);
            source.constructor = {
                [Symbol.species]: function() { return new Uint8Array(source.buffer, 2); }
            };
            console.log(source.slice(1, 4).join(','));
            "#
        ),
        [
            "7f800001,7fc12345,7fc23456,3f800000",
            "7fc12345,7fc23456,3f800000,3f800000",
            "7f800001,7fc12345,7fc23456,7f800001",
            "20,20,20,60",
        ]
    );
}

#[test]
fn empty_species_results_still_require_matching_content_types() {
    assert_eq!(
        run_lines(
            r#"
            var caught = 0;
            for (var direction = 0; direction < 2; direction++) {
                var source = direction ? new BigInt64Array(0) : new Int8Array(0);
                source.constructor = { [Symbol.species]: direction ? Int8Array : BigInt64Array };
                for (var method of ['slice', 'map', 'filter', 'subarray']) {
                    try {
                        if (method === 'map' || method === 'filter') source[method](function(x) { return x; });
                        else source[method]();
                    } catch (e) {
                        if (!(e instanceof TypeError)) throw e;
                        caught++;
                    }
                }
            }
            console.log(caught);
            "#
        ),
        ["8"]
    );
}

#[test]
fn map_checks_species_content_type_before_callbacks_and_filter_after() {
    assert_eq!(
        run_lines(
            r#"
            var source = new Int8Array([1, 2, 3]);
            source.constructor = { [Symbol.species]: BigInt64Array };
            var mapCalls = 0, filterCalls = 0, errors = 0;
            try { source.map(function(x) { mapCalls++; return 1n; }); }
            catch (e) { if (!(e instanceof TypeError)) throw e; errors++; }
            try { source.filter(function(x) { filterCalls++; return false; }); }
            catch (e) { if (!(e instanceof TypeError)) throw e; errors++; }
            console.log(mapCalls, filterCalls, errors);
            "#
        ),
        ["0 3 2"]
    );
}

#[test]
fn subarray_rejects_detached_and_out_of_bounds_species_results() {
    assert_eq!(
        run_lines(
            r#"
            var errors = 0;
            for (var mode = 0; mode < 3; mode++) {
                var source = new Uint8Array(8);
                source.constructor = { [Symbol.species]: function() {
                    var buffer = new ArrayBuffer(8, {maxByteLength: 16});
                    var result = mode === 2 ? new Uint8Array(buffer, 4) : new Uint8Array(buffer, 0, 8);
                    if (mode === 0) buffer.transfer();
                    else buffer.resize(2);
                    return result;
                } };
                try { source.subarray(0, 1); }
                catch (e) { if (!(e instanceof TypeError)) throw e; errors++; }
            }
            console.log(errors);
            "#
        ),
        ["3"]
    );
}

#[test]
fn subarray_accepts_short_and_immutable_species_results() {
    assert_eq!(
        run_lines(
            r#"
            var source = new Uint8Array(8);
            var short = new Uint8Array(0);
            source.constructor = { [Symbol.species]: function() { return short; } };
            console.log(source.subarray(0, 8) === short);
            var buffer = new Uint8Array([4, 5, 6]).buffer.transferToImmutable();
            var immutable = new Uint8Array(buffer);
            source.constructor = { [Symbol.species]: function() { return immutable; } };
            console.log(source.subarray(0, 8) === immutable, immutable.subarray(1).join(','));
            "#
        ),
        ["true", "true 5,6"]
    );
}

#[test]
fn default_subarray_species_rechecks_bounds_after_coercion_and_lookup() {
    assert_eq!(
        run_lines(
            r#"
            var errors = 0;
            for (var tracking = 0; tracking < 2; tracking++) {
                for (var atLookup = 0; atLookup < 2; atLookup++) {
                    var buffer = new ArrayBuffer(8, {maxByteLength: 16});
                    var source = tracking ? new Uint8Array(buffer) : new Uint8Array(buffer, 0, 8);
                    var start = atLookup ? 4 : { valueOf: function() { buffer.resize(2); return 4; } };
                    Object.defineProperty(source, 'constructor', {get: function() {
                        if (atLookup) buffer.resize(2);
                        return undefined;
                    }});
                    try { source.subarray(start); }
                    catch (e) { if (!(e instanceof RangeError)) throw e; errors++; }
                }
            }
            var buffer = new ArrayBuffer(8, {maxByteLength: 16});
            var source = new Uint8Array(buffer, 0, 8);
            Object.defineProperty(source, 'constructor', {get: function() {
                buffer.resize(4); return { [Symbol.species]: null };
            }});
            try { source.subarray(0, 8); }
            catch (e) { if (!(e instanceof RangeError)) throw e; errors++; }
            console.log(errors);
            "#
        ),
        ["5"]
    );
}

#[test]
fn slice_immutable_destination_and_detach_order_remain_correct() {
    assert_eq!(
        run_lines(
            r#"
            var errors = 0;
            var immutable = new Uint8Array([1, 2, 3].length).buffer.transferToImmutable();
            var source = new Uint8Array(3);
            source.constructor = { [Symbol.species]: function() { return new Uint8Array(immutable); } };
            for (var count of [0, 2]) {
                try { source.slice(0, count); }
                catch (e) { if (!(e instanceof TypeError)) throw e; errors++; }
            }
            var lengths = [];
            for (var count of [0, 2]) {
                var current = new Uint8Array([1, 2, 3]);
                current.constructor = { [Symbol.species]: function(length) {
                    current.buffer.transfer(); return new Uint8Array(length);
                } };
                try { lengths.push(current.slice(0, count).length); }
                catch (e) { if (!(e instanceof TypeError)) throw e; errors++; }
            }
            var readable = new Uint8Array(new Uint8Array([7, 8]).buffer.transferToImmutable());
            console.log(errors, lengths.join(','), readable.slice().join(','));
            "#
        ),
        ["3 0 7,8"]
    );
}
