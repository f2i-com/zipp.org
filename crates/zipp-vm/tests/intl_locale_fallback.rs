//! Locale fallback is explicit and independent of guest String prototype hooks.
#[test]
fn date_format_fallback_ignores_replaced_split_method() {
    let result = zipp_vm::run(
        r#"
        const expected = Intl.DateTimeFormat("en", {timeZone: "UTC"}).format(86400000);
        console.log(Intl.DateTimeFormat.supportedLocalesOf(["fr"]).length);
        console.log(Intl.DateTimeFormat("fr").resolvedOptions().locale);
        for (const replacement of ["", "x-foo", "de-u-co", "en-US"]) {
            String.prototype[Symbol.split] = function () { return [replacement]; };
            console.log(Intl.DateTimeFormat("fr", {timeZone: "UTC"}).format(86400000) === expected);
        }
    "#,
    )
    .expect("valid locale program");
    assert!(result.error.is_none(), "{:?}", result.error);
    assert_eq!(result.output, ["0", "en", "true", "true", "true", "true"]);
}
