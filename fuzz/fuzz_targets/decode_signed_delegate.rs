#![no_main]

use base64::{Engine as _, engine::general_purpose::STANDARD};
use libfuzzer_sys::fuzz_target;
use x402_chain_near::decode_signed_delegate;

fuzz_target!(|data: &[u8]| {
    if let Ok(encoded) = std::str::from_utf8(data) {
        let _ = decode_signed_delegate(encoded);
    }

    // Feed every byte string through valid standard base64 as well, so the
    // Borsh decoder receives arbitrary binary inputs instead of relying on the
    // fuzzer to discover base64 syntax first.
    let encoded = STANDARD.encode(data);
    let _ = decode_signed_delegate(&encoded);
});
