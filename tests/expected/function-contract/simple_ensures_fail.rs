// Copyright Kani Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
// kani-flags: --check-contract max/main

#[kani::ensures(*result == x)]
fn max(x: u32, y: u32) -> u32 {
    if x > y { x } else { y }
}

#[kani::proof]
fn main() {
    let _ = Box::new(9_usize);
    max(7, 9);
}
