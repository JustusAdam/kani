// Copyright Kani Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
//! Basic type definitions for function contracts.
use rustc_hir::def_id::DefId;
use rustc_middle::mir::Place;

/// Generic representation for a function contract. This is so that we can reuse
/// this type for different resolution stages if the implementation functions
/// (`C`).
#[derive(Default)]
pub struct GFnContract<C, A> {
    requires: Vec<C>,
    ensures: Vec<C>,
    assigns: Vec<A>,
}

pub type FnContract<'tcx> = GFnContract<DefId, Place<'tcx>>;

impl<C, A> GFnContract<C, A> {
    /// Read access to all preondition clauses.
    pub fn requires(&self) -> &[C] {
        &self.requires
    }

    /// Read access to all postcondition clauses.
    pub fn ensures(&self) -> &[C] {
        &self.ensures
    }

    pub fn assigns(&self) -> &[A] {
        &self.assigns
    }

    pub fn new(requires: Vec<C>, ensures: Vec<C>, assigns: Vec<A>) -> Self {
        Self { requires, ensures, assigns }
    }

    /// Perform a transformation on each implementation item. Usually these are
    /// resolution steps.
    pub fn map_c<C0, F: FnMut(&C) -> C0>(&self, mut f: F) -> GFnContract<C0, A>
    where
        A: Clone,
    {
        GFnContract {
            requires: self.requires.iter().map(&mut f).collect(),
            ensures: self.ensures.iter().map(&mut f).collect(),
            assigns: self.assigns.clone(),
        }
    }

    /// If this is false, then this contract has no clauses and can safely be ignored.
    pub fn enforceable(&self) -> bool {
        !self.requires().is_empty() || !self.ensures().is_empty()
    }
}
