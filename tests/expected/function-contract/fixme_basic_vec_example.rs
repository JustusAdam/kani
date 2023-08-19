
// kani-flags: -Zfunction-contracts
use std::alloc::{self, Layout};
use std::ptr::NonNull;
extern crate kani;

use kani::implies;

struct Arr<T> {
    ptr: NonNull<T>,
    len: usize,
    cap: usize,
}

impl<T> Arr<T> {
    fn new() -> Self {
        Self { ptr: NonNull::dangling(), cap: 0, len: 0 }
    }

    fn grow(&mut self) {
        let (new_cap, new_layout) = if self.cap == 0 {
            (1, Layout::array::<T>(1).unwrap())
        } else {
            // This can't overflow since self.cap <= isize::MAX.
            let new_cap = 2 * self.cap;

            // `Layout::array` checks that the number of bytes is <= usize::MAX,
            // but this is redundant since old_layout.size() <= isize::MAX,
            // so the `unwrap` should never fail.
            let new_layout = Layout::array::<T>(new_cap).unwrap();
            (new_cap, new_layout)
        };

        // Ensure that the new allocation doesn't exceed `isize::MAX` bytes.
        assert!(new_layout.size() <= isize::MAX as usize, "Allocation too large");

        let new_ptr = if self.cap == 0 {
            unsafe { alloc::alloc(new_layout) }
        } else {
            let old_layout = Layout::array::<T>(self.cap).unwrap();
            let old_ptr = self.ptr.as_ptr() as *mut u8;
            unsafe { alloc::realloc(old_ptr, old_layout, new_layout.size()) }
        };

        // If allocation fails, `new_ptr` will be null, in which case we abort.
        self.ptr = match NonNull::new(new_ptr as *mut T) {
            Some(p) => p,
            None => alloc::handle_alloc_error(new_layout),
        };
        self.cap = new_cap;
    }

    #[kani::ensures((*self).len <= (*self).cap)]
    #[kani::assigns((*self).ptr, (*self).len, (*self).cap)]
    fn push(&mut self, elem: T) {
        if self.len >= self.cap {
            self.grow();
        }
        unsafe {
            self.ptr.as_ptr().offset(self.len as isize).write(elem);
        }
        self.len += 1;
    }
}

#[kani::proof_for_contract(push)]
fn push_contract() {
    let _ = Box::new(0_usize);
    let mut arr = Arr::<u8>::new();
    arr.push(kani::any());
}
