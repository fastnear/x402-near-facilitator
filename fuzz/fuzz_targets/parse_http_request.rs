#![no_main]

use libfuzzer_sys::fuzz_target;
use x402_near_facilitator::{config::PaymentIdentifierConfig, protocol::parse_request};

fuzz_target!(|data: &[u8]| {
    let _ = parse_request(data, &PaymentIdentifierConfig::default());
});
