// Copyright Kani Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
// kani-flags: -Zfunction-contracts

#[kani::ensures(old(*ptr) == *ptr)]
#[kani::requires(*ptr < 100)]
#[kani::assigns(*ptr)]
fn modify(ptr: &mut u32) {
    *ptr += 1;
}

#[kani::proof_for_contract(modify)]
fn main() {
    let _ = Box::new(());
    let mut i = kani::any();
    modify(&mut i);
}
