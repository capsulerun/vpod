// For addition of logic blocks (Multiplication, Floating Points) to the decoder and execution.

macro_rules! amo_w {
    ($name:ident, $op:expr) => {
        pub fn $name(memory_value: u32, src: u64) -> (u64, u32) {
            let new_memory_value: u32 = $op(memory_value, src as u32);
            (memory_value as i32 as i64 as u64, new_memory_value)
        }
    };
}

amo_w!(amoswap_w, |_, src| src);
amo_w!(amoadd_w, |memory_value: u32, src: u32| memory_value.wrapping_add(src));
amo_w!(amoxor_w, |memory_value: u32, src: u32| memory_value ^ src);
amo_w!(amoand_w, |memory_value: u32, src: u32| memory_value & src);
amo_w!(amoor_w, |memory_value: u32, src: u32| memory_value | src);
amo_w!(
    amomin_w,
    |memory_value: u32, src: u32| (memory_value as i32).min(src as i32) as u32
);
amo_w!(
    amomax_w,
    |memory_value: u32, src: u32| (memory_value as i32).max(src as i32) as u32
);
amo_w!(amominu_w, |memory_value: u32, src: u32| memory_value.min(src));
amo_w!(amomaxu_w, |memory_value: u32, src: u32| memory_value.max(src));

macro_rules! amo_d {
    ($name:ident, $op:expr) => {
        pub fn $name(memory_value: u64, src: u64) -> (u64, u64) {
            let new_memory_value: u64 = $op(memory_value, src);
            (memory_value, new_memory_value)
        }
    };
}

amo_d!(amoswap_d, |_, src| src);
amo_d!(amoadd_d, |memory_value: u64, src: u64| memory_value.wrapping_add(src));
amo_d!(amoxor_d, |memory_value: u64, src: u64| memory_value ^ src);
amo_d!(amoand_d, |memory_value: u64, src: u64| memory_value & src);
amo_d!(amoor_d, |memory_value: u64, src: u64| memory_value | src);
amo_d!(
    amomin_d,
    |memory_value: u64, src: u64| (memory_value as i64).min(src as i64) as u64
);
amo_d!(
    amomax_d,
    |memory_value: u64, src: u64| (memory_value as i64).max(src as i64) as u64
);
amo_d!(amominu_d, |memory_value: u64, src: u64| memory_value.min(src));
amo_d!(amomaxu_d, |memory_value: u64, src: u64| memory_value.max(src));

macro_rules! word_signed_op {
    ($name:ident, div_zero_is_lhs: false, $overflow_ret:expr, $op:ident) => {
        #[inline(always)]
        pub fn $name(lhs: u64, rhs: u64) -> u64 {
            let lhs = lhs as i32;
            let rhs = rhs as i32;
            if rhs == 0 {
                return u64::MAX;
            }

            if lhs == i32::MIN && rhs == -1 {
                return $overflow_ret;
            }

            lhs.$op(rhs) as i64 as u64
        }
    };
    ($name:ident, div_zero_is_lhs: true, $overflow_ret:expr, $op:ident) => {
        #[inline(always)]
        pub fn $name(lhs: u64, rhs: u64) -> u64 {
            let lhs = lhs as i32;
            let rhs = rhs as i32;

            if rhs == 0 {
                return lhs as i64 as u64;
            }

            if lhs == i32::MIN && rhs == -1 {
                return $overflow_ret;
            }

            lhs.$op(rhs) as i64 as u64
        }
    };
}

word_signed_op!(divw, div_zero_is_lhs: false, i32::MIN as i64 as u64, wrapping_div);
word_signed_op!(remw, div_zero_is_lhs: true,  0, wrapping_rem);

macro_rules! word_unsigned_op {
    ($name:ident, div_zero_is_lhs: false, $op:tt) => {

        #[inline(always)]
        pub fn $name(lhs: u64, rhs: u64) -> u64 {
            let lhs = lhs as u32;
            let rhs = rhs as u32;

            if rhs == 0 { return u64::MAX; }
            (lhs $op rhs) as i32 as i64 as u64
        }
    };
    ($name:ident, div_zero_is_lhs: true, $op:tt) => {

        #[inline(always)]
        pub fn $name(lhs: u64, rhs: u64) -> u64 {
            let lhs = lhs as u32;
            let rhs = rhs as u32;

            if rhs == 0 { return lhs as i32 as i64 as u64; }
            (lhs $op rhs) as i32 as i64 as u64
        }
    };
}

word_unsigned_op!(divuw, div_zero_is_lhs: false, /);
word_unsigned_op!(remuw, div_zero_is_lhs: true,  %);

#[inline(always)]
pub fn mul(lhs: u64, rhs: u64) -> u64 {
    lhs.wrapping_mul(rhs)
}

#[inline(always)]
pub fn mulh(lhs: u64, rhs: u64) -> u64 {
    ((lhs as i64 as i128).wrapping_mul(rhs as i64 as i128) >> 64) as u64
}

#[inline(always)]
pub fn mulhu(lhs: u64, rhs: u64) -> u64 {
    ((lhs as u128).wrapping_mul(rhs as u128) >> 64) as u64
}

#[inline(always)]
pub fn mulhsu(lhs: u64, rhs: u64) -> u64 {
    ((lhs as i64 as i128).wrapping_mul(rhs as u128 as i128) >> 64) as u64
}

#[inline(always)]
pub fn div(lhs: u64, rhs: u64) -> u64 {
    if rhs == 0 {
        return u64::MAX;
    }

    let lhs = lhs as i64;
    let rhs = rhs as i64;

    if lhs == i64::MIN && rhs == -1 {
        return i64::MIN as u64;
    }

    lhs.wrapping_div(rhs) as u64
}

#[inline(always)]
pub fn divu(lhs: u64, rhs: u64) -> u64 {
    lhs.checked_div(rhs).unwrap_or(u64::MAX)
}

#[inline(always)]
pub fn rem(lhs: u64, rhs: u64) -> u64 {
    if rhs == 0 {
        return lhs;
    }

    let lhs = lhs as i64;
    let rhs = rhs as i64;

    if lhs == i64::MIN && rhs == -1 {
        return 0;
    }

    lhs.wrapping_rem(rhs) as u64
}

#[inline(always)]
pub fn remu(lhs: u64, rhs: u64) -> u64 {
    if rhs == 0 { lhs } else { lhs % rhs }
}

#[inline(always)]
pub fn mulw(lhs: u64, rhs: u64) -> u64 {
    (lhs as u32).wrapping_mul(rhs as u32) as i32 as i64 as u64
}
