// Copyright Kani Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
//! This module contains code for processing Rust attributes (like `kani::proof`).

use std::collections::BTreeMap;

use kani_metadata::{CbmcSolver, HarnessAttributes, Stub};
use rustc_ast::{
    attr, AttrArgs, AttrArgsEq, AttrKind, Attribute, LitKind, MetaItem, MetaItemKind,
    NestedMetaItem,
};
use rustc_errors::ErrorGuaranteed;
use rustc_hir::{
    def::DefKind,
    def_id::{DefId, LocalDefId},
};
use rustc_middle::ty::{Instance, TyCtxt, TyKind};
use rustc_span::Span;
use std::str::FromStr;
use strum_macros::{AsRefStr, EnumString};

use tracing::{debug, trace};

use super::resolve;

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
    Requires,
    Ensures,
}

impl KaniAttributeKind {
    /// Returns whether an item is only relevant for harnesses.
    pub fn is_harness_only(self) -> bool {
        match self {
            KaniAttributeKind::Proof
            | KaniAttributeKind::ShouldPanic
            | KaniAttributeKind::Solver
            | KaniAttributeKind::Stub
            | KaniAttributeKind::Unwind => true,
            KaniAttributeKind::Unstable
            | KaniAttributeKind::Ensures
            | KaniAttributeKind::Requires => false,
        }
    }
}

/// Check that all attributes assigned to an item is valid.
/// Errors will be added to the session. Invoke self.tcx.sess.abort_if_errors() to terminate
/// the session and emit all errors found.
pub(super) fn check_attributes(tcx: TyCtxt, def_id: DefId) {
    let attributes = extract_kani_attributes(tcx, def_id);

    // Check that all attributes are correctly used and well formed.
    let is_harness = attributes.contains_key(&KaniAttributeKind::Proof);
    for (kind, attrs) in attributes {
        if !is_harness && kind.is_harness_only() {
            tcx.sess.span_err(
                attrs[0].span,
                format!(
                    "the `{}` attribute also requires the `#[kani::proof]` attribute",
                    kind.as_ref()
                )
                .as_str(),
            );
        }
        match kind {
            KaniAttributeKind::ShouldPanic => {
                expect_single(tcx, kind, &attrs);
                attrs.iter().for_each(|attr| {
                    expect_no_args(tcx, kind, attr);
                })
            }
            KaniAttributeKind::Solver => {
                expect_single(tcx, kind, &attrs);
                attrs.iter().for_each(|attr| {
                    parse_solver(tcx, attr);
                })
            }
            KaniAttributeKind::Stub => {
                parse_stubs(tcx, def_id, attrs);
            }
            KaniAttributeKind::Unwind => {
                expect_single(tcx, kind, &attrs);
                attrs.iter().for_each(|attr| {
                    parse_unwind(tcx, attr);
                })
            }
            KaniAttributeKind::Proof => {
                expect_single(tcx, kind, &attrs);
                attrs.iter().for_each(|attr| check_proof_attribute(tcx, def_id, attr))
            }
            KaniAttributeKind::Unstable => attrs.iter().for_each(|attr| {
                let _ = UnstableAttribute::try_from(*attr).map_err(|err| err.report(tcx));
            }),
            KaniAttributeKind::Ensures | KaniAttributeKind::Requires => (),
        };
    }
}

pub fn is_proof_harness(tcx: TyCtxt, def_id: DefId) -> bool {
    let attributes = extract_kani_attributes(tcx, def_id);
    attributes.contains_key(&KaniAttributeKind::Proof)
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

/// Extract harness attributes for a given `def_id`.
///
/// We only extract attributes for harnesses that are local to the current crate.
/// Note that all attributes should be valid by now.
pub fn extract_harness_attributes(tcx: TyCtxt, def_id: DefId) -> HarnessAttributes {
    // Abort if not local.
    assert!(def_id.is_local(), "Expected a local item, but got: {def_id:?}");
    let attributes = extract_kani_attributes(tcx, def_id);
    trace!(?def_id, ?attributes, "extract_harness_attributes");
    assert!(attributes.contains_key(&KaniAttributeKind::Proof));
    attributes.into_iter().fold(HarnessAttributes::default(), |mut harness, (kind, attributes)| {
        match kind {
            KaniAttributeKind::ShouldPanic => harness.should_panic = true,
            KaniAttributeKind::Solver => {
                harness.solver = parse_solver(tcx, attributes[0]);
            }
            KaniAttributeKind::Stub => {
                harness.stubs = parse_stubs(tcx, def_id, attributes);
            }
            KaniAttributeKind::Unwind => harness.unwind_value = parse_unwind(tcx, attributes[0]),
            KaniAttributeKind::Proof => harness.proof = true,
            KaniAttributeKind::Unstable => {
                // Internal attribute which shouldn't exist here.
                unreachable!()
            }
            KaniAttributeKind::Ensures | KaniAttributeKind::Requires => {
                todo!("Contract attributes are not supported on proofs (yet)")
            }
        };
        harness
    })
}

/// Extract function contracts on this item.
///
/// This parses the annotation and resolves the mentioned implementation
/// functions for the contract.
///
/// If no contract annotations are found the return value of this function will
/// simply not be [`enforceable()`](super::contracts::GFnContract::enforceable) and can be ignored.
pub fn extract_contract(tcx: TyCtxt, local_def_id: LocalDefId) -> super::contracts::FnContract {
    use rustc_ast::ExprKind;
    use rustc_hir::{Item, ItemKind, Mod, Node};
    let hir_map = tcx.hir();
    let hir_id = hir_map.local_def_id_to_hir_id(local_def_id);
    let find_sibling_by_name = |name| {
        let find_in_mod = |md: &Mod<'_>| {
            md.item_ids.iter().find(|it| hir_map.item(**it).ident.name == name).unwrap().hir_id()
        };

        match hir_map.get_parent(hir_id) {
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
        .to_def_id()
    };

    //println!("Searching in {:?}", hir_map.module_items(enclosing_mod).map(|it| hir_map.item(it).ident.name).collect::<Vec<_>>());

    let parse_and_resolve = |attr: &Vec<&Attribute>| {
        attr.iter()
            .map(|clause| match &clause.get_normal_item().args {
                AttrArgs::Eq(_, it) => {
                    let sym = match it {
                        AttrArgsEq::Ast(expr) => match expr.kind {
                            ExprKind::Lit(tok) => LitKind::from_token_lit(tok).unwrap().str(),
                            _ => unreachable!(),
                        },
                        AttrArgsEq::Hir(lit) => lit.kind.str(),
                    }
                    .unwrap();
                    find_sibling_by_name(sym)
                }
                _ => unreachable!(),
            })
            .collect()
    };
    let attributes = extract_kani_attributes(tcx, local_def_id.to_def_id());
    let requires =
        attributes.get(&KaniAttributeKind::Requires).map_or_else(Vec::new, parse_and_resolve);
    let ensures =
        attributes.get(&KaniAttributeKind::Ensures).map_or_else(Vec::new, parse_and_resolve);
    super::contracts::FnContract::new(requires, ensures, vec![])
}

/// Check that any unstable API has been enabled. Otherwise, emit an error.
///
/// TODO: Improve error message by printing the span of the harness instead of the definition.
pub fn check_unstable_features(tcx: TyCtxt, enabled_features: &[String], def_id: DefId) {
    if !matches!(tcx.type_of(def_id).0.kind(), TyKind::FnDef(..)) {
        // skip closures due to an issue with rustc.
        // https://github.com/model-checking/kani/pull/2406#issuecomment-1534333862
        return;
    }
    let attributes = extract_kani_attributes(tcx, def_id);
    if let Some(unstable_attrs) = attributes.get(&KaniAttributeKind::Unstable) {
        for attr in unstable_attrs {
            let unstable_attr = UnstableAttribute::try_from(*attr).unwrap();
            if !enabled_features.contains(&unstable_attr.feature) {
                // Reached an unstable attribute that was not enabled.
                report_unstable_forbidden(tcx, def_id, &unstable_attr);
            } else {
                debug!(enabled=?attr, ?def_id, "check_unstable_features");
            }
        }
    }
}

/// Report misusage of an unstable feature that was not enabled.
fn report_unstable_forbidden(
    tcx: TyCtxt,
    def_id: DefId,
    unstable_attr: &UnstableAttribute,
) -> ErrorGuaranteed {
    let fn_name = tcx.def_path_str(def_id);
    tcx.sess
        .struct_err(format!(
            "Use of unstable feature `{}`: {}",
            unstable_attr.feature, unstable_attr.reason
        ))
        .span_note(tcx.def_span(def_id), format!("the function `{fn_name}` is unstable:"))
        .note(format!("see issue {} for more information", unstable_attr.issue))
        .help(format!("use `-Z {}` to enable using this function.", unstable_attr.feature))
        .emit()
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
            &format!("only one '#[kani::{}]' attribute is allowed per harness", kind.as_ref()),
        );
    }
    attr
}

/// Check that if an item is tagged with a proof_attribute, it is a valid harness.
fn check_proof_attribute(tcx: TyCtxt, def_id: DefId, proof_attribute: &Attribute) {
    let span = proof_attribute.span;
    expect_no_args(tcx, KaniAttributeKind::Proof, proof_attribute);
    if tcx.def_kind(def_id) != DefKind::Fn {
        tcx.sess.span_err(span, "the `proof` attribute can only be applied to functions");
    } else if tcx.generics_of(def_id).requires_monomorphization(tcx) {
        tcx.sess.span_err(span, "the `proof` attribute cannot be applied to generic functions");
    } else {
        let instance = Instance::mono(tcx, def_id);
        if !super::fn_abi(tcx, instance).args.is_empty() {
            tcx.sess.span_err(span, "functions used as harnesses cannot have any arguments");
        }
    }
}

/// Partition all the attributes according to their kind.
fn extract_kani_attributes(
    tcx: TyCtxt,
    def_id: DefId,
) -> BTreeMap<KaniAttributeKind, Vec<&Attribute>> {
    let all_attributes = tcx.get_attrs_unchecked(def_id);
    all_attributes.iter().fold(BTreeMap::default(), |mut result, attribute| {
        // Get the string the appears after "kanitool::" in each attribute string.
        // Ex - "proof" | "unwind" etc.
        if let Some(kind) = attr_kind(tcx, attribute) {
            result.entry(kind).or_default().push(attribute)
        }
        result
    })
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

fn parse_stubs(tcx: TyCtxt, harness: DefId, attributes: Vec<&Attribute>) -> Vec<Stub> {
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
            NestedMetaItem::MetaItem(item) => parse_path(item).ok_or(item.span),
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
