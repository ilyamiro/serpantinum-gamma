//! The system clock in UTC and in local time. Local time is used only by the fallback schedule and
//! to say which day "today" is for sunrise and sunset; the sun itself goes by UTC.

use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Now {
    pub unix: f64,
    /// Minutes since local midnight, with fractions
    pub local_minutes: f64,
}

unsafe extern "C" {
    // POSIX, but missing from the libc crate
    fn tzset();
}

fn local_tm(unix: i64) -> libc::tm {
    // SAFETY: tzset and localtime_r only read the time zone settings and write to the given struct
    unsafe {
        // Picks up a changed time zone, as when travelling; localtime_r alone needn't
        tzset();
        let time = unix as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&time, &mut tm);
        tm
    }
}

pub fn now() -> Now {
    let unix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64();
    let tm = local_tm(unix.floor() as i64);
    let seconds = tm.tm_hour as f64 * 3600.0 + tm.tm_min as f64 * 60.0 + tm.tm_sec as f64 + unix.fract();
    Now { unix, local_minutes: seconds / 60.0 }
}

/// The local day containing `unix`, as `[start, end)`; 23 or 25 hours long when daylight saving
/// time starts or ends
pub fn local_day(unix: i64) -> (i64, i64) {
    let midnight = |day_offset: i32| {
        let mut tm = local_tm(unix);
        tm.tm_mday += day_offset;
        tm.tm_hour = 0;
        tm.tm_min = 0;
        tm.tm_sec = 0;
        tm.tm_isdst = -1;
        // SAFETY: mktime normalises the struct in place and reads the time zone settings
        unsafe { libc::mktime(&mut tm) as i64 }
    };
    (midnight(0), midnight(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn today_contains_now() {
        let now = now();
        let (start, end) = local_day(now.unix as i64);
        assert!(start <= now.unix as i64 && (now.unix as i64) < end);
        assert!((82800..=90000).contains(&(end - start)));
        assert!((0.0..1440.0).contains(&now.local_minutes));
    }

    #[test]
    fn days_follow_daylight_saving_time() {
        // The time zone belongs to the whole process, so the check runs in a child with its own
        const CHILD: &str = "SERPANTINUM_GAMMA_TZ_CHILD";
        if std::env::var_os(CHILD).is_some() {
            // New York: 23 hours when daylight saving time starts, 25 when it ends
            assert_eq!(local_day(1772985600), (1772946000, 1773028800));
            assert_eq!(local_day(1793552400), (1793505600, 1793595600));
            // 23:30 on the day it ends is still that day
            assert_eq!(local_day(1793595600 - 1800), (1793505600, 1793595600));
            return;
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "clock::tests::days_follow_daylight_saving_time"])
            // A POSIX rule rather than a zone name, so no time zone database is needed
            .env("TZ", "EST5EDT,M3.2.0,M11.1.0")
            .env(CHILD, "1")
            .status()
            .unwrap();
        assert!(status.success());
    }
}
