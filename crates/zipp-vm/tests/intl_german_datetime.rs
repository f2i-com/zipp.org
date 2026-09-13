//! German CLDR data is confined to DateTimeFormat; other services still fall back.
fn check(source: &str, expected: &[&str]) {
    let result = zipp_vm::run(source).expect("valid JavaScript");
    assert!(result.error.is_none(), "{:?}", result.error);
    assert_eq!(result.output, expected);
}

#[test]
fn service_locales_and_default_hour_cycle() {
    check(
        r#"
        console.log(Intl.DateTimeFormat.supportedLocalesOf(['de','de-DE','de-AT','fr']).join(','));
        console.log(Intl.NumberFormat.supportedLocalesOf(['de']).length);
        console.log(Intl.Collator.supportedLocalesOf(['de']).length);
        console.log(new Intl.DateTimeFormat('de-AT').resolvedOptions().locale);
        console.log(new Intl.DateTimeFormat('de', {hour:'numeric'}).resolvedOptions().hourCycle);
        console.log(new Intl.DateTimeFormat('en', {hour:'numeric'}).resolvedOptions().hourCycle);
        console.log(new Intl.Locale('de').getHourCycles().join(','));
        console.log(new Intl.Locale('de-u-hc-h11').getHourCycles().join(','));
    "#,
        &["de,de-DE,de-AT", "0", "0", "de", "h23", "h12", "h23", "h11"],
    );
}

#[test]
fn german_patterns_parts_and_standalone_months() {
    check(
        r#"
        const t = Date.UTC(2020,0,2,15,4,5,678);
        for (const dateStyle of ['full','long','medium','short']) {
            console.log(new Intl.DateTimeFormat('de',{dateStyle,timeZone:'UTC'}).format(t));
        }
        console.log(new Intl.DateTimeFormat('de',{month:'short',timeZone:'UTC'}).format(t));
        console.log(new Intl.DateTimeFormat('de',{year:'numeric',month:'short',day:'numeric',timeZone:'UTC'}).format(t));
        const f = new Intl.DateTimeFormat('de',{hour:'numeric',minute:'numeric',second:'numeric',fractionalSecondDigits:3,timeZone:'UTC'});
        console.log(f.format(t));
        console.log(f.formatToParts(t).map(p => p.type + ':' + p.value).join('|'));
    "#,
        &[
            "Donnerstag, 2. Januar 2020",
            "2. Januar 2020",
            "02.01.2020",
            "02.01.20",
            "Jan",
            "2. Jan. 2020",
            "15:04:05,678",
            "hour:15|literal::|minute:04|literal::|second:05|literal:,|fractionalSecond:678",
        ],
    );
}

#[test]
fn styles_honor_all_hour_cycles_at_midnight() {
    check(
        r#"
        for (const hourCycle of ['h11','h12','h23','h24']) {
            for (const timeStyle of ['full','long','medium','short']) {
                const f = new Intl.DateTimeFormat('de',{timeStyle,hourCycle,timeZone:'UTC'});
                console.log(f.format(0));
                console.log(f.formatToParts(0).map(p => p.value).join('') === f.format(0));
            }
        }
    "#,
        &[
            "00:00:00 AM Koordinierte Weltzeit",
            "true",
            "00:00:00 AM UTC",
            "true",
            "00:00:00 AM",
            "true",
            "00:00 AM",
            "true",
            "12:00:00 AM Koordinierte Weltzeit",
            "true",
            "12:00:00 AM UTC",
            "true",
            "12:00:00 AM",
            "true",
            "12:00 AM",
            "true",
            "00:00:00 Koordinierte Weltzeit",
            "true",
            "00:00:00 UTC",
            "true",
            "00:00:00",
            "true",
            "00:00",
            "true",
            "24:00:00 Koordinierte Weltzeit",
            "true",
            "24:00:00 UTC",
            "true",
            "24:00:00",
            "true",
            "24:00",
            "true",
        ],
    );
}

#[test]
fn calendar_day_period_and_interval_use_german_data() {
    check(
        r#"
        const t = Date.UTC(2020,0,2,15,4,5);
        console.log(new Intl.DateTimeFormat('de',{calendar:'buddhist',dateStyle:'full',timeZone:'UTC'}).format(t));
        console.log(new Intl.DateTimeFormat('de',{dayPeriod:'long',timeZone:'UTC'}).format(t));
        const f = new Intl.DateTimeFormat('de',{year:'numeric',month:'short',day:'numeric',timeZone:'UTC'});
        console.log(f.formatRange(t,t+86400000));
        console.log(f.formatRangeToParts(t,t+86400000).map(p=>p.value).join('') === f.formatRange(t,t+86400000));
        String.prototype[Symbol.split] = function() { return ['en-US']; };
        console.log(new Date(0).toLocaleDateString('de',{timeZone:'UTC'}));
    "#,
        &[
            "Donnerstag, 2. Januar 2563 BE",
            "nachmittags",
            "2.–3. Jan. 2020",
            "true",
            "1.1.1970",
        ],
    );
}
