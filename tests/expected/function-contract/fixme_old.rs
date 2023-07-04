// Copyright Kani Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
// kani-verify-fail

#[kani::ensures(*old_ptr == *ptr - 1)]
#[kani::requires(*ptr < 100)]
fn modify(ptr: &mut u32) -> u32 {
    *ptr += 1;
    0
}

#[kani::proof]
fn main() {
    let _ = Box::new(());
    let mut i = kani::any();
    modify(&mut i);
}