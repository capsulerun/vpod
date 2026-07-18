// Feature-gated profiling counters

use std::sync::atomic::AtomicU64;

#[cfg(feature = "perf-counters")]
use std::sync::atomic::Ordering::Relaxed;

use crate::csr::PrivMode;

macro_rules! counters {
    ($($name:ident),* $(,)?) => {
        $(pub static $name: AtomicU64 = AtomicU64::new(0);)*

        #[cfg(feature = "perf-counters")]
        const ALL: &[(&str, &AtomicU64)] = &[$((stringify!($name), &$name)),*];
    };
}

counters!(
    INSNS_MMODE,
    INSNS_KERNEL,
    INSNS_USER,
    WFI_EXECUTED,
    BLOCK_HITS,
    BLOCK_DECODES,
    SINGLE_STEPS,
    FETCH_PAGE_HITS,
    FETCH_TRANSLATES,
    LOADS,
    LOAD_FAST_HITS,
    STORES,
    STORE_FAST_HITS,
    CROSS_PAGE_ACCESSES,
    TLB_HITS,
    TLB_WALKS,
    BARE_TRANSLATES,
    STORE_PAGE_EVICTIONS,
    SS_SYSTEM,
    SS_AMO,
    SS_FP,
    SS_MISC_MEM,
    SS_OTHER,
);

#[inline(always)]
pub fn note_single_step_op(raw: u32) {
    #[cfg(feature = "perf-counters")]
    {
        let counter = if raw & 0x3 != 0x3 {
            &SS_OTHER // compressed insn the C-decoder refused
        } else {
            match raw & 0x7f {
                0x73 => &SS_SYSTEM,   // csr / ecall / ebreak / sret / wfi / sfence
                0x2f => &SS_AMO,      // atomics / lr / sc
                0x0f => &SS_MISC_MEM, // fence / fence.i
                0x07 | 0x27 | 0x43 | 0x47 | 0x4b | 0x4f | 0x53 => &SS_FP,
                _ => &SS_OTHER,
            }
        };
        counter.fetch_add(1, Relaxed);
    }
    #[cfg(not(feature = "perf-counters"))]
    let _ = raw;
}

#[inline(always)]
pub fn note_retired(priv_mode: PrivMode, count: u64) {
    #[cfg(feature = "perf-counters")]
    {
        let counter = match priv_mode {
            PrivMode::M => &INSNS_MMODE,
            PrivMode::S => &INSNS_KERNEL,
            PrivMode::U => &INSNS_USER,
        };
        counter.fetch_add(count, Relaxed);
    }
    #[cfg(not(feature = "perf-counters"))]
    let _ = (priv_mode, count);
}

macro_rules! note_fns {
    ($($fn_name:ident => $counter:ident),* $(,)?) => {
        $(
            #[inline(always)]
            pub fn $fn_name() {
                #[cfg(feature = "perf-counters")]
                $counter.fetch_add(1, Relaxed);
            }
        )*
    };
}

note_fns!(
    note_wfi => WFI_EXECUTED,
    note_block_hit => BLOCK_HITS,
    note_block_decode => BLOCK_DECODES,
    note_single_step => SINGLE_STEPS,
    note_fetch_page_hit => FETCH_PAGE_HITS,
    note_fetch_translate => FETCH_TRANSLATES,
    note_load => LOADS,
    note_load_fast_hit => LOAD_FAST_HITS,
    note_store => STORES,
    note_store_fast_hit => STORE_FAST_HITS,
    note_cross_page => CROSS_PAGE_ACCESSES,
    note_tlb_hit => TLB_HITS,
    note_tlb_walk => TLB_WALKS,
    note_bare_translate => BARE_TRANSLATES,
    note_store_page_eviction => STORE_PAGE_EVICTIONS,
);

pub fn report() -> Option<String> {
    #[cfg(feature = "perf-counters")]
    {
        let get = |c: &AtomicU64| c.swap(0, Relaxed);
        let values: Vec<(&str, u64)> = ALL.iter().map(|(n, c)| (*n, get(c))).collect();
        let total_insns: u64 = values[..3].iter().map(|(_, v)| v).sum();
        if total_insns == 0 {
            return None;
        }

        let v = |name: &str| values.iter().find(|(n, _)| *n == name).unwrap().1;
        let pct = |part: u64, whole: u64| {
            if whole == 0 {
                0.0
            } else {
                100.0 * part as f64 / whole as f64
            }
        };

        let total_loads = v("LOADS") + v("LOAD_FAST_HITS");
        let total_stores = v("STORES") + v("STORE_FAST_HITS");
        let mem_accesses = total_loads + total_stores;
        let translations = v("TLB_HITS") + v("TLB_WALKS") + v("BARE_TRANSLATES");
        let block_entries = v("BLOCK_HITS") + v("BLOCK_DECODES");

        let mut out = String::from("── vpod perf counters ──────────────────────\n");
        out.push_str(&format!(
            "retired: {} total | M(sbi) {:.1}% | S(kernel) {:.1}% | U(user) {:.1}%\n",
            total_insns,
            pct(v("INSNS_MMODE"), total_insns),
            pct(v("INSNS_KERNEL"), total_insns),
            pct(v("INSNS_USER"), total_insns),
        ));
        out.push_str(&format!(
            "wfi executed: {} (kernel-idle marker)\n",
            v("WFI_EXECUTED")
        ));
        out.push_str(&format!(
            "blocks: {} hits | {} decodes ({:.2}% miss) | {} single-step fallbacks | {:.1} insns/block\n",
            v("BLOCK_HITS"),
            v("BLOCK_DECODES"),
            pct(v("BLOCK_DECODES"), block_entries),
            v("SINGLE_STEPS"),
            if block_entries == 0 { 0.0 } else { total_insns as f64 / block_entries as f64 },
        ));
        out.push_str(&format!(
            "single-step by op: {} system | {} amo | {} fp | {} fence | {} other\n",
            v("SS_SYSTEM"),
            v("SS_AMO"),
            v("SS_FP"),
            v("SS_MISC_MEM"),
            v("SS_OTHER"),
        ));
        #[cfg(feature = "aot")]
        {
            let calls = crate::aot::DISPATCH_CALLS.swap(0, Relaxed);
            let retired = crate::aot::DISPATCH_RETIRED.swap(0, Relaxed);
            out.push_str(&format!(
                "aot: {} dispatches | {} insns retired ({:.1}% of all retired) | {:.1} insns/dispatch\n",
                calls,
                retired,
                pct(retired, total_insns),
                if calls == 0 { 0.0 } else { retired as f64 / calls as f64 },
            ));
        }
        out.push_str(&format!(
            "fetch: {} page-cache hits | {} translates ({:.2}% miss)\n",
            v("FETCH_PAGE_HITS"),
            v("FETCH_TRANSLATES"),
            pct(
                v("FETCH_TRANSLATES"),
                v("FETCH_PAGE_HITS") + v("FETCH_TRANSLATES")
            ),
        ));
        out.push_str(&format!(
            "memory: {} loads ({:.1}% fast-path) | {} stores ({:.1}% fast-path) | {:.1}% of insns | {} cross-page\n",
            total_loads,
            pct(v("LOAD_FAST_HITS"), total_loads),
            total_stores,
            pct(v("STORE_FAST_HITS"), total_stores),
            pct(mem_accesses, total_insns),
            v("CROSS_PAGE_ACCESSES"),
        ));
        out.push_str(&format!(
            "softmmu: {} tlb hits | {} walks ({:.4}% miss) | {} bare | {} store-page evictions\n",
            v("TLB_HITS"),
            v("TLB_WALKS"),
            pct(v("TLB_WALKS"), translations),
            v("BARE_TRANSLATES"),
            v("STORE_PAGE_EVICTIONS"),
        ));
        Some(out)
    }

    #[cfg(not(feature = "perf-counters"))]
    None
}
