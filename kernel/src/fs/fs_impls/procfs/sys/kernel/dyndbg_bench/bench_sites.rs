// SPDX-License-Identifier: MPL-2.0

// This file contains generated bench callsites and a batch runner.
// It is kept as a separate module to keep the main benchmark file concise.

// Macro to generate bench callsite functions from a (name => literal) list.
macro_rules! gen_bench_logs {
    ($($name:ident => $msg:literal),* $(,)?) => {
        $(
            #[inline(never)]
            fn $name() {
                aster_logger::dyndbg_debug_site!($msg, $msg);
                // Prevent LTO from eliminating this function when call sites are disabled.
                core::hint::black_box(());
            }
        )*

        // Collect function pointers into a static slice for easy iteration.
        #[allow(non_upper_case_globals)]
        static BENCH_FNS: &[fn()] = &[
            $( $name ),*
        ];

        #[inline(never)]
        pub fn bench_log_batch() {
            for f in BENCH_FNS {
                core::hint::black_box(f());
            }
        }
    }
}

// Generate 64 bench callsites. Listing is explicit but compact via the macro.
gen_bench_logs! {
    bench_log_0 => "bench_0",bench_log_1 => "bench_1",bench_log_2 => "bench_2",bench_log_3 => "bench_3",
    bench_log_4 => "bench_4",bench_log_5 => "bench_5",bench_log_6 => "bench_6",bench_log_7 => "bench_7",
    bench_log_8 => "bench_8",bench_log_9 => "bench_9",bench_log_10 => "bench_10",bench_log_11 => "bench_11",
    bench_log_12 => "bench_12",bench_log_13 => "bench_13",bench_log_14 => "bench_14",bench_log_15 => "bench_15",
    bench_log_16 => "bench_16",bench_log_17 => "bench_17",bench_log_18 => "bench_18",bench_log_19 => "bench_19",
    bench_log_20 => "bench_20",bench_log_21 => "bench_21",bench_log_22 => "bench_22",bench_log_23 => "bench_23",
    bench_log_24 => "bench_24",bench_log_25 => "bench_25",bench_log_26 => "bench_26",bench_log_27 => "bench_27",
    bench_log_28 => "bench_28",bench_log_29 => "bench_29",bench_log_30 => "bench_30",bench_log_31 => "bench_31",
    bench_log_32 => "bench_32",bench_log_33 => "bench_33",bench_log_34 => "bench_34",bench_log_35 => "bench_35",
    bench_log_36 => "bench_36",bench_log_37 => "bench_37",bench_log_38 => "bench_38",bench_log_39 => "bench_39",
    bench_log_40 => "bench_40",bench_log_41 => "bench_41",bench_log_42 => "bench_42",bench_log_43 => "bench_43",
    bench_log_44 => "bench_44",bench_log_45 => "bench_45",bench_log_46 => "bench_46",bench_log_47 => "bench_47",
    bench_log_48 => "bench_48",bench_log_49 => "bench_49",bench_log_50 => "bench_50",bench_log_51 => "bench_51",
    bench_log_52 => "bench_52",bench_log_53 => "bench_53",bench_log_54 => "bench_54",bench_log_55 => "bench_55",
    bench_log_56 => "bench_56",bench_log_57 => "bench_57",bench_log_58 => "bench_58",bench_log_59 => "bench_59",
    bench_log_60 => "bench_60",bench_log_61 => "bench_61",bench_log_62 => "bench_62",bench_log_63 => "bench_63",
}
