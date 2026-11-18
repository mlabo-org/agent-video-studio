//! Timeline time as integer microseconds.
//!
//! Every position and duration in the project is a [`Time`]. Arithmetic is exact, so
//! there is no epsilon comparison and no rounding helper anywhere else in the code.
//! JSON carries seconds as a float; conversion happens only here.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::ops::{Add, AddAssign, Neg, Sub, SubAssign};

const MICROS_PER_SECOND: f64 = 1_000_000.0;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Time(i64);

impl Time {
    pub const ZERO: Time = Time(0);

    pub const fn from_micros(micros: i64) -> Self {
        Time(micros)
    }

    pub const fn micros(self) -> i64 {
        self.0
    }

    /// Converts seconds to a time. Returns `None` for NaN, infinities, and values
    /// too large to represent.
    pub fn from_secs(seconds: f64) -> Option<Self> {
        if !seconds.is_finite() {
            return None;
        }
        let micros = (seconds * MICROS_PER_SECOND).round();
        if micros.abs() > 9.0e15 {
            return None;
        }
        Some(Time(micros as i64))
    }

    pub fn secs(self) -> f64 {
        self.0 as f64 / MICROS_PER_SECOND
    }

    /// Multiplies by a real factor (for example a playback speed), rounding to the
    /// nearest microsecond.
    pub fn scale(self, factor: f64) -> Self {
        Time((self.0 as f64 * factor).round() as i64)
    }

    pub fn is_positive(self) -> bool {
        self.0 > 0
    }

    pub fn is_negative(self) -> bool {
        self.0 < 0
    }
}

impl Add for Time {
    type Output = Time;
    fn add(self, rhs: Time) -> Time {
        Time(self.0 + rhs.0)
    }
}

impl Sub for Time {
    type Output = Time;
    fn sub(self, rhs: Time) -> Time {
        Time(self.0 - rhs.0)
    }
}

impl Neg for Time {
    type Output = Time;
    fn neg(self) -> Time {
        Time(-self.0)
    }
}

impl AddAssign for Time {
    fn add_assign(&mut self, rhs: Time) {
        self.0 += rhs.0;
    }
}

impl SubAssign for Time {
    fn sub_assign(&mut self, rhs: Time) {
        self.0 -= rhs.0;
    }
}

impl fmt::Debug for Time {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}s", self.secs())
    }
}

impl fmt::Display for Time {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.secs())
    }
}

impl Serialize for Time {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f64(self.secs())
    }
}

impl<'de> Deserialize<'de> for Time {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let seconds = f64::deserialize(deserializer)?;
        Time::from_secs(seconds).ok_or_else(|| serde::de::Error::custom("time must be a finite number of seconds"))
    }
}

/// Shorthand used by tests and callers that hold literal seconds.
pub fn secs(seconds: f64) -> Time {
    Time::from_secs(seconds).expect("finite seconds")
}
