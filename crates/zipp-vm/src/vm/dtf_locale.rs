//! Date/time locale data. Other Intl services retain their own locale sets.
use super::{cldr_ar_eg, cldr_de, cldr_dtf_en, cldr_ja, cldr_zh};

#[derive(Clone, Copy)]
pub(crate) struct CalendarPatterns {
    pub id: &'static str,
    pub available_formats: &'static [(&'static str, &'static str)],
    pub interval_formats: &'static [(&'static str, &'static str, &'static str)],
    pub interval_fallback: &'static str,
    pub datetime_glue_at: [&'static str; 4],
}

pub(crate) struct DateTimeLocale {
    pub months_wide: [&'static str; 12],
    pub months_abbr: [&'static str; 12],
    pub months_narrow: [&'static str; 12],
    pub months_sa_wide: [&'static str; 12],
    pub months_sa_abbr: [&'static str; 12],
    pub months_sa_narrow: [&'static str; 12],
    pub days_wide: [&'static str; 7],
    pub days_abbr: [&'static str; 7],
    pub days_short: [&'static str; 7],
    pub days_narrow: [&'static str; 7],
    pub eras_wide: [&'static str; 2],
    pub eras_abbr: [&'static str; 2],
    pub eras_narrow: [&'static str; 2],
    pub cal_months: &'static [(
        &'static str,
        &'static [&'static str],
        &'static [&'static str],
        &'static [&'static str],
    )],
    pub cal_eras: &'static [(
        &'static str,
        &'static [&'static str],
        &'static [&'static str],
        &'static [&'static str],
    )],
    pub cal_date_formats: &'static [(&'static str, [&'static str; 4])],
    pub day_periods: &'static [(&'static str, &'static str, &'static str, &'static str)],
    pub day_period_rules: &'static [(&'static str, i32, i32, i32)],
    pub date_formats: [&'static str; 4],
    pub time_formats: [&'static str; 4],
    pub datetime_glue_at: [&'static str; 4],
    pub available_formats: &'static [(&'static str, &'static str)],
    pub append_items: &'static [(&'static str, &'static str, &'static str)],
    pub interval_formats: &'static [(&'static str, &'static str, &'static str)],
    pub interval_fallback: &'static str,
    pub sym_decimal: &'static str,
    pub utc_long: &'static str,
    pub hour_cycle: &'static str,
    pub hour_cycle12: &'static str,
    pub numbering_system: &'static str,
    pub calendar_patterns: &'static [CalendarPatterns],
    pub cyclic_years: &'static [(&'static str, &'static [&'static str])],
    pub leap_month_patterns: &'static [(&'static str, [&'static str; 4], [&'static str; 4])],
    pub cal_date_number_systems: &'static [(&'static str, [&'static str; 4])],
    pub japanese_first_year: &'static str,
    pub lunar_day_names: &'static [&'static str],
}

impl DateTimeLocale {
    pub fn patterns(&self, calendar: &str) -> CalendarPatterns {
        self.calendar_patterns
            .iter()
            .find(|p| p.id == calendar)
            .copied()
            .unwrap_or(CalendarPatterns {
                id: "gregory",
                available_formats: self.available_formats,
                interval_formats: self.interval_formats,
                interval_fallback: self.interval_fallback,
                datetime_glue_at: self.datetime_glue_at,
            })
    }
}

macro_rules! locale {
    ($module:ident) => {
        DateTimeLocale {
            months_wide: $module::MONTHS_WIDE,
            months_abbr: $module::MONTHS_ABBR,
            months_narrow: $module::MONTHS_NARROW,
            months_sa_wide: $module::MONTHS_SA_WIDE,
            months_sa_abbr: $module::MONTHS_SA_ABBR,
            months_sa_narrow: $module::MONTHS_SA_NARROW,
            days_wide: $module::DAYS_WIDE,
            days_abbr: $module::DAYS_ABBR,
            days_short: $module::DAYS_SHORT,
            days_narrow: $module::DAYS_NARROW,
            eras_wide: $module::ERAS_WIDE,
            eras_abbr: $module::ERAS_ABBR,
            eras_narrow: $module::ERAS_NARROW,
            cal_months: $module::CAL_MONTHS,
            cal_eras: $module::CAL_ERAS,
            cal_date_formats: $module::CAL_DATE_FORMATS,
            day_periods: $module::DAY_PERIODS,
            day_period_rules: $module::DAY_PERIOD_RULES,
            date_formats: $module::DATE_FORMATS,
            time_formats: $module::TIME_FORMATS,
            datetime_glue_at: $module::DATETIME_GLUE_AT,
            available_formats: $module::AVAILABLE_FORMATS,
            append_items: $module::APPEND_ITEMS,
            interval_formats: $module::INTERVAL_FORMATS,
            interval_fallback: $module::INTERVAL_FALLBACK,
            sym_decimal: $module::SYM_DECIMAL,
            utc_long: $module::UTC_LONG,
            hour_cycle: $module::DEFAULT_HOUR_CYCLE,
            hour_cycle12: $module::DEFAULT_HOUR_CYCLE12,
            numbering_system: $module::DEFAULT_NUMBERING_SYSTEM,
            calendar_patterns: $module::CAL_PATTERNS,
            cyclic_years: $module::CYCLIC_YEARS,
            leap_month_patterns: $module::LEAP_MONTH_PATTERNS,
            cal_date_number_systems: $module::CAL_DATE_NUMBER_SYSTEMS,
            japanese_first_year: $module::JAPANESE_FIRST_YEAR,
            lunar_day_names: $module::LUNAR_DAY_NAMES,
        }
    };
}

pub(crate) static EN: DateTimeLocale = locale!(cldr_dtf_en);
pub(crate) static DE: DateTimeLocale = locale!(cldr_de);
pub(crate) static JA: DateTimeLocale = locale!(cldr_ja);
pub(crate) static ZH: DateTimeLocale = locale!(cldr_zh);
pub(crate) static AR_EG: DateTimeLocale = locale!(cldr_ar_eg);

pub(crate) fn for_locale(locale: &str) -> &'static DateTimeLocale {
    match locale.split('-').next() {
        Some("de") => &DE,
        Some("ja") => &JA,
        Some("zh") => &ZH,
        Some("ar") if locale.starts_with("ar-EG") => &AR_EG,
        _ => &EN,
    }
}
