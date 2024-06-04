#![allow(dead_code)]

use core::ops::{Bound, Range, RangeBounds};

pub mod singleton;

pub trait BitAccess {
    const BIT_LENGTH: usize;
    fn get_bit(&self, idx: usize) -> bool;
    fn get_bits<T: RangeBounds<usize>>(&self, range: T) -> Self;
    fn set_bit(&mut self, idx: usize, val: bool) -> &mut Self;
    fn set_bits<T: RangeBounds<usize>>(&mut self, range: T, val: Self) -> &mut Self;
}

macro_rules! bit_access_impl {
    ($($t:ty)*) => ($(
        impl BitAccess for $t {
            const BIT_LENGTH: usize = ::core::mem::size_of::<Self>() as usize * 8;

            #[inline]
            fn get_bit(&self, bit: usize) -> bool {
                assert!(bit < Self::BIT_LENGTH);

                (*self & (1 << bit)) != 0
            }

            #[inline]
            fn get_bits<T: RangeBounds<usize>>(&self, range: T) -> Self {
                let range = to_regular_range(&range, Self::BIT_LENGTH);

                assert!(range.start < Self::BIT_LENGTH);
                assert!(range.end <= Self::BIT_LENGTH);
                assert!(range.start < range.end);

                // shift away high bits
                let bits = *self << (Self::BIT_LENGTH - range.end) >> (Self::BIT_LENGTH - range.end);

                // shift away low bits
                bits >> range.start
            }

            #[inline]
            fn set_bit(&mut self, bit: usize, value: bool) -> &mut Self {
                assert!(bit < Self::BIT_LENGTH);

                if value {
                    *self |= 1 << bit;
                } else {
                    *self &= !(1 << bit);
                }

                self
            }

            #[inline]
            fn set_bits<T: RangeBounds<usize>>(&mut self, range: T, value: Self) -> &mut Self {
                let range = to_regular_range(&range, Self::BIT_LENGTH);

                assert!(range.start < Self::BIT_LENGTH);
                assert!(range.end <= Self::BIT_LENGTH);
                assert!(range.start < range.end);
                assert!(value << (Self::BIT_LENGTH - (range.end - range.start)) >>
                        (Self::BIT_LENGTH - (range.end - range.start)) == value,
                        "value does not fit into bit range");

                let bitmask: Self = !(!0 << (Self::BIT_LENGTH - range.end) >>
                                    (Self::BIT_LENGTH - range.end) >>
                                    range.start << range.start);

                *self = (*self & bitmask) | (value << range.start);

                self
            }
        }
    )*)
}

bit_access_impl!(u8 u16 u32);

fn to_regular_range<T: RangeBounds<usize>>(generic_rage: &T, bit_length: usize) -> Range<usize> {
    let start = match generic_rage.start_bound() {
        Bound::Excluded(&value) => value + 1,
        Bound::Included(&value) => value,
        Bound::Unbounded => 0,
    };
    let end = match generic_rage.end_bound() {
        Bound::Excluded(&value) => value,
        Bound::Included(&value) => value + 1,
        Bound::Unbounded => bit_length,
    };

    start..end
}
