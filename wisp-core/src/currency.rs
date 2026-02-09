use anyhow::{anyhow, Result};
use bincode::{Decode, Encode};
use serde::{Deserialize, Serialize};
use std::iter::Sum;
use std::ops::AddAssign;
use std::ops::SubAssign;

use std::fmt;
use std::ops::{Add, Sub};

/// Represents a currency amount in the smallest indivisible unit.
#[derive(
    Encode,
    Decode,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    Default,
)]
pub struct Amount(pub u64);

use crate::sha256::Hashable;
use sha2::{Digest, Sha256};

/// Implements the `Hashable` trait for `Amount` to allow it to be included in hashed data structures.
impl Hashable for Amount {
    fn update_hasher(&self, hasher: &mut Sha256) {
        hasher.update(&self.0.to_be_bytes());
    }
}

impl Amount {
    /// The maximum possible amount.
    pub const MAX: Amount = Amount(u64::MAX);
    /// The number of decimal places for the main currency unit (WISP).
    pub const DECIMAL_PLACES: u32 = 8;
    /// The factor to convert between the main unit (WISP) and the smallest unit.
    pub const CONVERSION_FACTOR: u64 = 10u64.pow(Self::DECIMAL_PLACES);
    /// The maximum representable amount in whole WISP units.
    pub const MAX_WISP: u64 = Self::MAX.0 / Self::CONVERSION_FACTOR;

    /// Creates an `Amount` from the smallest currency unit.
    pub fn from_smallest_unit(units: u64) -> Self {
        Amount(units)
    }

    /// Creates an `Amount` from a whole number of WISP, converting it to the smallest unit.
    pub fn from_wisp(wisp: u64) -> Result<Self> {
        wisp.checked_mul(Self::CONVERSION_FACTOR)
            .map(Amount)
            .ok_or_else(|| anyhow!("Overflow when converting WISP to smallest units: {}", wisp))
    }

    /// Returns the value of the `Amount` in the smallest currency unit.
    pub fn as_smallest_unit(&self) -> u64 {
        self.0
    }

    /// Creates an `Amount` of zero.
    pub const fn zero() -> Self {
        Amount(0)
    }

    /// Performs a checked addition, returning `None` on overflow.
    pub fn checked_add(self, other: Self) -> Option<Self> {
        self.0.checked_add(other.0).map(Amount)
    }

    /// Performs a checked subtraction, returning `None` on underflow.
    pub fn checked_sub(self, other: Self) -> Option<Self> {
        self.0.checked_sub(other.0).map(Amount)
    }
    /// Performs a checked multiplication, returning `None` on overflow.
    pub fn checked_mul(self, other: u64) -> Option<Self> {
        self.0.checked_mul(other).map(Amount)
    }

    /// Converts the `Amount` to a string representation in WISP, handling decimal places correctly.
    pub fn to_string_wisp(&self) -> String {
        let integer_part = self.0 / Self::CONVERSION_FACTOR;
        let fractional_part = self.0 % Self::CONVERSION_FACTOR;

        if fractional_part == 0 {
            format!("{}.0", integer_part)
        } else {
            let fractional_str = format!(
                "{:0width$}",
                fractional_part,
                width = Self::DECIMAL_PLACES as usize
            );

            let trimmed_fractional = fractional_str.trim_end_matches('0');
            if trimmed_fractional.is_empty() {
                format!("{}.0", integer_part)
            } else {
                format!("{}.{}", integer_part, trimmed_fractional)
            }
        }
    }

    /// Parses a string representing an amount in WISP (e.g., "1.23") into an `Amount`.
    pub fn from_string_wisp(s: &str) -> Result<Self> {
        let parts: Vec<&str> = s.split('.').collect();
        if parts.len() > 2 {
            return Err(anyhow!(
                "Invalid decimal format: multiple decimal points in '{}'",
                s
            ));
        }

        let integer_str = parts[0];
        let fractional_str = parts.get(1).unwrap_or(&"");

        if fractional_str.len() > Self::DECIMAL_PLACES as usize {
            return Err(anyhow!(
                "Too many decimal places in '{}'. Max {} allowed.",
                s,
                Self::DECIMAL_PLACES
            ));
        }
        let parsed_integer = if integer_str.is_empty() {
            0
        } else {
            integer_str
                .parse::<u64>()
                .map_err(|e| anyhow!("Invalid integer part in '{}': {}", s, e))?
        };

        let mut total_units = parsed_integer
            .checked_mul(Self::CONVERSION_FACTOR)
            .ok_or_else(|| anyhow!("Integer part overflow when converting '{}'", s))?;

        if !fractional_str.is_empty() {
            let scale_factor =
                10u64.pow((Self::DECIMAL_PLACES as usize - fractional_str.len()) as u32);
            let parsed_fractional = fractional_str
                .parse::<u64>()
                .map_err(|e| anyhow!("Invalid fractional part in '{}': {}", s, e))?;

            let scaled_fractional =
                parsed_fractional.checked_mul(scale_factor).ok_or_else(|| {
                    anyhow!("Fractional part scaling overflow when converting '{}'", s)
                })?;

            total_units = total_units
                .checked_add(scaled_fractional)
                .ok_or_else(|| anyhow!("Total units overflow when converting '{}'", s))?;
        }

        Ok(Amount(total_units))
    }
}

/// Implements addition for `Amount`. Panics on overflow in debug builds.
impl Add for Amount {
    type Output = Self;
    fn add(self, other: Self) -> Self {
        self.checked_add(other).expect("Amount addition overflowed")
    }
}

/// Implements subtraction for `Amount`. Panics on underflow in debug builds.
impl Sub for Amount {
    type Output = Self;
    fn sub(self, other: Self) -> Self {
        self.checked_sub(other)
            .expect("Amount subtraction underflowed")
    }
}

/// Implements `AddAssign` for `Amount` for in-place addition.
impl AddAssign for Amount {
    fn add_assign(&mut self, other: Self) {
        *self = *self + other;
    }
}

/// Implements `SubAssign` for `Amount` for in-place subtraction.
impl SubAssign for Amount {
    fn sub_assign(&mut self, other: Self) {
        *self = *self - other;
    }
}

/// Implements `Sum` for `Amount`, allowing an iterator of `Amount`s to be summed up.
/// On overflow, the sum will saturate at `Amount::MAX`.
impl Sum for Amount {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Amount::zero(), |acc, x| {
            acc.checked_add(x).unwrap_or_else(|| Amount(u64::MAX)) // Saturate on overflow
        })
    }
}

/// Implements `Display` for `Amount`, formatting it as a WISP string.
impl fmt::Display for Amount {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.to_string_wisp())
    }
}
