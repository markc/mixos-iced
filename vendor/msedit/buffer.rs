// Adapted Microsoft Edit modules; see buffer/README.md and LICENSE.

    pub mod document;
    pub mod gap_buffer;
    pub mod helpers;
    pub mod navigation;
    pub mod simd;
    pub mod unicode;
    pub mod stdext {
        pub mod helpers;
        pub mod sys_unix;
        pub mod unicode {
            mod utf8;
            pub use utf8::*;
        }
    }
