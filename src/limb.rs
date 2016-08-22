// Copyright 2016 David Judd.
// Copyright 2016 Brian Smith.
//
// Permission to use, copy, modify, and/or distribute this software for any
// purpose with or without fee is hereby granted, provided that the above
// copyright notice and this permission notice appear in all copies.
//
// THE SOFTWARE IS PROVIDED "AS IS" AND AND THE AUTHORS DISCLAIM ALL WARRANTIES
// WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
// MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHORS BE LIABLE FOR ANY
// SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
// WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN ACTION
// OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR IN
// CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
use {rand, polyfill, c, core, error};

// XXX: Not correct for x32 ABIs.
#[cfg(target_pointer_width = "64")] pub type Limb = u64;
#[cfg(target_pointer_width = "32")] pub type Limb = u32;
#[cfg(target_pointer_width = "64")] pub const LIMB_BITS: usize = 64;
#[cfg(target_pointer_width = "32")] pub const LIMB_BITS: usize = 32;

pub const LIMB_BYTES: usize = (LIMB_BITS + 7) / 8;

/// References a positive integer range `[1..max_exclusive)` where
/// `max_exclusive` is a slice of limbs ordered least-significant-limb to
/// most-significant-limb (and the limbs use the native endianness).
pub struct Range<'a> {
    max_exclusive: &'a [Limb],
}

impl <'a>Range<'a> {
    pub fn from_max_exclusive(max_exclusive: &[Limb]) -> Range {
        debug_assert!(!limbs_are_zero_constant_time(max_exclusive));
        debug_assert!(max_exclusive[max_exclusive.len() - 1] > 0);

        Range {
            max_exclusive: max_exclusive
        }
    }

    fn leading_zero_bits(&self) -> usize {
        let most_significant_limb =
            self.max_exclusive[self.max_exclusive.len() - 1];
        most_significant_limb.leading_zeros() as usize
    }

    /// Are these little-endian limbs within the range?
    ///
    /// Checks in constant time.
    pub fn are_limbs_within(&self, limbs: &[Limb]) -> bool {
        assert_eq!(self.max_exclusive.len(), limbs.len());

        let is_gt_zero = !limbs_are_zero_constant_time(limbs);
        let is_lt_max = limbs_less_than_limbs_constant_time(limbs, self.max_exclusive);

        is_lt_max && is_gt_zero
    }

    // Loosely based on NSA Guide Appendix B.2:
    // "Key Pair Generation by Testing Candidates",
    // with modifications adapted from OpenSSL.
    //
    // TODO: DRY-up with `ec::suite_b::private_key::generate_private_key`?
    //
    /// Chooses a positive integer within the range and stores it into `dest`
    /// encoded as limbs ordered least-significant-limb to most-significant-limb.
    ///
    /// This function is intended to be suitable for generating private keys.
    pub fn sample_into_limbs(&self, dest: &mut [Limb], rng: &rand::SecureRandom)
                             -> Result<(), error::Unspecified> {

        assert_eq!(self.max_exclusive.len(), dest.len());

        // TODO(DJ) Move into constructor
        // TODO(DJ) Clean up and comment logic which skips this special case
        // if there are no leading zeros
        let starts_with_0b100 = starts_with_0b100(self.max_exclusive) && self.leading_zero_bits() > 0;
        let bits_to_mask = if starts_with_0b100 { self.leading_zero_bits() - 1 } else { self.leading_zero_bits() };

        // XXX: The value 100 was chosen to match OpenSSL due to uncertainty of
        // what specific value would be better, but it seems bad to try 100 times.
        for _ in 0..100 {
            {
                let mut dest_as_bytes = limbs_as_bytes_mut(dest);
                try!(rng.fill(&mut dest_as_bytes));
            }

            if bits_to_mask > 0 {
                let mask: Limb = (1 << (LIMB_BITS - bits_to_mask)) - 1;
                dest[self.max_exclusive.len() - 1] &= mask;
            }

            if self.are_limbs_within(&dest) {
                return Ok(());
            }

            if starts_with_0b100 {
                // `x` is of the form `0b100...`. This means:
                //
                //    x < 2**n - 2**(n-2) - 2**(n-3).
                //
                // This means that `3*x < 2**(n+1)`. Proof:
                //
                //  3*x < 3*(2**n - 2**(n-2) - 2**(n-3))
                //      < (2 + 1)*(2**n - 2**(n-2) - 2**(n-3))
                //      < 2*(2**n - 2**(n-2) - 2**(n-3)) + 2**n - 2**(n-2) - 2**(n-3)
                //      < 2**(n+1) - 2**(n-1) - 2**(n-2) + 2**n - 2**(n-2) - 2**(n-3)
                //      < 2**(n+1) + 2**n - 2**(n-1) - 2**(n-2) - 2**(n-2) - 2**(n-3)
                //      < 2**(n+1) + 2**n - 2**(n-1) - 2*(2**(n-2)) - 2**(n-3)
                //      < 2**(n+1) + 2**n - 2**(n-1) - 2**(n-1) - 2**(n-3)
                //      < 2**(n+1) + 2**n - 2*(2**(n-1)) - 2**(n-3)
                //      < 2**(n+1) + 2**n - 2**n - 2**(n-3)
                //      < 2**(n+1) - 2**(n-3)
                //
                // Then clearly 2**(n+1) - 2**(n-3) < 2**(n+1) since n is positive.
                //
                // This means we can generate a value in the range [0, 2**(n+1)),
                // which would fall into one of four sub-intervals:
                //
                //    [0, max)          => Return the value as-is.
                //    [max, 2*max)      => Return `value - max`.
                //    [2*max, 3*max)    => Return `value - max - max`.
                //    [3*max, 2**(n+1)) => Generate a new random value and try again.
                //
                // This avoids biasing the result towards small values, which is
                // what reducing the random value (mod max) would do, while
                // reducing the probability that a new random value will be needed.
                //
                // Further, at this point, we know `dest` isn't in the range [0, max),
                // so either it's zero or it's >= `max_exclusive`. Because
                // `limbs_reduce_once_constant_time` compares before subtracting,
                // what follows is safe in either case, although it's pointless
                // in the (highly improbable) zero case.

                limbs_reduce_once_constant_time(dest, self.max_exclusive);
                if self.are_limbs_within(&dest) {
                    // [max, 2*max)
                    return Ok(());
                }

                limbs_reduce_once_constant_time(dest, self.max_exclusive);
                if self.are_limbs_within(&dest) {
                    // [2*max, 3*max)
                    return Ok(());
                }

                // [3*max, 2**(n+1)) or zero
            }
        }

        Err(error::Unspecified)
    }

}

#[allow(unsafe_code)]
#[allow(non_snake_case)]
#[doc(hidden)]
#[no_mangle]
pub unsafe extern fn GFp_rand_mod(dest: *mut Limb,
                                  max_exclusive: *const Limb,
                                  num_limbs: c::size_t,
                                  rng: *mut rand::RAND)
                                  -> c::int {
    const ERR: c::int = 0;
    const SUCCESS: c::int = 1;

    let range = Range::from_max_exclusive(
        core::slice::from_raw_parts(max_exclusive, num_limbs));
    let mut dest = core::slice::from_raw_parts_mut(dest, num_limbs);

    let result = range.sample_into_limbs(&mut dest, (*rng).rng);
    if result.is_err() {
        return ERR;
    }

    SUCCESS
}


#[cfg(target_pointer_width = "64")]
fn limbs_as_bytes_mut<'a>(src: &'a mut [Limb]) -> &'a mut [u8] {
    polyfill::slice::u64_as_u8_mut(src)
}

#[cfg(target_pointer_width = "32")]
fn limbs_as_bytes_mut<'a>(src: &'a mut [Limb]) -> &'a mut [u8] {
    polyfill::slice::u32_as_u8_mut(src)
}

fn starts_with_0b100(limbs: &[Limb]) -> bool {
    debug_assert!(limbs.len() > 0);

    let most_sig_limb = limbs[limbs.len() - 1];
    let next_limb = if limbs.len() > 1 {
        Some(limbs[limbs.len() - 2])
    } else {
        None
    };

    debug_assert!(most_sig_limb > 0);

    match most_sig_limb {
        4 => true,
        3 => false,
        2 => match next_limb {
            None => false,
            Some(l) => { l < (1 << (LIMB_BITS - 1)) },
        },
        1 => match next_limb {
            None => false,
            Some(l) => { l < (1 << (LIMB_BITS - 2)) },
        },
        _ => {
            let used_bits = LIMB_BITS - (most_sig_limb.leading_zeros() as usize);
            let most_sig_bit = 1 << (used_bits - 1);
            most_sig_limb - most_sig_bit < most_sig_bit >> 2
        },
    }
}

#[allow(unsafe_code)]
pub fn limbs_less_than_limbs_constant_time(a: &[Limb], b: &[Limb]) -> bool {
    assert_eq!(a.len(), b.len());
    let result = unsafe {
        GFp_constant_time_limbs_lt_limbs(a.as_ptr(), b.as_ptr(), b.len())
    };
    result != 0
}

#[allow(unsafe_code)]
pub fn limbs_are_zero_constant_time(limbs: &[Limb]) -> bool {
    let result = unsafe {
        GFp_constant_time_limbs_are_zero(limbs.as_ptr(), limbs.len())
    };
    result != 0
}

/// Equivalent to `if (r >= m) { r -= m; }`
#[allow(unsafe_code)]
pub fn limbs_reduce_once_constant_time(r: &mut [Limb], m: &[Limb]) {
    assert_eq!(r.len(), m.len());
    unsafe {
        GFp_constant_time_limbs_reduce_once(r.as_mut_ptr(), m.as_ptr(), m.len());
    }
}

extern {
    fn GFp_constant_time_limbs_are_zero(a: *const Limb, num_limbs: c::size_t)
                                        -> Limb;

    fn GFp_constant_time_limbs_lt_limbs(a: *const Limb, b: *const Limb,
                                        num_limbs: c::size_t) -> Limb;

    fn GFp_constant_time_limbs_reduce_once(r: *mut Limb, m: *const Limb,
                                           num_limbs: c::size_t);
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::starts_with_0b100;
    use rand;

    #[cfg(target_pointer_width = "64")] const MAX_LIMB: Limb = 0xffffffffffffffff;
    #[cfg(target_pointer_width = "32")] const MAX_LIMB: Limb = 0xffffffff;

    #[test]
    fn test_limbs_in_range() {
        let limbs = &[MAX_LIMB, MAX_LIMB];
        let range = Range::from_max_exclusive(limbs);
        assert!(!range.are_limbs_within(&[MAX_LIMB, MAX_LIMB]));
        assert!(range.are_limbs_within(&[MAX_LIMB, MAX_LIMB-1]));
        assert!(range.are_limbs_within(&[MAX_LIMB-1, MAX_LIMB]));
        assert!(!range.are_limbs_within(&[0, 0]));
        assert!(range.are_limbs_within(&[1, 0]));
        assert!(range.are_limbs_within(&[0, 1]));

        let limbs = &[0xdeadbeef, 0xdeadbeef];
        let range = Range::from_max_exclusive(limbs);
        assert!(!range.are_limbs_within(&[0xdeadbeef, 0xdeadbeef]));
        assert!(range.are_limbs_within(&[0xdeadbeee, 0xdeadbeef]));
        assert!(range.are_limbs_within(&[0xdeadbeef, 0xdeadbeee]));
        assert!(!range.are_limbs_within(&[0xdeadbeff, 0xdeadbeef]));
        assert!(!range.are_limbs_within(&[0xdeadbeef, 0xdeadbeff]));

        let limbs = &[2];
        let range = Range::from_max_exclusive(limbs);
        assert!(!range.are_limbs_within(&[0]));
        assert!(range.are_limbs_within(&[1]));
        assert!(!range.are_limbs_within(&[2]));
    }

    #[test]
    fn test_random_generation() {
        let rng = rand::SystemRandom::new();

        let mut dest: [Limb; 2] = [0; 2];
        let limbs = &[MAX_LIMB, MAX_LIMB];
        let range = Range::from_max_exclusive(limbs);
        assert!(range.sample_into_limbs(&mut dest, &rng).is_ok());
        assert!(dest.iter().any( |b| *b > 0 ));

        let mut dest: [Limb; 2] = [0; 2];
        let limbs = &[0xdeadbeef, 0xdeadbeef];
        let range = Range::from_max_exclusive(limbs);
        assert!(range.sample_into_limbs(&mut dest, &rng).is_ok());
        assert!(dest.iter().any( |b| *b > 0 ));

        let mut dest: [Limb; 1] = [0; 1];
        let limbs = &[2];
        let range = Range::from_max_exclusive(limbs);
        assert!(range.sample_into_limbs(&mut dest, &rng).is_ok());
        assert_eq!([1], dest);

        let mut dest: [Limb; 1] = [0; 1];
        let limbs = &[1 << (LIMB_BITS - 1)];
        let range = Range::from_max_exclusive(limbs);
        assert!(range.sample_into_limbs(&mut dest, &rng).is_ok());
        assert!(dest.iter().any( |b| *b > 0 ));
    }

    #[test]
    fn test_starts_with_0b100() {
        assert!(!starts_with_0b100(&[1]));
        assert!(!starts_with_0b100(&[2]));
        assert!(!starts_with_0b100(&[3]));
        assert!(starts_with_0b100(&[4]));
        assert!(!starts_with_0b100(&[5]));
        assert!(!starts_with_0b100(&[6]));
        assert!(!starts_with_0b100(&[7]));
        assert!(starts_with_0b100(&[8]));
        assert!(starts_with_0b100(&[9]));
        assert!(!starts_with_0b100(&[10]));

        assert!(starts_with_0b100(&[0, 1]));
        assert!(starts_with_0b100(&[Limb::max_value() >> 2, 1]));
        assert!(!starts_with_0b100(&[Limb::max_value() >> 1, 1]));
        assert!(starts_with_0b100(&[Limb::max_value() >> 1, 2]));
        assert!(!starts_with_0b100(&[Limb::max_value(), 2]));
        assert!(!starts_with_0b100(&[Limb::max_value() >> 1, 3]));
        assert!(starts_with_0b100(&[Limb::max_value(), 4]));
        assert!(!starts_with_0b100(&[0, 5]));
        assert!(!starts_with_0b100(&[0, 6]));
        assert!(!starts_with_0b100(&[0, 7]));
        assert!(starts_with_0b100(&[0, 8]));
        assert!(starts_with_0b100(&[0, 9]));
        assert!(!starts_with_0b100(&[0, 10]));
    }

}

#[cfg(feature = "internal_benches")]
mod bench {
    use super::*;
    use bench;
    use rand;

    #[bench]
    fn bench_sample_into_limbs(b: &mut bench::Bencher) {
        let limbs = &[0, 0, 0, 0, 0, 0, 0, 16];
        let mut dest: [Limb; 8] = [0; 8];
        let rng = rand::SystemRandom::new();

        b.iter(|| {
            let range = Range::from_max_exclusive(limbs);
            range.sample_into_limbs(&mut dest, &rng)
        });
    }
}
