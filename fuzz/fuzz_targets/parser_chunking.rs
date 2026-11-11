#![no_main]
use libfuzzer_sys::fuzz_target;
fuzz_target!(|data: &[u8]| ghostty_vt::fuzz_support::check_chunking(data));
