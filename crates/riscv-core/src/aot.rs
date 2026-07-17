// AOT-translated guest blocks

#[allow(unused_variables, unreachable_code, clippy::all)]
pub mod generated {
    include!("aot/generated.rs");
}

pub use generated::AOT_PAGE_HASHES;
pub use generated::dispatch;

use std::sync::atomic::{AtomicU64, Ordering};

pub static DISPATCH_CALLS: AtomicU64 = AtomicU64::new(0);
pub static DISPATCH_RETIRED: AtomicU64 = AtomicU64::new(0);

#[inline(always)]
pub fn note_dispatch(retired: u64) {
    DISPATCH_CALLS.fetch_add(1, Ordering::Relaxed);
    DISPATCH_RETIRED.fetch_add(retired, Ordering::Relaxed);
}
