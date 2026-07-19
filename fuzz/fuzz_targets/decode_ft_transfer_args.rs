#![no_main]

use libfuzzer_sys::fuzz_target;
use x402_chain_near::decode_ft_transfer_args;

fuzz_target!(|data: &[u8]| {
    let _ = decode_ft_transfer_args(data);
});
