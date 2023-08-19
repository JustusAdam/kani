// Copyright Kani Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
// kani-flags: -Zfunction-contracts

#[kani::ensures(kani::forall(|i : usize| i != x as usize))]
fn max(x: u32, y: u32) -> u32 {
    if x > y { x } else { y }
}

#[kani::proof_for_contract(max)]
fn main() {
    max(kani::any(), kani::any());
}
