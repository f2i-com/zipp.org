//! Calendar patterns, localized defaults and numbering annotations share one formatter.
fn check(source: &str, expected: &[&str]) {
    let result = zipp_vm::run(source).expect("valid JavaScript");
    assert!(result.error.is_none(), "{:?}", result.error);
    assert_eq!(result.output, expected);
}

#[test]
fn related_year_and_cyclic_name_are_distinct_fields() {
    check(
        r#"
        const date = Date.UTC(2020,4,23);
        for (const calendar of ['chinese','dangi']) {
            const numeric = new Intl.DateTimeFormat('en',{calendar,timeZone:'UTC',year:'numeric',month:'numeric',day:'numeric'});
            console.log(numeric.formatToParts(date).map(p=>p.type+':'+p.value).join('|'));
            const long = new Intl.DateTimeFormat('en',{calendar,timeZone:'UTC',year:'numeric',month:'long',day:'numeric'});
            console.log(long.formatToParts(date).filter(p=>p.type==='relatedYear'||p.type==='yearName').map(p=>p.type+':'+p.value).join('|'));
        }
        const zh = new Intl.DateTimeFormat('zh-u-ca-chinese',{year:'numeric',timeZone:'UTC'});
        console.log(zh.format(Date.UTC(2019,5,1)));
    "#,
        &[
            "month:4bis|literal:/|day:1|literal:/|relatedYear:2020",
            "relatedYear:2020|yearName:geng-zi",
            "month:4bis|literal:/|day:1|literal:/|relatedYear:2020",
            "relatedYear:2020|yearName:geng-zi",
            "2019己亥年",
        ],
    );
}

#[test]
fn hebrew_month_names_follow_common_and_leap_years() {
    check(
        r#"
        const f = new Intl.DateTimeFormat('en',{calendar:'hebrew',month:'numeric',timeZone:'UTC'});
        for (const date of [Date.UTC(2023,2,15),Date.UTC(2024,1,15),Date.UTC(2024,2,15),Date.UTC(2024,3,25)]) {
            console.log(f.formatToParts(date).find(p=>p.type==='month').value);
        }
    "#,
        &["Adar", "Adar I", "Adar II", "Nisan"],
    );
}

#[test]
fn era_names_follow_calendar_codes_and_proleptic_rules() {
    check(
        r#"
        for (const calendar of ['islamic-civil','islamic-tbla','islamic-umalqura']) {
            const f = new Intl.DateTimeFormat('en',{calendar,era:'short',year:'numeric',timeZone:'UTC'});
            for (const year of [600,2025]) console.log(f.formatToParts(Date.UTC(year,5,15)).find(p=>p.type==='era').value);
        }
        for (const [calendar, year] of [['coptic',250],['coptic',2025],['japanese',1850],['japanese',-100]]) {
            const f = new Intl.DateTimeFormat('en',{calendar,era:'short',year:'numeric',timeZone:'UTC'});
            console.log(f.formatToParts(Date.UTC(year,5,15)).find(p=>p.type==='era').value);
        }
    "#,
        &["BH", "AH", "BH", "AH", "BH", "AH", "AM", "AM", "AD", "BC"],
    );
}

#[test]
fn locale_defaults_and_calendar_numbering_annotations() {
    check(
        r#"
        console.log(new Intl.DateTimeFormat('ja',{hour:'numeric',hour12:true}).resolvedOptions().hourCycle);
        console.log(new Intl.DateTimeFormat('ja',{timeStyle:'short',hour12:true,timeZone:'UTC'}).format(0));
        const japanese = new Intl.DateTimeFormat('ja',{calendar:'japanese',dateStyle:'long',timeZone:'UTC'});
        console.log(japanese.formatToParts(Date.UTC(2019,4,1)).find(p=>p.type==='year').value);
        const lunar = new Intl.DateTimeFormat('zh',{calendar:'chinese',dateStyle:'long',timeZone:'UTC'});
        for (const day of [1,10,11,20,21,30]) {
            // The lunar month beginning 2019-02-05 has all thirty days.
            console.log(lunar.formatToParts(Date.UTC(2019,1,4+day)).find(p=>p.type==='day').value);
        }
        const arabic = new Intl.DateTimeFormat('ar-EG',{year:'numeric',month:'numeric',day:'numeric',timeZone:'UTC'});
        console.log(arabic.resolvedOptions().numberingSystem);
        console.log(arabic.formatToParts(Date.UTC(2020,0,2)).filter(p=>p.type!=='literal').map(p=>p.value).join(','));
    "#,
        &[
            "h11",
            "午前0:00",
            "元",
            "初一",
            "初十",
            "十一",
            "二十",
            "廿一",
            "三十",
            "arab",
            "٢,١,٢٠٢٠",
        ],
    );
}
