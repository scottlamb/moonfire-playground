// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2016-2020 The Moonfire NVR Authors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// In addition, as a special exception, the copyright holders give
// permission to link the code of portions of this program with the
// OpenSSL library under certain conditions as described in each
// individual source file, and distribute linked combinations including
// the two.
//
// You must obey the GNU General Public License in all respects for all
// of the code used other than OpenSSL. If you modify file(s) with this
// exception, you may extend this exception to your version of the
// file(s), but you are not obligated to do so. If you do not wish to do
// so, delete this exception statement from your version. If you delete
// this exception statement from all source files in the program, then
// also delete it here.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

//! Recording time and duration stuff.
//!
//! Copy'n'pasted from the server-side implementation (moonfire-nvr/db/recording.rs).
//! TODO: extract to a common crate.

use failure::{Error, bail, format_err};
use lazy_static::lazy_static;
use regex::Regex;
use std::ops;
use std::fmt;
use std::str::FromStr;
use time;

pub const TIME_UNITS_PER_SEC: i64 = 90000;

/// A time specified as 90,000ths of a second since 1970-01-01 00:00:00 UTC.
#[derive(Clone, Copy, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct Time(pub i64);

impl Time {
    pub fn new(tm: time::Timespec) -> Self {
        Time(tm.sec * TIME_UNITS_PER_SEC + tm.nsec as i64 * TIME_UNITS_PER_SEC / 1_000_000_000)
    }

    pub const fn min_value() -> Self { Time(i64::min_value()) }
    pub const fn max_value() -> Self { Time(i64::max_value()) }

    /// Parses a time as either 90,000ths of a second since epoch or a RFC 3339-like string.
    ///
    /// The former is 90,000ths of a second since 1970-01-01T00:00:00 UTC, excluding leap seconds.
    ///
    /// The latter is a string such as `2006-01-02T15:04:05`, followed by an optional 90,000ths of
    /// a second such as `:00001`, followed by an optional time zone offset such as `Z` or
    /// `-07:00`. A missing fraction is assumed to be 0. A missing time zone offset implies the
    /// local time zone.
    pub fn parse(s: &str) -> Result<Self, Error> {
        lazy_static! {
            static ref RE: Regex = Regex::new(r#"(?x)
                ^
                ([0-9]{4})-([0-9]{2})-([0-9]{2})T([0-9]{2}):([0-9]{2}):([0-9]{2})
                (?::([0-9]{5}))?
                (Z|[+-]([0-9]{2}):([0-9]{2}))?
                $"#).unwrap();
        }

        // First try parsing as 90,000ths of a second since epoch.
        match i64::from_str(s) {
            Ok(i) => return Ok(Time(i)),
            Err(_) => {},
        }

        // If that failed, parse as a time string or bust.
        let c = RE.captures(s).ok_or_else(|| format_err!("unparseable time {:?}", s))?;
        let mut tm = time::Tm{
            tm_sec: i32::from_str(c.get(6).unwrap().as_str()).unwrap(),
            tm_min: i32::from_str(c.get(5).unwrap().as_str()).unwrap(),
            tm_hour: i32::from_str(c.get(4).unwrap().as_str()).unwrap(),
            tm_mday: i32::from_str(c.get(3).unwrap().as_str()).unwrap(),
            tm_mon: i32::from_str(c.get(2).unwrap().as_str()).unwrap(),
            tm_year: i32::from_str(c.get(1).unwrap().as_str()).unwrap(),
            tm_wday: 0,
            tm_yday: 0,
            tm_isdst: -1,
            tm_utcoff: 0,
            tm_nsec: 0,
        };
        if tm.tm_mon == 0 {
            bail!("time {:?} has month 0", s);
        }
        tm.tm_mon -= 1;
        if tm.tm_year < 1900 {
            bail!("time {:?} has year before 1900", s);
        }
        tm.tm_year -= 1900;

        // The time crate doesn't use tm_utcoff properly; it just calls timegm() if tm_utcoff == 0,
        // mktime() otherwise. If a zone is specified, use the timegm path and a manual offset.
        // If no zone is specified, use the tm_utcoff path. This is pretty lame, but follow the
        // chrono crate's lead and just use 0 or 1 to choose between these functions.
        let sec = if let Some(zone) = c.get(8) {
            tm.to_timespec().sec + if zone.as_str() == "Z" {
                0
            } else {
                let off = i64::from_str(c.get(9).unwrap().as_str()).unwrap() * 3600 +
                          i64::from_str(c.get(10).unwrap().as_str()).unwrap() * 60;
                if zone.as_str().as_bytes()[0] == b'-' { off } else { -off }
            }
        } else {
            tm.tm_utcoff = 1;
            tm.to_timespec().sec
        };
        let fraction = if let Some(f) = c.get(7) { i64::from_str(f.as_str()).unwrap() } else { 0 };
        Ok(Time(sec * TIME_UNITS_PER_SEC + fraction))
    }

    /// Convert to unix seconds by floor method (rounding down).
    pub fn unix_seconds(&self) -> i64 { self.0 / TIME_UNITS_PER_SEC }
}

impl std::str::FromStr for Time {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> { Self::parse(s) }
}

impl ops::Sub for Time {
    type Output = Duration;
    fn sub(self, rhs: Time) -> Duration { Duration(self.0 - rhs.0) }
}

impl ops::AddAssign<Duration> for Time {
    fn add_assign(&mut self, rhs: Duration) { self.0 += rhs.0 }
}

impl ops::Add<Duration> for Time {
    type Output = Time;
    fn add(self, rhs: Duration) -> Time { Time(self.0 + rhs.0) }
}

impl ops::Sub<Duration> for Time {
    type Output = Time;
    fn sub(self, rhs: Duration) -> Time { Time(self.0 - rhs.0) }
}

impl fmt::Debug for Time {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // Write both the raw and display forms.
        write!(f, "{} /* {} */", self.0, self)
    }
}

impl fmt::Display for Time {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let tm = time::at(time::Timespec{sec: self.0 / TIME_UNITS_PER_SEC, nsec: 0});
        let zone_minutes = tm.tm_utcoff.abs() / 60;
        write!(f, "{}:{:05}{}{:02}:{:02}", tm.strftime("%FT%T").or_else(|_| Err(fmt::Error))?,
               self.0 % TIME_UNITS_PER_SEC,
               if tm.tm_utcoff > 0 { '+' } else { '-' }, zone_minutes / 60, zone_minutes % 60)
    }
}

/// A duration specified in 1/90,000ths of a second.
/// Durations are typically non-negative, but a `db::CameraDayValue::duration` may be negative.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct Duration(pub i64);

impl Duration {
    pub fn to_tm_duration(&self) -> time::Duration {
        time::Duration::nanoseconds(self.0 * 100000 / 9)
    }
}

impl fmt::Display for Duration {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut seconds = self.0 / TIME_UNITS_PER_SEC;
        const MINUTE_IN_SECONDS: i64 = 60;
        const HOUR_IN_SECONDS: i64 = 60 * MINUTE_IN_SECONDS;
        const DAY_IN_SECONDS: i64 = 24 * HOUR_IN_SECONDS;
        let days = seconds / DAY_IN_SECONDS;
        seconds %= DAY_IN_SECONDS;
        let hours = seconds / HOUR_IN_SECONDS;
        seconds %= HOUR_IN_SECONDS;
        let minutes = seconds / MINUTE_IN_SECONDS;
        seconds %= MINUTE_IN_SECONDS;
        let mut have_written = if days > 0 {
            write!(f, "{} day{}", days, if days == 1 { "" } else { "s" })?;
            true
        } else {
            false
        };
        if hours > 0 {
            write!(f, "{}{} hour{}", if have_written { " " } else { "" },
                   hours, if hours == 1 { "" } else { "s" })?;
            have_written = true;
        }
        if minutes > 0 {
            write!(f, "{}{} minute{}", if have_written { " " } else { "" },
                   minutes, if minutes == 1 { "" } else { "s" })?;
            have_written = true;
        }
        if seconds > 0 || !have_written {
            write!(f, "{}{} second{}", if have_written { " " } else { "" },
                   seconds, if seconds == 1 { "" } else { "s" })?;
        }
        Ok(())
    }
}

impl ops::Add for Duration {
    type Output = Duration;
    fn add(self, rhs: Duration) -> Duration { Duration(self.0 + rhs.0) }
}

impl ops::AddAssign for Duration {
    fn add_assign(&mut self, rhs: Duration) { self.0 += rhs.0 }
}

impl ops::SubAssign for Duration {
    fn sub_assign(&mut self, rhs: Duration) { self.0 -= rhs.0 }
}
