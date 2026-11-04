//! The automatic day and night schedule: by the sun's elevation when a location is known, otherwise
//! by fixed local clock times

use crate::clock::{self, Now};
use crate::color::{TEMP_MAX, TEMP_MIN};
use crate::solar;

/// At or above this elevation it is fully day
pub const DAY_ELEVATION: f64 = 2.0;
/// At or below this elevation it is fully night
pub const NIGHT_ELEVATION: f64 = -4.5;
/// Without a location, how long the fade after each fallback time takes
pub const FALLBACK_FADE_MINUTES: f64 = 30.0;
pub const MINUTES_PER_DAY: u16 = 1440;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Location {
    pub latitude: f64,
    pub longitude: f64,
}

impl Location {
    pub fn new(latitude: f64, longitude: f64) -> Option<Location> {
        let valid = latitude.is_finite()
            && longitude.is_finite()
            && (-90.0..=90.0).contains(&latitude)
            && (-180.0..=180.0).contains(&longitude);
        valid.then_some(Location { latitude, longitude })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Schedule {
    pub auto: bool,
    pub day_temperature: u16,
    pub night_temperature: u16,
    pub location: Option<Location>,
    /// Minutes after local midnight at which the fade to day starts, without a location
    pub fallback_day_start: u16,
    /// Minutes after local midnight at which the fade to night starts, without a location
    pub fallback_night_start: u16,
}

impl Default for Schedule {
    fn default() -> Self {
        Schedule {
            auto: false,
            day_temperature: 6500,
            night_temperature: 4000,
            location: None,
            fallback_day_start: 7 * 60,
            fallback_night_start: 19 * 60,
        }
    }
}

impl Schedule {
    /// 0 in full day, 1 in full night, in between while fading
    pub fn night_fraction(&self, now: &Now) -> f64 {
        match self.location {
            Some(location) => elevation_fraction(solar::elevation(now.unix, location.latitude, location.longitude)),
            None => clock_fraction(now.local_minutes, self.fallback_day_start as f64, self.fallback_night_start as f64),
        }
    }

    pub fn temperature(&self, now: &Now) -> u16 {
        blend(self.day_temperature, self.night_temperature, self.night_fraction(now))
    }

    /// What the Schedule interface shows at `now`
    pub fn view(&self, now: &Now) -> View {
        let (has_location, latitude, longitude, sunrise, sunset, sun_elevation) = match self.location {
            Some(Location { latitude, longitude }) => {
                let (start, end) = clock::local_day(now.unix.floor() as i64);
                let (rise, set) = solar::crossings(start, end, latitude, longitude, solar::SUNRISE_ELEVATION);
                let unix = |t: Option<i64>| t.map_or(0, |t| t as u64);
                (true, latitude, longitude, unix(rise), unix(set), solar::elevation(now.unix, latitude, longitude))
            }
            None => (false, 0.0, 0.0, 0, 0, 0.0),
        };
        View {
            auto: self.auto,
            day_temperature: self.day_temperature,
            night_temperature: self.night_temperature,
            has_location,
            latitude,
            longitude,
            fallback_day_start: self.fallback_day_start,
            fallback_night_start: self.fallback_night_start,
            sunrise,
            sunset,
            sun_elevation,
        }
    }
}

/// Every property of the Schedule interface
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub auto: bool,
    pub day_temperature: u16,
    pub night_temperature: u16,
    pub has_location: bool,
    pub latitude: f64,
    pub longitude: f64,
    pub fallback_day_start: u16,
    pub fallback_night_start: u16,
    /// Unix seconds of today's sunrise and sunset in local time, 0 for none or without a location
    pub sunrise: u64,
    pub sunset: u64,
    pub sun_elevation: f64,
}

fn smoothstep(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

pub fn elevation_fraction(elevation: f64) -> f64 {
    smoothstep((DAY_ELEVATION - elevation) / (DAY_ELEVATION - NIGHT_ELEVATION))
}

/// Without a location: fading to night for 30 minutes from `night_start`, to day from `day_start`
pub fn clock_fraction(minutes: f64, day_start: f64, night_start: f64) -> f64 {
    if day_start == night_start {
        return 0.0;
    }
    let day = MINUTES_PER_DAY as f64;
    let since_night = (minutes - night_start).rem_euclid(day);
    let since_day = (minutes - day_start).rem_euclid(day);
    if since_night < since_day {
        smoothstep(since_night / FALLBACK_FADE_MINUTES)
    } else {
        1.0 - smoothstep(since_day / FALLBACK_FADE_MINUTES)
    }
}

/// Mixes in mireds (a million over kelvin), where equal steps look equally large
pub fn blend(day: u16, night: u16, night_fraction: f64) -> u16 {
    let day = 1e6 / day as f64;
    let night = 1e6 / night as f64;
    let mired = day + (night - day) * night_fraction.clamp(0.0, 1.0);
    ((1e6 / mired).round() as u16).clamp(TEMP_MIN, TEMP_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(local_minutes: f64) -> Now {
        Now { unix: 0.0, local_minutes }
    }

    #[test]
    fn elevation_fraction_runs_from_day_to_night() {
        assert_eq!(elevation_fraction(30.0), 0.0);
        assert_eq!(elevation_fraction(DAY_ELEVATION), 0.0);
        assert_eq!(elevation_fraction(NIGHT_ELEVATION), 1.0);
        assert_eq!(elevation_fraction(-40.0), 1.0);
        let middle = (DAY_ELEVATION + NIGHT_ELEVATION) / 2.0;
        assert!((elevation_fraction(middle) - 0.5).abs() < 1e-12);
        let mut last = 0.0;
        for step in 0..=100 {
            let fraction = elevation_fraction(DAY_ELEVATION - step as f64 * 0.065);
            assert!(fraction >= last);
            last = fraction;
        }
    }

    #[test]
    fn blend_mixes_in_mireds() {
        assert_eq!(blend(6500, 4000, 0.0), 6500);
        assert_eq!(blend(6500, 4000, 1.0), 4000);
        // Halfway between 153.8 and 250 mireds is 201.9, which is 4952 K, not the 5250 K of kelvins
        assert_eq!(blend(6500, 4000, 0.5), 4952);
        assert_eq!(blend(3000, 3000, 0.5), 3000);
    }

    #[test]
    fn fallback_follows_the_clock() {
        let schedule = Schedule::default();
        let fraction = |minutes| schedule.night_fraction(&at(minutes));
        assert_eq!(fraction(12.0 * 60.0), 0.0);
        assert_eq!(fraction(19.0 * 60.0), 0.0);
        assert_eq!(fraction(19.0 * 60.0 + 15.0), 0.5);
        assert_eq!(fraction(19.0 * 60.0 + 30.0), 1.0);
        assert_eq!(fraction(0.0), 1.0);
        assert_eq!(fraction(7.0 * 60.0), 1.0);
        assert_eq!(fraction(7.0 * 60.0 + 15.0), 0.5);
        assert_eq!(fraction(7.0 * 60.0 + 30.0), 0.0);
    }

    #[test]
    fn fallback_wraps_around_midnight() {
        let schedule = Schedule { fallback_night_start: 23 * 60 + 50, fallback_day_start: 60, ..Schedule::default() };
        let fraction = |minutes| schedule.night_fraction(&at(minutes));
        assert_eq!(fraction(23.0 * 60.0), 0.0);
        assert_eq!(fraction(5.0), 0.5); // 15 minutes into the fade that began at 23:50
        assert_eq!(fraction(30.0), 1.0);
        assert_eq!(fraction(60.0 + 15.0), 0.5);
        assert_eq!(fraction(12.0 * 60.0), 0.0);
    }

    #[test]
    fn fallback_with_equal_times_stays_day() {
        let schedule = Schedule { fallback_day_start: 600, fallback_night_start: 600, ..Schedule::default() };
        assert_eq!(schedule.night_fraction(&at(600.0)), 0.0);
        assert_eq!(schedule.night_fraction(&at(0.0)), 0.0);
    }

    #[test]
    fn location_overrides_the_clock() {
        // Moscow at 13:44 UTC on 2026-11-04: the sun at -0.87°, a little over a third into the fade
        let schedule = Schedule { location: Location::new(55.7558, 37.6173), ..Schedule::default() };
        let now = Now { unix: 1793799840.0, local_minutes: 12.0 * 60.0 };
        let expected = elevation_fraction(-0.8685);
        assert!((schedule.night_fraction(&now) - expected).abs() < 0.01);
        assert!((0.3..0.5).contains(&expected));
        // Tokyo at the same moment is in deep night
        let tokyo = Schedule { location: Location::new(35.6762, 139.6503), ..schedule };
        assert_eq!(tokyo.temperature(&now), 4000);
    }

    #[test]
    fn locations_out_of_range_are_refused() {
        assert!(Location::new(91.0, 0.0).is_none());
        assert!(Location::new(0.0, -180.5).is_none());
        assert!(Location::new(f64::NAN, 0.0).is_none());
        assert!(Location::new(-90.0, 180.0).is_some());
    }
}
