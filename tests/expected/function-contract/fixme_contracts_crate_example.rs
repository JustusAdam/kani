/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */
// kani-flags: -Zfunction-contracts
extern crate kani;
use kani::*;
use std::collections::BTreeSet;

#[derive(Clone, Copy, Hash, Eq, PartialEq, PartialOrd, Ord)]
pub enum Title {
    DasKapital,
    TheDiaryOfAnneFrank,
    HiddenFigures,
    WhenHeavenAndEarthChangedPlaces,
}

pub fn implies(premise: bool, conclusion: bool) -> bool {
    !premise || conclusion
}

pub struct Library {
    available: BTreeSet<Title>,
    lent: BTreeSet<Title>,
}

impl Library {
    fn book_exists(&self, book_id: &Title) -> bool {
        self.available.contains(book_id) || self.lent.contains(book_id)
    }

    #[requires(!self.book_exists(book_id))]
    #[ensures(self.available.contains(book_id))]
    //#[ensures(self.available.len() == old(self.available.len()) + 1)]
    //#[ensures(self.lent.len() == old(self.lent.len()))]
    pub fn add_book(&mut self, book_id: &Title) {
        self.available.insert(*book_id);
    }

    #[requires(self.book_exists(book_id))]
    //#[ensures(implies(result, self.available.len() == old(self.available.len()) - 1))]
    //#[ensures(implies(result, self.lent.len() == old(self.lent.len()) + 1))]
    #[ensures(implies(result, self.lent.contains(book_id)))]
    #[ensures(implies(!result, self.lent.contains(book_id)))]
    pub fn lend(&mut self, book_id: &Title) -> bool {
        if self.available.contains(book_id) {
            self.available.remove(book_id);
            self.lent.insert(*book_id);
            true
        } else {
            false
        }
    }

    #[requires(self.lent.contains(book_id))]
    //#[ensures(self.lent.len() == old(self.lent.len()) - 1)]
    //#[ensures(self.available.len() == old(self.available.len()) + 1)]
    #[ensures(!self.lent.contains(book_id))]
    #[ensures(self.available.contains(book_id))]
    pub fn return_book(&mut self, book_id: &Title) {
        self.lent.remove(book_id);
        self.available.insert(*book_id);
    }
}

fn main() {
    let mut lib = Library { available: Default::default(), lent: Default::default() };

    lib.add_book(&Title::DasKapital);

    let lent_successful = lib.lend(&Title::DasKapital);
    assert!(lent_successful);

    if lent_successful {

        lib.return_book(&Title::DasKapital);
    }
}


#[kani::proof_for_contract(Library::lend)]
#[kani::unwind(10)]
fn lend_harness() {
    main()
}

// #[kani::proof]
// #[kani::stub_verified(Library::lend)]
fn lend_replace() {
    main()
}