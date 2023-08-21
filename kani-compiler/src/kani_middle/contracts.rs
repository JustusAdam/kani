// Copyright Kani Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
//! Basic type definitions for function contracts.

/// Generic representation for a function contract. This is so that we can reuse
/// this type for different resolution stages if the implementation functions
/// (`C`).
///
/// Note that currently only the `assigns` clause is actually used, whereas
/// requires and ensures are handled by the frontend. We leave this struct here
/// since in theory a CBMC code gen for any clause has been implemented thus
/// this parallels the structure expected by CBMC.
#[derive(Default)]
pub struct GFnContract<C, A> {
    requires: Vec<C>,
    ensures: Vec<C>,
    assigns: Vec<A>,
    frees: Vec<A>,
}

impl<C, A> GFnContract<C, A> {
    /// Read access to all precondition clauses.
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

    pub fn frees(&self) -> &[A] {
        &self.frees
    }

    pub fn new(requires: Vec<C>, ensures: Vec<C>, assigns: Vec<A>, frees: Vec<A>) -> Self {
        Self { requires, ensures, assigns, frees }
    }
}
