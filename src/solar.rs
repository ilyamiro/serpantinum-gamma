//! The sun's position, by the equations of NOAA's solar calculator (after Meeus, "Astronomical
//! Algorithms"). The elevation is geometric (no refraction) and good to about 0.01° for dates
//! within a few centuries of 2000.

/// The elevation of the sun's centre at the standard sunrise and sunset: 34′ of refraction plus 16′
/// of the sun's radius below the horizon
pub const SUNRISE_ELEVATION: f64 = -0.833;

fn sin(degrees: f64) -> f64 {
    degrees.to_radians().sin()
}

fn cos(degrees: f64) -> f64 {
    degrees.to_radians().cos()
}

/// Degrees above the horizon of the sun's centre at `unix` seconds, seen from the given place
pub fn elevation(unix: f64, latitude: f64, longitude: f64) -> f64 {
    let julian_century = (unix / 86400.0 + 2440587.5 - 2451545.0) / 36525.0;
    let t = julian_century;

    let mean_longitude = (280.46646 + t * (36000.76983 + t * 0.0003032)).rem_euclid(360.0);
    let mean_anomaly = 357.52911 + t * (35999.05029 - 0.0001537 * t);
    let eccentricity = 0.016708634 - t * (0.000042037 + 0.0000001267 * t);
    let center = sin(mean_anomaly) * (1.914602 - t * (0.004817 + 0.000014 * t))
        + sin(2.0 * mean_anomaly) * (0.019993 - 0.000101 * t)
        + sin(3.0 * mean_anomaly) * 0.000289;
    let omega = 125.04 - 1934.136 * t;
    let apparent_longitude = mean_longitude + center - 0.00569 - 0.00478 * sin(omega);
    let mean_obliquity = 23.0 + (26.0 + (21.448 - t * (46.815 + t * (0.00059 - t * 0.001813))) / 60.0) / 60.0;
    let obliquity = mean_obliquity + 0.00256 * cos(omega);
    let declination = (sin(obliquity) * sin(apparent_longitude)).asin().to_degrees();

    // The equation of time, in minutes: how far the sun runs ahead of or behind the clock
    let y = (obliquity / 2.0).to_radians().tan().powi(2);
    let l0 = mean_longitude.to_radians();
    let m = mean_anomaly.to_radians();
    let e = eccentricity;
    let equation_of_time = 4.0
        * (y * (2.0 * l0).sin() - 2.0 * e * m.sin() + 4.0 * e * y * m.sin() * (2.0 * l0).cos()
            - 0.5 * y * y * (4.0 * l0).sin()
            - 1.25 * e * e * (2.0 * m).sin())
        .to_degrees();

    let utc_minutes = unix.rem_euclid(86400.0) / 60.0;
    let solar_minutes = (utc_minutes + equation_of_time + 4.0 * longitude).rem_euclid(1440.0);
    let hour_angle = solar_minutes / 4.0 - 180.0;
    let cos_zenith = sin(latitude) * sin(declination) + cos(latitude) * cos(declination) * cos(hour_angle);
    90.0 - cos_zenith.clamp(-1.0, 1.0).acos().to_degrees()
}

/// The first moments in `[start, end)` at which the sun rises above and sets below `threshold`,
/// to the second; `None` for one that doesn't happen, as in polar day and night
pub fn crossings(start: i64, end: i64, latitude: f64, longitude: f64, threshold: f64) -> (Option<i64>, Option<i64>) {
    // The sun never crosses a threshold twice within five minutes, except grazing it near the poles
    const STEP: i64 = 300;
    let height = |t: i64| elevation(t as f64, latitude, longitude) - threshold;
    let (mut rise, mut set) = (None, None);
    let (mut t0, mut h0) = (start, height(start));
    while t0 < end {
        let t1 = (t0 + STEP).min(end);
        let h1 = height(t1);
        if (h0 < 0.0) != (h1 < 0.0) {
            let (mut low, mut high, mut h_low) = (t0, t1, h0);
            while high - low > 1 {
                let middle = low + (high - low) / 2;
                let h_middle = height(middle);
                if (h_middle < 0.0) == (h_low < 0.0) {
                    (low, h_low) = (middle, h_middle);
                } else {
                    high = middle;
                }
            }
            if high < end {
                if h0 < 0.0 {
                    rise.get_or_insert(high);
                } else {
                    set.get_or_insert(high);
                }
            }
        }
        (t0, h0) = (t1, h1);
    }
    (rise, set)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Generated with PyEphem 4.2.1 (pressure 0, so geometric; sunrise and sunset for the sun's
    // centre at -0°50′), an independent implementation of the full theory

    // (latitude, longitude, unix time, geometric elevation in degrees)
    const ELEVATIONS: &[(f64, f64, i64, f64)] = &[
        (55.7558, 37.6173, 1782064800, 0.9721),     // Moscow, 2026-06-21 18:00 UTC
        (-33.8688, 151.2093, 1797840000, 11.4685),  // Sydney, 2026-12-21 08:00 UTC
        (64.1466, -21.9426, 1774008000, 23.4677),   // Reykjavik, 2026-03-20 12:00 UTC
        (69.6492, 18.9553, 1797850800, -3.1435),    // Tromsø at noon in polar night, 2026-12-21 11:00 UTC
        (-0.1807, -78.4678, 1790182800, 88.4545),   // Quito, almost overhead, 2026-09-23 17:00 UTC
        (21.3069, -157.8583, 1793761200, 11.1023),  // Honolulu, 2026-11-04 03:00 UTC
        (-36.8485, 174.7633, 1768518000, 64.7421),  // Auckland, 2026-01-15 23:00 UTC
        (90.0000, 0.0000, 1782000000, 23.4353),     // The North Pole, 2026-06-21 00:00 UTC
        (61.2181, -149.9003, 1802289600, 10.0228),  // Anchorage, 2027-02-10 20:00 UTC
        (40.7128, -74.0060, 1909438200, 9.3966),    // New York, 2030-07-04 23:30 UTC
        (51.5074, -0.1278, 946728000, 15.4531),     // London, 2000-01-01 12:00 UTC
        (-77.8500, 166.6700, 1782885600, -19.6227), // McMurdo, 2026-07-01 06:00 UTC
        (55.7558, 37.6173, 1793799840, -0.8685),    // Moscow, 2026-11-04 13:44 UTC
        (35.6762, 139.6503, 1793799840, -67.8294),  // Tokyo, 2026-11-04 13:44 UTC
    ];

    // (latitude, longitude, local day start, local day end, sunrise, sunset); 0 = none that day
    const EVENTS: &[(f64, f64, i64, i64, i64, i64)] = &[
        (55.7558, 37.6173, 1781989200, 1782075600, 1782002672, 1782065886), // Moscow, 2026-06-21
        (55.7558, 37.6173, 1797800400, 1797886800, 1797832647, 1797857857), // Moscow, 2026-12-21
        (-33.8688, 151.2093, 1797771600, 1797858000, 1797792038, 1797843924), // Sydney, 2026-12-21
        (69.6492, 18.9553, 1797807600, 1797894000, 0, 0),                   // Tromsø, polar night, 2026-12-21
        (69.6492, 18.9553, 1781992800, 1782079200, 0, 0),                   // Tromsø, midnight sun, 2026-06-21
        (64.1466, -21.9426, 1773964800, 1774051200, 1773991717, 1774035804), // Reykjavik, 2026-03-20
        (-0.1807, -78.4678, 1790139600, 1790226000, 1790161375, 1790204964), // Quito, 2026-09-23
        (40.7128, -74.0060, 1772946000, 1773028800, 1772968732, 1773010510), // New York, 23 hours long: DST starts, 2026-03-08
        (-36.8485, 174.7633, 1768388400, 1768474800, 1768411065, 1768462930), // Auckland, 2026-01-15
        (21.3069, -157.8583, 1793786400, 1793872800, 1793810168, 1793850817), // Honolulu, 2026-11-04
    ];

    #[test]
    fn elevation_matches_pyephem() {
        for &(latitude, longitude, unix, expected) in ELEVATIONS {
            let actual = elevation(unix as f64, latitude, longitude);
            assert!(
                (actual - expected).abs() < 0.03,
                "at ({latitude}, {longitude}) and {unix}: {actual:.4}° but PyEphem says {expected:.4}°"
            );
        }
    }

    #[test]
    fn sunrise_and_sunset_match_pyephem_within_a_minute() {
        for &(latitude, longitude, start, end, rise, set) in EVENTS {
            let (actual_rise, actual_set) = crossings(start, end, latitude, longitude, SUNRISE_ELEVATION);
            for (what, actual, expected) in [("sunrise", actual_rise, rise), ("sunset", actual_set, set)] {
                match (actual, expected) {
                    (None, 0) => {}
                    (Some(actual), expected) if expected != 0 => assert!(
                        (actual - expected).abs() <= 60,
                        "{what} at ({latitude}, {longitude}) on the day from {start}: {actual} but PyEphem says {expected}"
                    ),
                    _ => panic!(
                        "{what} at ({latitude}, {longitude}) on the day from {start}: {actual:?} but PyEphem says {expected}"
                    ),
                }
            }
        }
    }

    #[test]
    fn elevation_repeats_daily_and_peaks_at_noon() {
        // Quito at the September equinox: the sun passes nearly overhead and sets about 12 hours later
        let noon = 1790182800.0 + 6.0 * 60.0;
        assert!(elevation(noon, -0.1807, -78.4678) > 88.0);
        assert!(elevation(noon + 6.0 * 3600.0, -0.1807, -78.4678).abs() < 2.0);
        assert!(elevation(noon + 12.0 * 3600.0, -0.1807, -78.4678) < -85.0);
    }
}
