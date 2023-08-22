// Copyright Kani Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
//! This module contains code for processing Rust attributes (like `kani::proof`).

use std::collections::BTreeMap;

use kani_metadata::{CbmcSolver, HarnessAttributes, Stub};
use rustc_abi::FIRST_VARIANT;
use rustc_ast::{
    attr,
    token::{BinOpToken, Delimiter, Token, TokenKind},
    tokenstream::{TokenStream, TokenTree},
    AttrArgs, AttrArgsEq, AttrKind, Attribute, ExprKind, LitKind, MetaItem, MetaItemKind,
    NestedMetaItem,
};
use rustc_errors::ErrorGuaranteed;
use rustc_hir::{
    def::DefKind,
    def_id::{DefId, LocalDefId},
};
use rustc_middle::{
    mir::{Place, PlaceElem, ProjectionElem},
    ty::{Instance, TyCtxt, TyKind},
};
use rustc_session::Session;
use rustc_span::{Span, Symbol};
use std::str::FromStr;
use strum_macros::{AsRefStr, EnumString};

use tracing::{debug, trace};

use super::resolve::{self, resolve_fn, ResolveError};

#[derive(Debug, Clone, Copy, AsRefStr, EnumString, PartialEq, Eq, PartialOrd, Ord)]
#[strum(serialize_all = "snake_case")]
enum KaniAttributeKind {
    Proof,
    ShouldPanic,
    Solver,
    Stub,
    /// Attribute used to mark unstable APIs.
    Unstable,
    Unwind,
    Assigns,
    Frees,
    StubVerified,
    /// A harness, similar to [`Self::Proof`], but for checking a function
    /// contract, e.g. the contract check is substituted for the target function
    /// before the the verification runs.
    ProofForContract,
    /// Attribute on a function with a contract that identifies the code
    /// implementing the check for this contract.
    CheckedWith,
    ReplacedWith,
    MemoryHavocDummy,
    /// Attribute on a function that was auto-generated from expanding a
    /// function contract.
    IsContractGenerated,
}

impl KaniAttributeKind {
    /// Returns whether an item is only relevant for harnesses.
    pub fn is_harness_only(self) -> bool {
        match self {
            KaniAttributeKind::Proof
            | KaniAttributeKind::ShouldPanic
            | KaniAttributeKind::Solver
            | KaniAttributeKind::Stub
            | KaniAttributeKind::ProofForContract
            | KaniAttributeKind::StubVerified
            | KaniAttributeKind::Unwind => true,
            KaniAttributeKind::Unstable
            | KaniAttributeKind::Assigns
            | KaniAttributeKind::Frees
            | KaniAttributeKind::ReplacedWith
            | KaniAttributeKind::CheckedWith
            | KaniAttributeKind::MemoryHavocDummy
            | KaniAttributeKind::IsContractGenerated => false,
        }
    }

    /// Is this attribute kind one of the suite of attributes that form the
    /// function contracts API. E.g. where [`Self::is_function_contract`] is
    /// true but also auto harness attributes like `proof_for_contract`
    pub fn is_function_contract_api(self) -> bool {
        use KaniAttributeKind::*;
        self.is_function_contract() || matches!(self, ProofForContract)
    }

    /// Would this attribute be placed on a function as part of a function
    /// contract. E.g. created by `requires`, `ensures`
    pub fn is_function_contract(self) -> bool {
        use KaniAttributeKind::*;
        matches!(self, CheckedWith | ReplacedWith | IsContractGenerated)
    }
}

/// Bundles together common data used when evaluating the attributes of a given
/// function.
#[derive(Clone)]
pub struct KaniAttributes<'tcx> {
    /// Rustc type context/queries
    tcx: TyCtxt<'tcx>,
    /// The function which these attributes decorate.
    item: DefId,
    /// All attributes we found in raw format.
    map: BTreeMap<KaniAttributeKind, Vec<&'tcx Attribute>>,
}

impl<'tcx> std::fmt::Debug for KaniAttributes<'tcx> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KaniAttributes")
            .field("item", &self.tcx.def_path_debug_str(self.item))
            .field("map", &self.map)
            .finish()
    }
}

impl<'tcx> KaniAttributes<'tcx> {
    /// Perform preliminary parsing and checking for the attributes on this
    /// function
    pub fn for_item(tcx: TyCtxt<'tcx>, def_id: DefId) -> Self {
        let all_attributes = tcx.get_attrs_unchecked(def_id);
        let map = all_attributes.iter().fold(
            <BTreeMap<KaniAttributeKind, Vec<&'tcx Attribute>>>::default(),
            |mut result, attribute| {
                // Get the string the appears after "kanitool::" in each attribute string.
                // Ex - "proof" | "unwind" etc.
                if let Some(kind) = attr_kind(tcx, attribute) {
                    result.entry(kind).or_default().push(attribute)
                }
                result
            },
        );
        Self { map, tcx, item: def_id }
    }

    /// Expect that at most one attribute of this kind exists on the function
    /// and return it.
    fn expect_maybe_one(&self, kind: KaniAttributeKind) -> Option<&'tcx Attribute> {
        match self.map.get(&kind)?.as_slice() {
            [one] => Some(one),
            _ => {
                self.tcx.sess.err(format!(
                    "Too many {} attributes on {}, expected 0 or 1",
                    kind.as_ref(),
                    self.tcx.def_path_debug_str(self.item)
                ));
                None
            }
        }
    }

    /// Parse and extract the `proof_for_contract(TARGET)` attribute. The
    /// returned symbol and defid are respectively the name and id of `TARGET`,
    /// the span in the span for the attribute (contents).
    pub fn interpret_the_for_contract_attribute(&self) -> Option<(Symbol, DefId, Span)> {
        self.expect_maybe_one(KaniAttributeKind::ProofForContract).and_then(|target| {
            let name = expect_key_string_value(self.tcx.sess, target);
            let resolved = self.resolve_sibling(name.as_str());
            match resolved {
                Err(e) => {
                    self.tcx.sess.span_err(
                        target.span,
                        format!(
                            "Failed to resolve replacement function {} because {e}",
                            name.as_str()
                        ),
                    );
                    None
                }
                Ok(ok) => Some((name, ok, target.span)),
            }
        })
    }

    pub fn use_contract(&self) -> Vec<(Symbol, DefId, Span)> {
        self.map.get(&KaniAttributeKind::StubVerified).map_or_else(Vec::new, |attr| {
            attr.iter()
                .filter_map(|attr| {
                    let name = expect_key_string_value(self.tcx.sess, attr);
                    let resolved = self.resolve_sibling(name.as_str());
                    match resolved {
                        Err(e) => {
                            self.tcx.sess.span_err(
                                attr.span,
                                format!(
                                    "Sould not resolve replacement function {} because {e}",
                                    name.as_str()
                                ),
                            );
                            None
                        }
                        Ok(ok) => Some((name, ok, attr.span)),
                    }
                })
                .collect()
        })
    }

    /// Extact the name of the sibling function this contract is checked with
    /// (if any)
    pub fn checked_with(&self) -> Option<Symbol> {
        self.expect_maybe_one(KaniAttributeKind::CheckedWith)
            .map(|target| expect_key_string_value(self.tcx.sess, target))
    }

    pub fn replaced_with(&self) -> Option<Symbol> {
        self.expect_maybe_one(KaniAttributeKind::ReplacedWith)
            .map(|target| expect_key_string_value(self.tcx.sess, target))
    }

    pub fn memory_havoc_dummy(&self) -> Option<DefId> {
        use rustc_hir::{Item, ItemKind, Mod, Node};
        let name = self
            .expect_maybe_one(KaniAttributeKind::MemoryHavocDummy)
            .map(|target| expect_key_string_value(self.tcx.sess, target))?;

        let hir_map = self.tcx.hir();
        let hir_id = hir_map.local_def_id_to_hir_id(self.item.expect_local());
        let find_in_mod = |md: &Mod<'_>| {
            md.item_ids.iter().find(|it| hir_map.item(**it).ident.name == name).unwrap().hir_id()
        };

        let result = match hir_map.get_parent(hir_id) {
            Node::Item(Item { kind, .. }) => match kind {
                ItemKind::Mod(m) => find_in_mod(m),
                ItemKind::Impl(imp) => {
                    imp.items.iter().find(|it| it.ident.name == name).unwrap().id.hir_id()
                }
                other => panic!("Odd parent item kind {other:?}"),
            },
            Node::Crate(m) => find_in_mod(m),
            other => panic!("Odd prant node type {other:?}"),
        }
        .expect_owner()
        .def_id
        .to_def_id();
        Some(result)
    }

    fn resolve_sibling(&self, path_str: &str) -> Result<DefId, ResolveError<'tcx>> {
        resolve_fn(self.tcx, self.tcx.parent_module_from_def_id(self.item.expect_local()), path_str)
    }

    /// Check that all attributes assigned to an item is valid.
    /// Errors will be added to the session. Invoke self.tcx.sess.abort_if_errors() to terminate
    /// the session and emit all errors found.
    pub(super) fn check_attributes(&self) {
        // Check that all attributes are correctly used and well formed.
        let is_harness = self.is_harness();
        for (&kind, attrs) in self.map.iter() {
            if !is_harness && kind.is_harness_only() {
                self.tcx.sess.span_err(
                    attrs[0].span,
                    format!(
                        "the `{}` attribute also requires the `#[kani::proof]` attribute",
                        kind.as_ref()
                    ),
                );
            }
            match kind {
                KaniAttributeKind::ShouldPanic => {
                    expect_single(self.tcx, kind, &attrs);
                    attrs.iter().for_each(|attr| {
                        expect_no_args(self.tcx, kind, attr);
                    })
                }
                KaniAttributeKind::Solver => {
                    expect_single(self.tcx, kind, &attrs);
                    attrs.iter().for_each(|attr| {
                        parse_solver(self.tcx, attr);
                    })
                }
                KaniAttributeKind::Stub => {
                    parse_stubs(self.tcx, self.item, attrs);
                }
                KaniAttributeKind::Unwind => {
                    expect_single(self.tcx, kind, &attrs);
                    attrs.iter().for_each(|attr| {
                        parse_unwind(self.tcx, attr);
                    })
                }
                KaniAttributeKind::Proof => {
                    assert!(!self.map.contains_key(&KaniAttributeKind::ProofForContract));
                    expect_single(self.tcx, kind, &attrs);
                    attrs.iter().for_each(|attr| self.check_proof_attribute(attr))
                }
                KaniAttributeKind::Unstable => attrs.iter().for_each(|attr| {
                    let _ = UnstableAttribute::try_from(*attr).map_err(|err| err.report(self.tcx));
                }),
                KaniAttributeKind::ProofForContract => {
                    assert!(!self.map.contains_key(&KaniAttributeKind::Proof));
                    expect_single(self.tcx, kind, &attrs);
                }
                KaniAttributeKind::StubVerified => {}
                KaniAttributeKind::CheckedWith
                | KaniAttributeKind::ReplacedWith
                | KaniAttributeKind::MemoryHavocDummy => {
                    self.expect_maybe_one(kind)
                        .map(|attr| expect_key_string_value(&self.tcx.sess, attr));
                }
                KaniAttributeKind::IsContractGenerated => {
                    // Ignored here because this is only used by the proc macros
                    // to communicate with one another. So by the time it gets
                    // here we don't care if it's valid or not.
                }
                KaniAttributeKind::Assigns => {
                    self.assigns_contract();
                }
                KaniAttributeKind::Frees => {
                    self.frees_contract();
                }
            }
        }
    }

    /// Check that any unstable API has been enabled. Otherwise, emit an error.
    ///
    /// TODO: Improve error message by printing the span of the harness instead of the definition.
    pub fn check_unstable_features(&self, enabled_features: &[String]) {
        if !matches!(self.tcx.type_of(self.item).skip_binder().kind(), TyKind::FnDef(..)) {
            // Skip closures since it shouldn't be possible to add an unstable attribute to them.
            // We have to explicitly skip them though due to an issue with rustc:
            // https://github.com/model-checking/kani/pull/2406#issuecomment-1534333862
            return;
        }

        // If the `function-contracts` unstable feature is not enabled then no
        // function should use any of those APIs.
        if !enabled_features.iter().any(|feature| feature == "function-contracts") {
            for kind in self.map.keys().copied().filter(|a| a.is_function_contract_api()) {
                let msg = format!(
                    "Using the {} attribute requires activating the unstable `function-contracts` feature",
                    kind.as_ref()
                );
                if let Some(attr) = self.map.get(&kind).unwrap().first() {
                    self.tcx.sess.span_err(attr.span, msg);
                } else {
                    self.tcx.sess.err(msg);
                }
            }
        }

        if let Some(unstable_attrs) = self.map.get(&KaniAttributeKind::Unstable) {
            for attr in unstable_attrs {
                let unstable_attr = UnstableAttribute::try_from(*attr).unwrap();
                if !enabled_features.contains(&unstable_attr.feature) {
                    // Reached an unstable attribute that was not enabled.
                    self.report_unstable_forbidden(&unstable_attr);
                } else {
                    debug!(enabled=?attr, def_id=?self.item, "check_unstable_features");
                }
            }
        }
    }

    /// Report misusage of an unstable feature that was not enabled.
    fn report_unstable_forbidden(&self, unstable_attr: &UnstableAttribute) -> ErrorGuaranteed {
        let fn_name = self.tcx.def_path_str(self.item);
        self.tcx
            .sess
            .struct_err(format!(
                "Use of unstable feature `{}`: {}",
                unstable_attr.feature, unstable_attr.reason
            ))
            .span_note(
                self.tcx.def_span(self.item),
                format!("the function `{fn_name}` is unstable:"),
            )
            .note(format!("see issue {} for more information", unstable_attr.issue))
            .help(format!("use `-Z {}` to enable using this function.", unstable_attr.feature))
            .emit()
    }

    /// Is this item a harness? (either `proof` or `proof_for_contract`
    /// attribute are present)
    fn is_harness(&self) -> bool {
        self.map.contains_key(&KaniAttributeKind::Proof)
            || self.map.contains_key(&KaniAttributeKind::ProofForContract)
    }

    /// Extract harness attributes for a given `def_id`.
    ///
    /// We only extract attributes for harnesses that are local to the current crate.
    /// Note that all attributes should be valid by now.
    pub fn harness_attributes(&self) -> HarnessAttributes {
        // Abort if not local.
        let Some(local_id) = self.item.as_local() else {
            panic!("Expected a local item, but got: {:?}", self.item);
        };
        trace!(?self, "extract_harness_attributes");
        assert!(self.is_harness());
        let mut attrs = self.map.iter().fold(
            HarnessAttributes::default(),
            |mut harness, (kind, attributes)| {
                match kind {
                    KaniAttributeKind::ShouldPanic => harness.should_panic = true,
                    KaniAttributeKind::Solver => {
                        harness.solver = parse_solver(self.tcx, attributes[0]);
                    }
                    KaniAttributeKind::Stub => {
                        harness.stubs = parse_stubs(self.tcx, self.item, attributes);
                    }
                    KaniAttributeKind::Unwind => {
                        harness.unwind_value = parse_unwind(self.tcx, attributes[0])
                    }
                    KaniAttributeKind::Proof | KaniAttributeKind::ProofForContract => {
                        harness.proof = true
                    }
                    KaniAttributeKind::Unstable => {
                        // Internal attribute which shouldn't exist here.
                        unreachable!()
                    }
                    KaniAttributeKind::CheckedWith
                    | KaniAttributeKind::IsContractGenerated
                    | KaniAttributeKind::Assigns
                    | KaniAttributeKind::Frees
                    | KaniAttributeKind::MemoryHavocDummy
                    | KaniAttributeKind::ReplacedWith => {
                        todo!("Contract attributes are not supported on proofs")
                    }
                    KaniAttributeKind::StubVerified => {}
                };
                harness
            },
        );
        let current_module = self.tcx.parent_module_from_def_id(local_id);
        attrs.stubs.extend(
            self.interpret_the_for_contract_attribute()
                .and_then(|(name, id, span)| {
                    let replacement_name = KaniAttributes::for_item(self.tcx, id).checked_with();
                    if replacement_name.is_none() {
                        self.tcx
                            .sess
                            .span_err(span, "Target function for this check has no contract");
                    }
                    Some((name, replacement_name?))
                })
                .into_iter()
                .chain(self.use_contract().into_iter().filter_map(|(name, id, span)| {
                    let replacement_name = KaniAttributes::for_item(self.tcx, id).replaced_with();
                    if replacement_name.is_none() {
                        self.tcx.sess.span_err(
                            span,
                            "The target item of this verified stubbing has no contract",
                        );
                    }
                    Some((name, replacement_name?))
                }))
                .map(|(original, replacement)| {
                    let replace_str = replacement.as_str();
                    let original_str = original.as_str();
                    let replacement = original_str.rsplit_once("::").map_or_else(
                        || replace_str.to_string(),
                        |t| t.0.to_string() + "::" + replace_str,
                    );
                    resolve::resolve_fn(self.tcx, current_module, &replacement).unwrap();
                    Stub { original: original_str.to_string(), replacement }
                }),
        );
        attrs
    }

    /// Check that if this item is tagged with a proof_attribute, it is a valid harness.
    fn check_proof_attribute(&self, proof_attribute: &Attribute) {
        let span = proof_attribute.span;
        let tcx = self.tcx;
        expect_no_args(tcx, KaniAttributeKind::Proof, proof_attribute);
        if tcx.def_kind(self.item) != DefKind::Fn {
            tcx.sess.span_err(span, "the `proof` attribute can only be applied to functions");
        } else if tcx.generics_of(self.item).requires_monomorphization(tcx) {
            tcx.sess.span_err(span, "the `proof` attribute cannot be applied to generic functions");
        } else {
            let instance = Instance::mono(tcx, self.item);
            if !super::fn_abi(tcx, instance).args.is_empty() {
                tcx.sess.span_err(span, "functions used as harnesses cannot have any arguments");
            }
        }
    }

    pub fn assigns_contract(&self) -> Option<Vec<AssignsRange<'tcx>>> {
        let local_def_id = self.item.expect_local();
        self.map.get(&KaniAttributeKind::Assigns).map(|attr| {
            attr.iter()
                .flat_map(|clause| match &clause.get_normal_item().args {
                    AttrArgs::Delimited(lvals) => {
                        parse_assign_values(self.tcx, local_def_id, &lvals.tokens)
                    }
                    _ => unreachable!(),
                })
                .collect()
        })
    }

    pub fn frees_contract(&self) -> Option<Vec<Place<'tcx>>> {
        let local_def_id = self.item.expect_local();
        self.map.get(&KaniAttributeKind::Frees).map(|attr| {
            attr.iter()
                .flat_map(|clause| match &clause.get_normal_item().args {
                    AttrArgs::Delimited(lvals) => {
                        parse_frees_values(self.tcx, local_def_id, &lvals.tokens)
                    }
                    _ => unreachable!(),
                })
                .collect()
        })
    }
}

/// An efficient check for the existence for a particular [`KaniAttributeKind`].
/// Unlike querying [`KaniAttributes`] this method builds no new heap data
/// structures and has short circuiting.
fn has_kani_attribute<F: Fn(KaniAttributeKind) -> bool>(
    tcx: TyCtxt,
    def_id: DefId,
    predicate: F,
) -> bool {
    tcx.get_attrs_unchecked(def_id).iter().filter_map(|a| attr_kind(tcx, a)).any(predicate)
}

#[allow(dead_code)]
/// Test is this function was generated by expanding a contract attribute like
/// `requires` and `ensures`
pub fn is_function_contract_generated(tcx: TyCtxt, def_id: DefId) -> bool {
    has_kani_attribute(tcx, def_id, KaniAttributeKind::is_function_contract)
}

/// Same as [`KaniAttributes::is_harness`] but more efficient because less
/// attribute parsing is performed.
pub fn is_proof_harness(tcx: TyCtxt, def_id: DefId) -> bool {
    has_kani_attribute(tcx, def_id, |a| {
        matches!(a, KaniAttributeKind::Proof | KaniAttributeKind::ProofForContract)
    })
}

/// Does this `def_id` have `#[rustc_test_marker]`?
pub fn is_test_harness_description(tcx: TyCtxt, def_id: DefId) -> bool {
    let attrs = tcx.get_attrs_unchecked(def_id);
    attr::contains_name(attrs, rustc_span::symbol::sym::rustc_test_marker)
}

/// Extract the test harness name from the `#[rustc_test_maker]`
pub fn test_harness_name(tcx: TyCtxt, def_id: DefId) -> String {
    let attrs = tcx.get_attrs_unchecked(def_id);
    let marker = attr::find_by_name(attrs, rustc_span::symbol::sym::rustc_test_marker).unwrap();
    parse_str_value(&marker).unwrap()
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct AssignsRange<'tcx> {
    base: Place<'tcx>,
    slice: Option<(Option<Place<'tcx>>, Option<Place<'tcx>>)>,
}

impl<'tcx> AssignsRange<'tcx> {
    fn expect_base_only(self) -> Place<'tcx> {
        assert!(self.slice.is_none());
        self.base
    }

    fn project_deeper(mut self, projections: &[PlaceElem<'tcx>], tcx: TyCtxt<'tcx>) -> Self {
        self.base = self.base.project_deeper(projections, tcx);
        self
    }

    pub fn base(self) -> Place<'tcx> {
        self.base
    }

    pub fn slice(self) -> Option<(Option<Place<'tcx>>, Option<Place<'tcx>>)> {
        self.slice
    }
}

struct UntilDotDot<I> {
    inner: I,
    seen_dot_dot: bool,
}
macro_rules! comma_tok {
    () => {
        TokenTree::Token(Token { kind: TokenKind::Comma, .. }, _)
    };
}
macro_rules! dot_dot_tok {
    () => {
        TokenTree::Token(Token { kind: TokenKind::DotDot, .. }, _)
    };
}

impl<'a, I: Iterator<Item = &'a TokenTree>> Iterator for UntilDotDot<I> {
    type Item = &'a TokenTree;
    fn next(&mut self) -> Option<Self::Item> {
        if self.seen_dot_dot {
            return None;
        }
        let nxt = self.inner.next()?;
        if matches!(nxt, dot_dot_tok!()) {
            self.seen_dot_dot;
            None
        } else {
            Some(nxt)
        }
    }
}

impl<I> UntilDotDot<I> {
    fn new(inner: I) -> Self {
        Self { inner, seen_dot_dot: false }
    }
}
/// Parse a place (base variable with a series of projections) from an iterator
/// over a tokens.
///
/// Terminates when the iterator is empty *or* a `,` token is encountered.
/// Guarantees to leave the iterator either empty *or* at the token directly
/// after `,`.
///
/// Returns `None` if no tokens are encountered before iterator end or the `,`
/// token or if there were errors, which are emitted with `tcx.sess`
fn parse_place<'tcx, 'b, I: Iterator<Item = &'b TokenTree>>(
    tcx: TyCtxt<'tcx>,
    local_def_id: LocalDefId,
    t: &mut I,
    deny_wildcard_subslice: Option<&str>,
) -> Option<AssignsRange<'tcx>> {
    let mir = tcx.optimized_mir(local_def_id);

    let local_decls = &mir.local_decls;
    // Skips the iterator forward until either it is empty or a `,` token is encountered
    let skip = |t: &mut I| {
        let _ = t.by_ref().skip_while(|t| matches!(t, comma_tok!())).count();
    };
    if let Some(tree) = t.next() {
        let barks_up_the_wrong_tree = || {
            tcx.sess.span_fatal(
                tree.span(),
                "Parse error in assigns clause, expected `*`, identifier or parentheses",
            )
        };
        let mut base: AssignsRange<'tcx> = match tree {
            TokenTree::Delimited(_, Delimiter::Parenthesis, inner) => {
                let mut it = inner.trees();
                let Some(res) = parse_place(tcx, local_def_id, &mut it, deny_wildcard_subslice)
                else {
                    tcx.sess.span_err(tree.span(), "Expected an lvalue in the parentheses");
                    return None;
                };
                if !it.next().is_none() {
                    tcx.sess.span_err(
                        tree.span(),
                        "Expected only one lvalue in parenthesized expression",
                    );
                }
                res
            }
            TokenTree::Token(token, _) => match &token.kind {
                TokenKind::Ident(id, _) => {
                    let hir = tcx.hir();
                    let bid = hir.body_owned_by(local_def_id);
                    let local = hir
                        .body_param_names(bid)
                        .zip(mir.args_iter())
                        .find(|(name, _decl)| name.name == *id)
                        .unwrap()
                        .1;
                    AssignsRange { base: Place::from(local), slice: None }
                }
                TokenKind::BinOp(BinOpToken::Star) => {
                    let Some(res) = parse_place(tcx, local_def_id, t, deny_wildcard_subslice)
                        .map(|p| p.project_deeper(&[ProjectionElem::Deref], tcx))
                    else {
                        tcx.sess.span_err(
                            tree.span(),
                            "Expected this dereference to be followed by an lvalue",
                        );
                        skip(t);
                        return None;
                    };
                    res
                }
                _ => barks_up_the_wrong_tree(),
            },
            _ => barks_up_the_wrong_tree(),
        };
        while let Some(tree) = t.next() {
            match tree {
                comma_tok!() => break,
                TokenTree::Token(token, _) => match &token.kind {
                    TokenKind::Dot => match t.next() {
                        Some(
                            tok @ TokenTree::Token(
                                Token { kind: TokenKind::Ident(field, _), .. },
                                _,
                            ),
                        ) => {
                            let pty = base.expect_base_only().ty(local_decls, tcx);
                            let (adt_def, _) = match pty.ty.kind() {
                                TyKind::Adt(adt, substs) => (adt, substs),
                                _ => panic!(),
                            };
                            let variant_index = pty.variant_index.unwrap_or_else(|| {
                                assert!(adt_def.is_struct());
                                FIRST_VARIANT
                            });
                            let fidx = adt_def
                                .variant(variant_index)
                                .fields
                                .iter_enumerated()
                                .find(|(_idx, fdef)| fdef.name == *field)
                                .unwrap_or_else(|| {
                                    tcx.sess.span_fatal(
                                        tok.span(),
                                        format!(
                                            "Could not find field {field} in type {:?}",
                                            pty.ty
                                        ),
                                    )
                                })
                                .0;
                            let more_projections =
                                [ProjectionElem::Field(fidx, pty.field_ty(tcx, fidx))];
                            base = base.project_deeper(&more_projections, tcx);
                        }
                        thing => panic!("Incomplete field expression {thing:?}"),
                    },
                    _ => panic!("Unexpected token {tree:?}"),
                },
                tok @ TokenTree::Delimited(_, Delimiter::Bracket, inner) => {
                    let mut it = inner.trees();
                    let mut until_dot_dot = UntilDotDot::new(it.by_ref());
                    let from =
                        parse_place(tcx, local_def_id, &mut until_dot_dot, deny_wildcard_subslice)
                            .map(AssignsRange::expect_base_only);
                    let saw_dot_dot = until_dot_dot.seen_dot_dot;
                    assert!(!saw_dot_dot || it.next().is_none());
                    let to = parse_place(tcx, local_def_id, &mut it, deny_wildcard_subslice)
                        .map(AssignsRange::expect_base_only);

                    if !matches!(t.next(), None | Some(comma_tok!())) {
                        tcx.sess.span_err(
                            tok.span(),
                            "Slice pattern is only supported as last projection",
                        );
                    }
                    if let Some(elem) = deny_wildcard_subslice {
                        tcx.sess.span_err(
                            tok.span(),
                            format!("Subslice pattern is not allowed in {}.", elem),
                        );
                    }
                    return Some(AssignsRange {
                        base: base.expect_base_only(),
                        slice: Some((from, to)),
                    });
                }
                tok => tcx.sess.span_fatal(
                    tok.span(),
                    "Unexpected token, expected field projection, slice pattern or comma",
                ),
            }
        }
        Some(base)
    } else {
        None
    }
}

fn parse_assign_values<'tcx: 'a, 'a>(
    tcx: TyCtxt<'tcx>,
    local_def_id: LocalDefId,
    t: &'a TokenStream,
) -> impl Iterator<Item = AssignsRange<'tcx>> + 'a {
    let mut it = t.trees().peekable();
    std::iter::from_fn(move || {
        it.peek().is_some().then(|| parse_place(tcx, local_def_id, &mut it, None))
    })
    .filter_map(std::convert::identity)
}

fn parse_frees_values<'tcx: 'a, 'a>(
    tcx: TyCtxt<'tcx>,
    local_def_id: LocalDefId,
    t: &'a TokenStream,
) -> impl Iterator<Item = Place<'tcx>> + 'a {
    let mut it = t.trees().peekable();
    std::iter::from_fn(move || {
        it.peek().is_some().then(|| parse_place(tcx, local_def_id, &mut it, Some("`frees` clause")))
    })
    .filter_map(std::convert::identity)
    .map(AssignsRange::expect_base_only)
}

/// Expect the contents of this attribute to be of the format #[attribute =
/// "value"] and return the `"value"`
fn expect_key_string_value(sess: &Session, attr: &Attribute) -> rustc_span::Symbol {
    let span = attr.span;
    let AttrArgs::Eq(_, it) = &attr.get_normal_item().args else {
        sess.span_fatal(span, "Expected attribute of the form #[attr = \"value\"]")
    };
    let maybe_str = match it {
        AttrArgsEq::Ast(expr) => match expr.kind {
            ExprKind::Lit(tok) => LitKind::from_token_lit(tok).unwrap().str(),
            _ => sess.span_fatal(span, "Expected literal string as right hand side of `=`"),
        },
        AttrArgsEq::Hir(lit) => lit.kind.str(),
    };
    if let Some(str) = maybe_str {
        str
    } else {
        sess.span_fatal(span, "Expected literal string as right hand side of `=`")
    }
}

fn expect_single<'a>(
    tcx: TyCtxt,
    kind: KaniAttributeKind,
    attributes: &'a Vec<&'a Attribute>,
) -> &'a Attribute {
    let attr = attributes
        .first()
        .expect(&format!("expected at least one attribute {} in {attributes:?}", kind.as_ref()));
    if attributes.len() > 1 {
        tcx.sess.span_err(
            attr.span,
            format!("only one '#[kani::{}]' attribute is allowed per harness", kind.as_ref()),
        );
    }
    attr
}

/// Attribute used to mark a Kani lib API unstable.
#[derive(Debug)]
struct UnstableAttribute {
    /// The feature identifier.
    feature: String,
    /// A link to the stabilization tracking issue.
    issue: String,
    /// A user friendly message that describes the reason why this feature is marked as unstable.
    reason: String,
}

#[derive(Debug)]
struct UnstableAttrParseError<'a> {
    /// The reason why the parsing failed.
    reason: String,
    /// The attribute being parsed.
    attr: &'a Attribute,
}

impl<'a> UnstableAttrParseError<'a> {
    /// Report the error in a friendly format.
    fn report(&self, tcx: TyCtxt) -> ErrorGuaranteed {
        tcx.sess
            .struct_span_err(
                self.attr.span,
                format!("failed to parse `#[kani::unstable]`: {}", self.reason),
            )
            .note(format!(
                "expected format: #[kani::unstable({}, {}, {})]",
                r#"feature="<IDENTIFIER>""#, r#"issue="<ISSUE>""#, r#"reason="<DESCRIPTION>""#
            ))
            .emit()
    }
}

/// Try to parse an unstable attribute into an `UnstableAttribute`.
impl<'a> TryFrom<&'a Attribute> for UnstableAttribute {
    type Error = UnstableAttrParseError<'a>;
    fn try_from(attr: &'a Attribute) -> Result<Self, Self::Error> {
        let build_error = |reason: String| Self::Error { reason, attr };
        let args = parse_key_values(attr).map_err(build_error)?;
        let invalid_keys = args
            .iter()
            .filter_map(|(key, _)| {
                (!matches!(key.as_str(), "feature" | "issue" | "reason")).then_some(key)
            })
            .cloned()
            .collect::<Vec<_>>();

        if !invalid_keys.is_empty() {
            Err(build_error(format!("unexpected argument `{}`", invalid_keys.join("`, `"))))
        } else {
            let get_val = |name: &str| {
                args.get(name).cloned().ok_or(build_error(format!("missing `{name}` field")))
            };
            Ok(UnstableAttribute {
                feature: get_val("feature")?,
                issue: get_val("issue")?,
                reason: get_val("reason")?,
            })
        }
    }
}

fn expect_no_args(tcx: TyCtxt, kind: KaniAttributeKind, attr: &Attribute) {
    if !attr.is_word() {
        tcx.sess
            .struct_span_err(attr.span, format!("unexpected argument for `{}`", kind.as_ref()))
            .help("remove the extra argument")
            .emit();
    }
}

/// Return the unwind value from the given attribute.
fn parse_unwind(tcx: TyCtxt, attr: &Attribute) -> Option<u32> {
    // Get Attribute value and if it's not none, assign it to the metadata
    match parse_integer(attr) {
        None => {
            // There are no integers or too many arguments given to the attribute
            tcx.sess.span_err(
                attr.span,
                "invalid argument for `unwind` attribute, expected an integer",
            );
            None
        }
        Some(unwind_integer_value) => {
            if let Ok(val) = unwind_integer_value.try_into() {
                Some(val)
            } else {
                tcx.sess.span_err(attr.span, "value above maximum permitted value - u32::MAX");
                None
            }
        }
    }
}

fn parse_stubs(tcx: TyCtxt, harness: DefId, attributes: &[&Attribute]) -> Vec<Stub> {
    let current_module = tcx.parent_module_from_def_id(harness.expect_local());
    let check_resolve = |attr: &Attribute, name: &str| {
        let result = resolve::resolve_fn(tcx, current_module, name);
        if let Err(err) = result {
            tcx.sess.span_err(attr.span, format!("failed to resolve `{name}`: {err}"));
        }
    };
    attributes
        .iter()
        .filter_map(|attr| match parse_paths(attr) {
            Ok(paths) => match paths.as_slice() {
                [orig, replace] => {
                    check_resolve(attr, orig);
                    check_resolve(attr, replace);
                    Some(Stub { original: orig.clone(), replacement: replace.clone() })
                }
                _ => {
                    tcx.sess.span_err(
                        attr.span,
                        format!(
                            "attribute `kani::stub` takes two path arguments; found {}",
                            paths.len()
                        ),
                    );
                    None
                }
            },
            Err(error_span) => {
                tcx.sess.span_err(
                    error_span,
                        "attribute `kani::stub` takes two path arguments; found argument that is not a path",
                );
                None
            }
        })
        .collect()
}

fn parse_solver(tcx: TyCtxt, attr: &Attribute) -> Option<CbmcSolver> {
    // TODO: Argument validation should be done as part of the `kani_macros` crate
    // <https://github.com/model-checking/kani/issues/2192>
    const ATTRIBUTE: &str = "#[kani::solver]";
    let invalid_arg_err = |attr: &Attribute| {
        tcx.sess.span_err(
                attr.span,
                format!("invalid argument for `{ATTRIBUTE}` attribute, expected one of the supported solvers (e.g. `kissat`) or a SAT solver binary (e.g. `bin=\"<SAT_SOLVER_BINARY>\"`)")
            )
    };

    let attr_args = attr.meta_item_list().unwrap();
    if attr_args.len() != 1 {
        tcx.sess.span_err(
            attr.span,
            format!(
                "the `{ATTRIBUTE}` attribute expects a single argument. Got {} arguments.",
                attr_args.len()
            ),
        );
        return None;
    }
    let attr_arg = &attr_args[0];
    let meta_item = attr_arg.meta_item();
    if meta_item.is_none() {
        invalid_arg_err(attr);
        return None;
    }
    let meta_item = meta_item.unwrap();
    let ident = meta_item.ident().unwrap();
    let ident_str = ident.as_str();
    match &meta_item.kind {
        MetaItemKind::Word => {
            let solver = CbmcSolver::from_str(ident_str);
            match solver {
                Ok(solver) => Some(solver),
                Err(_) => {
                    tcx.sess.span_err(attr.span, format!("unknown solver `{ident_str}`"));
                    None
                }
            }
        }
        MetaItemKind::NameValue(lit) if ident_str == "bin" && lit.kind.is_str() => {
            Some(CbmcSolver::Binary(lit.symbol.to_string()))
        }
        _ => {
            invalid_arg_err(attr);
            None
        }
    }
}

/// Extracts the integer value argument from the attribute provided
/// For example, `unwind(8)` return `Some(8)`
fn parse_integer(attr: &Attribute) -> Option<u128> {
    // Vector of meta items , that contain the arguments given the attribute
    let attr_args = attr.meta_item_list()?;
    // Only extracts one integer value as argument
    if attr_args.len() == 1 {
        let x = attr_args[0].lit()?;
        match x.kind {
            LitKind::Int(y, ..) => Some(y),
            _ => None,
        }
    }
    // Return none if there are no attributes or if there's too many attributes
    else {
        None
    }
}

/// Extracts a vector with the path arguments of an attribute.
/// Emits an error if it couldn't convert any of the arguments.
fn parse_paths(attr: &Attribute) -> Result<Vec<String>, Span> {
    let attr_args = attr.meta_item_list();
    attr_args
        .unwrap_or_default()
        .iter()
        .map(|arg| match arg {
            NestedMetaItem::Lit(item) => Err(item.span),
            NestedMetaItem::MetaItem(item) => parse_path(&item).ok_or(item.span),
        })
        .collect()
}

/// Extracts a path from an attribute item, returning `None` if the item is not
/// syntactically a path.
fn parse_path(meta_item: &MetaItem) -> Option<String> {
    if meta_item.is_word() {
        Some(
            meta_item
                .path
                .segments
                .iter()
                .map(|seg| seg.ident.as_str())
                .collect::<Vec<&str>>()
                .join("::"),
        )
    } else {
        None
    }
}

/// Parse the arguments of the attribute into a (key, value) map.
fn parse_key_values(attr: &Attribute) -> Result<BTreeMap<String, String>, String> {
    trace!(list=?attr.meta_item_list(), ?attr, "parse_key_values");
    let args = attr.meta_item_list().ok_or("malformed attribute input")?;
    args.iter()
        .map(|arg| match arg.meta_item() {
            Some(MetaItem { path: key, kind: MetaItemKind::NameValue(val), .. }) => {
                Ok((key.segments.first().unwrap().ident.to_string(), val.symbol.to_string()))
            }
            _ => Err(format!(
                r#"expected "key = value" pair, but found `{}`"#,
                rustc_ast_pretty::pprust::meta_list_item_to_string(arg)
            )),
        })
        .collect()
}

/// Extracts the string value argument from the attribute provided.
///
/// For attributes with the following format, this will return a string that represents "VALUE".
/// - `#[attribute = "VALUE"]`
fn parse_str_value(attr: &Attribute) -> Option<String> {
    // Vector of meta items , that contain the arguments given the attribute
    let value = attr.value_str();
    value.map(|sym| sym.to_string())
}

/// If the attribute is named `kanitool::name`, this extracts `name`
fn attr_kind(tcx: TyCtxt, attr: &Attribute) -> Option<KaniAttributeKind> {
    match &attr.kind {
        AttrKind::Normal(normal) => {
            let segments = &normal.item.path.segments;
            if (!segments.is_empty()) && segments[0].ident.as_str() == "kanitool" {
                let ident_str = segments[1..]
                    .iter()
                    .map(|segment| segment.ident.as_str())
                    .intersperse("::")
                    .collect::<String>();
                KaniAttributeKind::try_from(ident_str.as_str())
                    .map_err(|err| {
                        debug!(?err, "attr_kind_failed");
                        tcx.sess.span_err(attr.span, format!("unknown attribute `{ident_str}`"));
                        err
                    })
                    .ok()
            } else {
                None
            }
        }
        _ => None,
    }
}
