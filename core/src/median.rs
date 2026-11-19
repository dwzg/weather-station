//! Median of a few samples.

use std::ops::{Add, Div};

/// Median of `v` (the input is left untouched). For an even count, the mean of
/// the middle two. None for an empty slice.
pub fn median<T>(v: &[T]) -> Option<T>
where
    T: Copy + PartialOrd + Add<Output = T> + Div<Output = T> + From<u8>,
{
    if v.is_empty() {
        return None;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = s.len();
    Some(if n % 2 == 1 { s[n / 2] } else { (s[n / 2 - 1] + s[n / 2]) / T::from(2) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn odd_even_and_untouched() {
        assert_eq!(median(&[5, 1, 9]), Some(5));
        let b = [4, 1, 3, 2];
        assert_eq!(median(&b), Some(2));
        assert_eq!(b[0], 4);
        assert_eq!(median(&[1.0f32, 2.0]), Some(1.5));
        assert_eq!(median::<i32>(&[]), None);
    }
}
