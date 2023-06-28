// Copyright Kani Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
// kani-verify-fail

#[kani::ensures(kani::old(ptr) == *ptr - 1)]
#[kani::requires(*ptr < 100)]
fn modify(ptr: &mut u32) -> u32 {
    *ptr += 1;
    0
}

#[kani::proof]
fn main() {
    let mut i = 0;
    modify(&mut i);
}