// Copyright Kani Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
// kani-flags: --check-contract div/main

#[kani::requires(divisor != 0)]
fn div(dividend: u32, divisor: u32) -> u32 {
    dividend / divisor
}

#[kani::proof]
fn main() {
    let _ = Box::new(());
    div(kani::any(), kani::any());
}
