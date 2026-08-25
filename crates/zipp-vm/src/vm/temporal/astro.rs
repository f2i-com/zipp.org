//! The solar and lunar positions the `chinese`/`dangi` calendars are defined by.
//!
//! Every other calendar in this engine is arithmetic: its year and month lengths
//! are a closed-form function of the year. The Chinese calendar is not. Its
//! months begin on the DAY OF A TRUE NEW MOON in the China (or Korea) time zone,
//! and its leap months are the months that contain no *zhongqi* — no instant at
//! which the apparent solar longitude is a multiple of 30°. Both are genuinely
//! astronomical quantities, so this file computes them, and there is no table to
//! fall back on: test262 exercises years far outside any tabulated window
//! (`PlainDate/from/extreme-dates.js` constructs chinese ±250000).
//!
//! The series are the standard truncated ones published in Reingold &
//! Dershowitz, *Calendrical Calculations* (4th ed.), which are in turn:
//!
//! * `solar_longitude` — the 49-term apparent-longitude series of Bretagnon &
//!   Simon reproduced by Meeus, *Astronomical Algorithms*, ch. 25, plus that
//!   chapter's nutation and aberration corrections.
//! * `nth_new_moon` — Meeus, ch. 49 (Phases of the Moon): the mean lunation
//!   polynomial plus 24 periodic terms, the 8 "additional" planetary terms and
//!   the free term in Ω.
//! * `ephemeris_correction` — ΔT, the Espenak & Meeus polynomial set.
//!
//! Accuracy actually measured against published values, since a Chinese calendar
//! that is *nearly* right is worse than none: the four 2024 equinoxes/solstices
//! come out within 31 s of the Astronomical Almanac's, the 2000 and 1977 new
//! moons within 20 s of Meeus's worked examples. The decisions this feeds —
//! "which side of local midnight" — are made on margins of minutes, so that
//! margin matters and is asserted in the tests at the bottom of `calendar.rs`.
//!
//! Everything here is in R.D. (rata die) moments: day 1.0 = 0001-01-01T00:00
//! proleptic Gregorian, fractions of a day are fractions of a UT day.

/// R.D. moment of 2000-01-01T12:00 UT — the J2000.0 epoch of the series below.
const J2000: f64 = 730_120.5;

/// Mean synodic month (Meeus ch. 49), in days.
pub(crate) const MEAN_SYNODIC_MONTH: f64 = 29.530_588_861;

/// Mean tropical year, in days — only used to *estimate* where a solar longitude
/// was last attained, never as a calendar quantity.
const MEAN_TROPICAL_YEAR: f64 = 365.242_189;

/// R.D. of 1970-01-01, the origin of this engine's `epoch_days`.
pub(crate) const RD_AT_EPOCH: i64 = 719_163;

fn poly(x: f64, c: &[f64]) -> f64 {
    c.iter().rev().fold(0.0, |acc, k| acc * x + k)
}

fn sin_deg(x: f64) -> f64 {
    (x % 360.0).to_radians().sin()
}

fn cos_deg(x: f64) -> f64 {
    (x % 360.0).to_radians().cos()
}

/// Proleptic-Gregorian year of an R.D. day. Only the year is ever needed here
/// (ΔT and the zone rules are keyed on it), so this is the civil-calendar
/// inverse without the month/day work.
fn gregorian_year(rd: i64) -> i64 {
    // Days since 0000-03-01 in the 400-year cycle, then unwind the cycle.
    // R.D. 1 (0001-01-01) is day 306 of the year that opened on 0000-03-01.
    let d = rd + 305;
    let era = d.div_euclid(146_097);
    let doe = d.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let y = yoe + era * 400;
    // doy counts from 1 March; January and February belong to the next year.
    if doy >= 306 {
        y + 1
    } else {
        y
    }
}

/// R.D. of `y`-01-01 proleptic Gregorian.
fn gregorian_new_year(y: i64) -> i64 {
    let ym = y - 1;
    365 * ym + ym.div_euclid(4) - ym.div_euclid(100) + ym.div_euclid(400) + 1
}

/// ΔT = TT − UT, in days: the Espenak & Meeus polynomial set (as tabulated in
/// *Calendrical Calculations* 4th ed. §14.2). The lunar and solar series below
/// are expressed in dynamical time, but a calendar day boundary is a *civil*
/// one, so every result has to be brought back to UT — for 1900–2100 that is
/// 20–200 s, which is the same order as the margins that decide a month
/// boundary, so it cannot be dropped.
fn ephemeris_correction(t: f64) -> f64 {
    let year = gregorian_year(t.floor() as i64);
    let c = (gregorian_new_year(year) + 181 - gregorian_new_year(1900)) as f64 / 36525.0;
    let y = (year - 2000) as f64;
    match year {
        2051..=2150 => {
            (-20.0 + 32.0 * ((year - 1820) as f64 / 100.0).powi(2) + 0.5628 * (2150 - year) as f64)
                / 86400.0
        }
        2006..=2050 => poly(y, &[62.92, 0.322_17, 0.005_589]) / 86400.0,
        1986..=2005 => {
            poly(
                y,
                &[
                    63.86,
                    0.3345,
                    -0.060_374,
                    0.001_727_5,
                    0.000_651_814,
                    0.000_023_735_99,
                ],
            ) / 86400.0
        }
        1900..=1985 => poly(
            c,
            &[
                -0.00002, 0.000_297, 0.025_184, -0.181_133, 0.553_040, -0.861_938, 0.677_066,
                -0.212_591,
            ],
        ),
        1800..=1899 => poly(
            c,
            &[
                -0.000_009, 0.003_844, 0.083_563, 0.865_736, 4.867_575, 15.845_535, 31.332_267,
                38.291_999, 28.316_289, 11.636_204, 2.043_794,
            ],
        ),
        1700..=1799 => {
            poly(
                (year - 1700) as f64,
                &[
                    8.118_780_842,
                    -0.005_092_142,
                    0.003_336_121,
                    -0.000_026_6484,
                ],
            ) / 86400.0
        }
        1600..=1699 => {
            poly(
                (year - 1600) as f64,
                &[120.0, -0.9808, -0.015_32, 1.0 / 7129.0],
            ) / 86400.0
        }
        500..=1599 => {
            poly(
                (year - 1000) as f64 / 100.0,
                &[
                    1574.2,
                    -556.01,
                    71.234_72,
                    0.319_781,
                    -0.850_346_3,
                    -0.005_050_998,
                    0.008_357_2073,
                ],
            ) / 86400.0
        }
        -499..=499 => {
            poly(
                year as f64 / 100.0,
                &[
                    10583.6,
                    -1014.41,
                    33.783_11,
                    -5.952_053,
                    -0.179_845_2,
                    0.022_174_192,
                    0.009_031_6521,
                ],
            ) / 86400.0
        }
        _ => {
            let x = 0.5 + (gregorian_new_year(year) - gregorian_new_year(1810)) as f64;
            (x * x / 41_048_480.0 - 15.0) / 86400.0
        }
    }
}

fn dynamical_from_universal(t: f64) -> f64 {
    t + ephemeris_correction(t)
}

fn universal_from_dynamical(t: f64) -> f64 {
    t - ephemeris_correction(t)
}

fn julian_centuries(t: f64) -> f64 {
    (dynamical_from_universal(t) - J2000) / 36525.0
}

// ── Apparent solar longitude ────────────────────────────────────────────────

#[rustfmt::skip]
const SOLAR_COEFF: [f64; 49] = [
    403406.0, 195207.0, 119433.0, 112392.0, 3891.0, 2819.0, 1721.0, 660.0, 350.0, 334.0,
    314.0, 268.0, 242.0, 234.0, 158.0, 132.0, 129.0, 114.0, 99.0, 93.0,
    86.0, 78.0, 72.0, 68.0, 64.0, 46.0, 38.0, 37.0, 32.0, 29.0,
    28.0, 27.0, 27.0, 25.0, 24.0, 21.0, 21.0, 20.0, 18.0, 17.0,
    14.0, 13.0, 13.0, 13.0, 12.0, 10.0, 10.0, 10.0, 10.0,
];
#[rustfmt::skip]
const SOLAR_MULT: [f64; 49] = [
    0.928_789_2, 35999.137_695_8, 35999.408_966_6, 35998.728_738_5, 71998.202_61,
    71998.4403, 36000.357_26, 71997.4812, 32964.4678, -19.4410,
    445267.1117, 45036.8840, 3.1008, 22518.4434, -19.9739,
    65928.9345, 9038.0293, 3034.7684, 33718.148, 3034.448,
    -2280.773, 29929.992, 31556.493, 149.588, 9037.750,
    107997.405, -4444.176, 151.771, 67555.316, 31556.080,
    -4561.540, 107996.706, 1221.655, 62894.167, 31437.369,
    14578.298, -31931.757, 34777.243, 1221.999, 62894.511,
    -4442.039, 107997.909, 119.066, 16859.071, -4.578,
    26895.292, -39.127, 12297.536, 90073.778,
];
#[rustfmt::skip]
const SOLAR_ADD: [f64; 49] = [
    270.548_61, 340.191_28, 63.918_54, 331.262_20, 317.843, 86.631, 240.052, 310.26, 247.23, 260.87,
    297.82, 343.14, 166.79, 81.53, 3.50, 132.75, 182.95, 162.03, 29.8, 266.4,
    249.2, 157.6, 257.8, 185.1, 69.9, 8.0, 197.1, 250.4, 65.3, 162.7,
    341.5, 291.6, 98.5, 146.7, 110.0, 5.2, 342.6, 230.9, 256.1, 45.3,
    242.9, 115.2, 151.8, 285.3, 53.3, 126.6, 205.7, 85.9, 146.1,
];

fn nutation(t: f64) -> f64 {
    let c = julian_centuries(t);
    let a = poly(c, &[124.90, -1934.134, 0.002_063]);
    let b = poly(c, &[201.11, 72001.5377, 0.000_57]);
    -0.004_778 * sin_deg(a) - 0.000_366_7 * sin_deg(b)
}

fn aberration(t: f64) -> f64 {
    let c = julian_centuries(t);
    0.000_097_4 * cos_deg(177.63 + 35999.018_48 * c) - 0.005_575
}

/// Apparent solar longitude at UT moment `t`, in degrees [0, 360).
pub(crate) fn solar_longitude(t: f64) -> f64 {
    let c = julian_centuries(t);
    let mut s = 0.0;
    for i in 0..49 {
        s += SOLAR_COEFF[i] * sin_deg(SOLAR_ADD[i] + SOLAR_MULT[i] * c);
    }
    // The series is in units of 1e-7 radians; 5.7295779513e-6 = 1e-7 * 180/pi.
    let lambda = 282.777_183_4 + 36000.769_537_44 * c + 0.000_005_729_577_951_308_232 * s;
    (lambda + aberration(t) + nutation(t)).rem_euclid(360.0)
}

/// A first guess at the UT moment at or before `t` when the sun last had
/// longitude `lambda`, by inverting the mean motion twice. The caller refines it
/// day by day; a guess is enough, and it stays finite for any `t`.
pub(crate) fn estimate_prior_solar_longitude(lambda: f64, t: f64) -> f64 {
    let rate = MEAN_TROPICAL_YEAR / 360.0;
    let tau = t - rate * (solar_longitude(t) - lambda).rem_euclid(360.0);
    let delta = (solar_longitude(tau) - lambda + 180.0).rem_euclid(360.0) - 180.0;
    (tau - rate * delta).min(t)
}

// ── New moons ───────────────────────────────────────────────────────────────

#[rustfmt::skip]
const NM_E_FACTOR: [i32; 24] = [0,1,0,0,1,1,2,0,0,1,0,1,1,1,0,0,0,0,0,0,0,0,0,0];
#[rustfmt::skip]
const NM_SOLAR: [f64; 24] = [0.,1.,0.,0.,-1.,1.,2.,0.,0.,1.,0.,1.,1.,-1.,2.,0.,3.,1.,0.,1.,-1.,-1.,1.,0.];
#[rustfmt::skip]
const NM_LUNAR: [f64; 24] = [1.,0.,2.,0.,1.,1.,0.,1.,1.,2.,3.,0.,0.,2.,1.,2.,0.,1.,2.,1.,1.,1.,3.,4.];
#[rustfmt::skip]
const NM_MOON: [f64; 24] = [0.,0.,0.,2.,0.,0.,0.,-2.,2.,0.,0.,2.,-2.,0.,0.,-2.,0.,-2.,2.,2.,2.,-2.,0.,0.];
#[rustfmt::skip]
const NM_SINE: [f64; 24] = [
    -0.40720, 0.17241, 0.01608, 0.01039, 0.00739, -0.00514, 0.00208, -0.00111,
    -0.00057, 0.00056, -0.00042, 0.00042, 0.00038, -0.00024, -0.00007, 0.00004,
    0.00004, 0.00003, 0.00003, -0.00003, 0.00003, -0.00002, -0.00002, 0.00002,
];
#[rustfmt::skip]
const NM_ADD_CONST: [f64; 13] = [
    251.88, 251.83, 349.42, 84.66, 141.74, 207.14, 154.84, 34.52, 207.19, 291.34,
    161.72, 239.56, 331.55,
];
#[rustfmt::skip]
const NM_ADD_COEFF: [f64; 13] = [
    0.016_321, 26.651_886, 36.412_478, 18.206_239, 53.303_771, 2.453_732, 7.306_860,
    27.261_239, 0.121_824, 1.844_379, 24.198_154, 25.513_099, 3.592_518,
];
#[rustfmt::skip]
const NM_ADD_FACTOR: [f64; 13] = [
    0.000_165, 0.000_164, 0.000_126, 0.000_110, 0.000_062, 0.000_060, 0.000_056, 0.000_047,
    0.000_042, 0.000_040, 0.000_037, 0.000_035, 0.000_023,
];

/// Mean (purely polynomial) new moon at lunation `n`, in dynamical time, with
/// `n` real so it can be inverted by Newton. Its derivative is the synodic month
/// to within a part in 10^4 out to ±250000 years, so it is strictly increasing —
/// which `new_moon_index_before` relies on.
fn mean_new_moon(n: f64) -> f64 {
    let c = (n - 24_724.0) / 1236.85;
    J2000
        + poly(
            c,
            &[
                5.097_66,
                MEAN_SYNODIC_MONTH * 1236.85,
                0.000_154_37,
                -0.000_000_15,
                0.000_000_000_73,
            ],
        )
}

/// UT moment of the `n`-th new moon, `n = 0` being the one in January 1900
/// (R&D's indexing: `n − 24724` is Meeus's `k`, counted from January 2000).
pub(crate) fn nth_new_moon(n: i64) -> f64 {
    let k = (n - 24_724) as f64;
    let c = k / 1236.85;
    let e = poly(c, &[1.0, -0.002_516, -0.000_007_4]);
    let solar_anomaly = poly(
        c,
        &[2.5534, 1236.85 * 29.105_356_7, -0.000_001_4, -0.000_000_11],
    );
    let lunar_anomaly = poly(
        c,
        &[
            201.5643,
            385.816_935_28 * 1236.85,
            0.010_758_2,
            0.000_012_38,
            -0.000_000_058,
        ],
    );
    let moon_argument = poly(
        c,
        &[
            160.7108,
            390.670_502_84 * 1236.85,
            -0.001_611_8,
            -0.000_002_27,
            0.000_000_011,
        ],
    );
    let omega = poly(
        c,
        &[124.7746, -1.563_755_88 * 1236.85, 0.002_067_2, 0.000_002_15],
    );
    let mut correction = -0.00017 * sin_deg(omega);
    for i in 0..24 {
        correction += NM_SINE[i]
            * e.powi(NM_E_FACTOR[i])
            * sin_deg(
                NM_SOLAR[i] * solar_anomaly
                    + NM_LUNAR[i] * lunar_anomaly
                    + NM_MOON[i] * moon_argument,
            );
    }
    correction += 0.000_325 * sin_deg(poly(c, &[299.77, 132.847_584_8, -0.009_173]));
    for i in 0..13 {
        correction += NM_ADD_FACTOR[i] * sin_deg(NM_ADD_CONST[i] + NM_ADD_COEFF[i] * k);
    }
    // The periodic part of the series never exceeds ±0.7 d where the series is
    // valid, but its E-factor is a polynomial in the century that explodes tens
    // of millennia out, and a non-monotone new-moon sequence would put a search
    // loop into an infinite regress. Clamping keeps the sequence strictly
    // increasing at absurd dates and provably never fires inside the range this
    // calendar claims (`new_moon_series_correction_stays_small` pins that).
    universal_from_dynamical(mean_new_moon(n as f64) + correction.clamp(-1.0, 1.0))
}

/// Index of the last new moon strictly before UT moment `t`.
pub(crate) fn new_moon_index_before(t: f64) -> i64 {
    // Invert the mean-lunation polynomial by Newton, in dynamical time so the
    // ΔT of an extreme year (which can reach thousands of days) does not leave
    // the correction loop below with a hundred months to walk.
    let td = dynamical_from_universal(t);
    let mut k = 24_724.0 + (td - J2000 - 5.097_66) / MEAN_SYNODIC_MONTH;
    for _ in 0..4 {
        k -= (mean_new_moon(k) - td) / MEAN_SYNODIC_MONTH;
    }
    let mut n = k.round().clamp(-4.0e6, 4.0e6) as i64;
    // The estimate is within a month or two even at the extremes; the bound is
    // only there so a pathological argument cannot spin.
    for _ in 0..64 {
        if nth_new_moon(n) < t {
            break;
        }
        n -= 1;
    }
    for _ in 0..64 {
        if nth_new_moon(n + 1) >= t {
            break;
        }
        n += 1;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rd(y: i64, m: i64, d: i64) -> f64 {
        // Proleptic Gregorian -> R.D., via the JDN formula.
        let a = (14 - m) / 12;
        let yy = y + 4800 - a;
        let mm = m + 12 * a - 3;
        (d + (153 * mm + 2) / 5 + 365 * yy + yy / 4 - yy / 100 + yy / 400 - 32045 - 1_721_425)
            as f64
    }

    /// The year extractor is the one ΔT and the zone rules are keyed on, so a
    /// day-boundary slip there would silently move a month.
    #[test]
    fn gregorian_year_boundaries() {
        for (y, m, d) in [
            (1900, 1, 1),
            (1900, 12, 31),
            (1929, 1, 1),
            (1970, 1, 1),
            (2000, 2, 29),
            (2100, 12, 31),
            (1, 1, 1),
            (0, 12, 31),
            (-1, 1, 1),
            (-4713, 1, 1),
        ] {
            assert_eq!(gregorian_year(rd(y, m, d) as i64), y, "{y}-{m}-{d}");
        }
        assert_eq!(gregorian_new_year(1970), 719_163);
    }

    /// Solar longitude, against the Astronomical Almanac's equinox and solstice
    /// instants. These decide which lunar month holds a zhongqi, and the
    /// closest call in 1900–2100 (chinese 1987) is 9m42s from midnight, so
    /// anything worse than a minute or two here would be unusable.
    #[test]
    fn solar_longitude_matches_published_equinoxes() {
        // (target longitude, published UT instant, tolerance in seconds)
        let cases = [
            (
                0.0,
                rd(2024, 3, 20) + (3.0 * 3600.0 + 6.0 * 60.0 + 21.0) / 86400.0,
            ),
            (
                90.0,
                rd(2024, 6, 20) + (20.0 * 3600.0 + 50.0 * 60.0 + 56.0) / 86400.0,
            ),
            (
                180.0,
                rd(2024, 9, 22) + (12.0 * 3600.0 + 43.0 * 60.0 + 36.0) / 86400.0,
            ),
            (
                270.0,
                rd(2024, 12, 21) + (9.0 * 3600.0 + 20.0 * 60.0 + 34.0) / 86400.0,
            ),
            (
                0.0,
                rd(2000, 3, 20) + (7.0 * 3600.0 + 35.0 * 60.0 + 15.0) / 86400.0,
            ),
            (
                270.0,
                rd(2000, 12, 21) + (13.0 * 3600.0 + 37.0 * 60.0 + 30.0) / 86400.0,
            ),
        ];
        for (lambda, published) in cases {
            let mut lo = published - 1.0;
            let mut hi = published + 1.0;
            for _ in 0..60 {
                let mid = (lo + hi) / 2.0;
                if (solar_longitude(mid) - lambda + 180.0).rem_euclid(360.0) - 180.0 < 0.0 {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            let err = ((lo + hi) / 2.0 - published).abs() * 86400.0;
            assert!(err < 40.0, "longitude {lambda}: off by {err:.1} s");
        }
    }

    /// New moons, against Meeus's own worked example (ch. 49, k = −283) and two
    /// published instants. Month starts turn on these to within minutes.
    #[test]
    fn new_moon_matches_published_instants() {
        let cases = [
            // 1977-02-18 03:37:42 TD in Meeus 49.a; DeltaT was then ~48 s.
            (rd(1977, 2, 18) + (3.0 * 3600.0 + 36.0 * 60.0 + 54.0) / 86400.0),
            (rd(2000, 1, 6) + (18.0 * 3600.0 + 14.0 * 60.0) / 86400.0),
            (rd(2024, 1, 11) + (11.0 * 3600.0 + 57.0 * 60.0) / 86400.0),
            (rd(1999, 1, 17) + (15.0 * 3600.0 + 46.0 * 60.0) / 86400.0),
            (rd(2018, 11, 7) + (16.0 * 3600.0 + 2.0 * 60.0) / 86400.0),
        ];
        for published in cases {
            let n = new_moon_index_before(published + 0.5);
            let err = (nth_new_moon(n) - published).abs() * 86400.0;
            assert!(
                err < 90.0,
                "new moon at R.D. {published}: off by {err:.1} s"
            );
        }
    }

    /// Two properties the search loops depend on: `nth_new_moon` is strictly
    /// increasing, and `new_moon_index_before` really inverts it. Checked over
    /// 1500–2500 (every year the calendar layer can reach for a value it
    /// claims) and then out at ±250000 years, where `extreme-dates.js` only
    /// asks that construction terminate.
    #[test]
    fn new_moon_sequence_is_monotone_and_invertible() {
        for &(lo, hi) in &[
            (-16_000i64, 13_000i64),
            (-3_100_000, -3_099_900),
            (3_099_900, 3_100_000),
        ] {
            let mut prev = nth_new_moon(lo);
            for n in lo + 1..hi {
                let t = nth_new_moon(n);
                assert!(t > prev + 25.0, "new moon {n} not after {}", n - 1);
                assert_eq!(new_moon_index_before(t + 0.001), n, "inverse at {n}");
                prev = t;
            }
        }
    }

    /// The monotonicity clamp in `nth_new_moon` must be inert wherever the
    /// calendar claims to be right — if it ever fired there it would be moving a
    /// real month boundary. Equivalently: the true series stays within ±0.7 d of
    /// the mean, so a 1-day clamp cannot bite.
    #[test]
    fn new_moon_series_correction_stays_small() {
        // Lunation -800..12500 spans R.D. years ~1836 BCE to 2900 CE.
        for n in -800..12_500 {
            let dev =
                nth_new_moon(n) + ephemeris_correction(nth_new_moon(n)) - mean_new_moon(n as f64);
            assert!(
                dev.abs() < 0.9,
                "lunation {n} deviates {dev} d from the mean"
            );
        }
    }
}
