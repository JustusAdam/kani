// Copyright Kani Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
use std::collections::{HashMap, HashSet};

use proc_macro::TokenStream;

use {
    quote::{quote, ToTokens},
    syn::{
        parse_macro_input, spanned::Spanned, visit::Visit, Expr, ExprCall, ExprPath, ItemFn, Path,
        PathSegment,
    },
};

use proc_macro2::{Ident, Span, TokenStream as TokenStream2};

/// Create a unique hash for a token stream (basically a [`std::hash::Hash`]
/// impl for `proc_macro2::TokenStream`).
fn hash_of_token_stream<H: std::hash::Hasher>(hasher: &mut H, stream: proc_macro2::TokenStream) {
    use proc_macro2::TokenTree;
    use std::hash::Hash;
    for token in stream {
        match token {
            TokenTree::Ident(i) => i.hash(hasher),
            TokenTree::Punct(p) => p.as_char().hash(hasher),
            TokenTree::Group(g) => {
                std::mem::discriminant(&g.delimiter()).hash(hasher);
                hash_of_token_stream(hasher, g.stream());
            }
            TokenTree::Literal(lit) => lit.to_string().hash(hasher),
        }
    }
}

macro_rules! assert_spanned_err {
    ($condition:expr, $span_source:expr, $msg:expr, $($args:expr),+) => {
        if !$condition {
            $span_source.span().unwrap().error(format!($msg, $($args),*)).emit();
            assert!(false);
        }
    };
    ($condition:expr, $span_source:expr, $msg:expr $(,)?) => {
        if !$condition {
            $span_source.span().unwrap().error($msg).emit();
            assert!(false);
        }
    };
    ($condition:expr, $span_source:expr) => {
        assert_spanned_err!($condition, $span_source, concat!("Failed assertion ", stringify!($condition)))
    };
}

use syn::{
    punctuated::Punctuated, visit_mut::VisitMut, Attribute, Block, ExprArray, ExprReference,
    ExprStruct, ExprTuple, FieldValue, FnArg, Pat, Signature, Token,
};

/// Hash this `TokenStream` and return an integer that is at most digits
/// long when hex formatted.
fn short_hash_of_token_stream(stream: &proc_macro::TokenStream) -> u64 {
    use std::hash::Hasher;
    let mut hasher = std::collections::hash_map::DefaultHasher::default();
    hash_of_token_stream(&mut hasher, proc_macro2::TokenStream::from(stream.clone()));
    let long_hash = hasher.finish();
    long_hash % 0x1_000_000 // six hex digits
}

/// Makes consistent names for a generated function which was created for
/// `purpose`, from an attribute that decorates `related_function` with the
/// hash `hash`.
fn identifier_for_generated_function(related_function: &ItemFn, purpose: &str, hash: u64) -> Ident {
    let identifier = format!("{}_{purpose}_{hash:x}", related_function.sig.ident);
    Ident::new(&identifier, proc_macro2::Span::mixed_site())
}

pub fn requires(attr: TokenStream, item: TokenStream) -> TokenStream {
    requires_ensures_alt(attr, item, true)
}

pub fn ensures(attr: TokenStream, item: TokenStream) -> TokenStream {
    requires_ensures_alt(attr, item, false)
}

pub fn assigns(attr: TokenStream, item: TokenStream) -> TokenStream {
    let item = proc_macro2::TokenStream::from(item);
    let attr = proc_macro2::TokenStream::from(attr);
    quote!(
        #[kanitool::assigns(#attr)]
        #item
    )
    .into()
}

pub fn frees(attr: TokenStream, item: TokenStream) -> TokenStream {
    let item = proc_macro2::TokenStream::from(item);
    let attr = proc_macro2::TokenStream::from(attr);
    quote!(
        #[kanitool::frees(#attr)]
        #item
    )
    .into()
}

trait OldTrigger {
    /// You are provided the expression that is the first argument of the
    /// `old()` call. You may modify it as you see fit. The return value
    /// indicates whether the entire `old()` call should be replaced by the
    /// (potentially altered) first argument.
    ///
    /// The second argument is the span of the original `old` expr
    fn trigger(&mut self, e: &mut Expr, s: Span) -> bool;
}

struct OldLifter(Vec<Expr>);

impl OldLifter {
    fn generate_identifier_for(index: usize) -> Ident {
        let gen_ident = format!("old_{index}");
        Ident::new(&gen_ident, proc_macro2::Span::mixed_site())
    }

    fn into_iter_exprs_and_idents(self) -> impl Iterator<Item = (Ident, Expr)> {
        self.0
            .into_iter()
            .enumerate()
            .map(|(index, e)| (OldLifter::generate_identifier_for(index), e))
    }

    fn new() -> Self {
        Self(vec![])
    }
}

struct OldDenier;

impl OldTrigger for OldDenier {
    fn trigger(&mut self, _: &mut Expr, s: Span) -> bool {
        s.unwrap().error("Nested calls to `old` are prohibited, because they are not well defined (what would it even mean?)").emit();
        false
    }
}

struct OldVisitor<T>(T);

impl<T: OldTrigger> OldVisitor<T> {
    fn new(t: T) -> Self {
        Self(t)
    }

    fn into_inner(self) -> T {
        self.0
    }
}

impl<T: OldTrigger> syn::visit_mut::VisitMut for OldVisitor<T> {
    fn visit_expr_mut(&mut self, ex: &mut Expr) {
        let trigger = match &*ex {
            Expr::Call(
                call @ ExprCall {
                    func:
                        box func @ Expr::Path(ExprPath {
                            attrs: func_attrs,
                            qself: None,
                            path: Path { leading_colon: None, segments },
                        }),
                    attrs,
                    args,
                    ..
                },
            ) if segments.len() == 1
                && segments.first().map_or(false, |sgm| sgm.ident == "old") =>
            {
                let first_segment = segments.first().unwrap();
                assert_spanned_err!(first_segment.arguments.is_empty(), first_segment);
                assert_spanned_err!(attrs.is_empty(), call);
                assert_spanned_err!(func_attrs.is_empty(), func);
                assert_spanned_err!(args.len() == 1, call);
                true
            }
            _ => false,
        };
        if trigger {
            let span = ex.span();
            let new_expr = if let Expr::Call(ExprCall { ref mut args, .. }) = ex {
                self.0
                    .trigger(args.iter_mut().next().unwrap(), span)
                    .then(|| args.pop().unwrap().into_value())
            } else {
                unreachable!()
            };
            if let Some(new) = new_expr {
                let _ = std::mem::replace(ex, new);
            }
        } else {
            syn::visit_mut::visit_expr_mut(self, ex)
        }
    }
}

impl OldTrigger for OldLifter {
    fn trigger(&mut self, e: &mut Expr, _: Span) -> bool {
        let mut denier = OldVisitor::new(OldDenier);
        // This ensures there are no nested calls to `old`
        denier.visit_expr_mut(e);

        self.0.push(std::mem::replace(
            e,
            Expr::Path(ExprPath {
                attrs: vec![],
                qself: None,
                path: Path {
                    leading_colon: None,
                    segments: [PathSegment {
                        ident: OldLifter::generate_identifier_for(self.0.len()),
                        arguments: syn::PathArguments::None,
                    }]
                    .into_iter()
                    .collect(),
                },
            }),
        ));
        true
    }
}

struct IdentToOldRewriter;

impl syn::visit_mut::VisitMut for IdentToOldRewriter {
    fn visit_pat_ident_mut(&mut self, i: &mut syn::PatIdent) {
        i.ident = Ident::new(&format!("old_{}", i.ident.to_string()), i.span())
    }
}

/// Collect all named identifiers used in the argument patterns of a function.
struct ArgumentIdentCollector(HashSet<Ident>);

impl ArgumentIdentCollector {
    fn new() -> Self {
        Self(HashSet::new())
    }
}

impl<'ast> Visit<'ast> for ArgumentIdentCollector {
    fn visit_pat_ident(&mut self, i: &'ast syn::PatIdent) {
        self.0.insert(i.ident.clone());
        syn::visit::visit_pat_ident(self, i)
    }
    fn visit_receiver(&mut self, _: &'ast syn::Receiver) {
        self.0.insert(Ident::new("self", proc_macro2::Span::call_site()));
    }
}

/// Applies the contained renaming (key renamed to value) to every ident pattern
/// and ident expr visited.
struct Renamer<'a>(&'a HashMap<Ident, Ident>);

impl<'a> VisitMut for Renamer<'a> {
    fn visit_expr_path_mut(&mut self, i: &mut syn::ExprPath) {
        if i.path.segments.len() == 1 {
            i.path
                .segments
                .first_mut()
                .and_then(|p| self.0.get(&p.ident).map(|new| p.ident = new.clone()));
        }
    }

    /// This restores shadowing. Without this we would rename all ident
    /// occurrences, but not rebinding location. This is because our
    /// [`visit_expr_path_mut`] is scope-unaware.
    fn visit_pat_ident_mut(&mut self, i: &mut syn::PatIdent) {
        if let Some(new) = self.0.get(&i.ident) {
            i.ident = new.clone();
        }
    }
}

/// Does the provided path have the same chain of identifiers as `mtch` (match)
/// and no arguments anywhere?
fn matches_path<E>(path: &syn::Path, mtch: &[E]) -> bool
where
    Ident: std::cmp::PartialEq<E>,
{
    path.segments.len() == mtch.len()
        && path.segments.iter().all(|s| s.arguments.is_empty())
        && path.leading_colon.is_none()
        && path.segments.iter().zip(mtch).all(|(actual, expected)| actual.ident == *expected)
}

macro_rules! swapped {
    ($src:expr, $target:expr, $code:expr) => {{
        std::mem::swap($src, $target);
        let result = $code;
        std::mem::swap($src, $target);
        result
    }};
}

/// Classifies the state a function is in in the contract handling pipeline.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ContractFunctionState {
    /// This is the original code, re-emitted from a contract attribute
    Original,
    /// This is the first time a contract attribute is evaluated on this
    /// function
    Untouched,
    /// This is a check function that was generated from a previous evaluation
    /// of a contract attribute
    Check,
    /// This is a replace function that was generated from a previous evaluation
    /// of a contract attribute
    Replace,
    ReplaceDummy,
}

impl ContractFunctionState {
    /// Find out if this attribute could be describing a "contract handling"
    /// state and if so return it.
    fn from_attribute(attribute: &syn::Attribute) -> Option<Self> {
        if let syn::Meta::List(lst) = &attribute.meta {
            if matches_path(&lst.path, &["kanitool", "is_contract_generated"]) {
                match syn::parse2::<Ident>(lst.tokens.clone()) {
                    Err(e) => {
                        lst.span().unwrap().error(format!("{e}")).emit();
                    }
                    Ok(ident) => {
                        let ident_str = ident.to_string();
                        return match ident_str.as_str() {
                            "check" => Some(Self::Check),
                            "replace" => Some(Self::Replace),
                            "replace_dummy" => Some(Self::ReplaceDummy),
                            _ => {
                                lst.span().unwrap().error("Expected `check` ident").emit();
                                None
                            }
                        };
                    }
                }
            }
        }
        if let syn::Meta::NameValue(nv) = &attribute.meta {
            if matches_path(&nv.path, &["kanitool", "checked_with"]) {
                return Some(ContractFunctionState::Original);
            }
        }
        None
    }

    fn emit_tag_attr(self) -> bool {
        matches!(self, ContractFunctionState::Untouched)
    }

    /// The only reason the `item_fn` is mutable is because we need to emit
    /// attributes in a different order and so we need to temporarily move
    /// item_fn.attrs.
    ///
    /// This function decides whether we will be emitting a check function, a
    /// replace function or both and emit a header into `output` if necessary.
    ///
    /// The first field of the returned tuple is a name for the replace
    /// function, the second for the check function.
    ///
    /// The following is going to happen depending on the state of `self`
    ///
    /// - On [`ContractFunctionState::Original`] we return an overall [`None`]
    ///   indicating to short circuit the code generation.
    /// - On [`ContractFunctionState::Replace`] and
    ///   [`ContractFunctionState::Check`] we return [`Some`] for one of the
    ///   tuple fields, indicating that only this type of function should be
    ///   emitted.
    /// - On [`ContractFunctionState::Untouched`] we return [`Some`] for both
    ///   tuple fields, indicating that both functions need to be emitted. We
    ///   also emit the original function with the `checked_with` and
    ///   `replaced_with` attributes added.
    fn prepare_header(
        self,
        item_fn: &mut ItemFn,
        output: &mut TokenStream2,
        a_short_hash: u64,
    ) -> Option<(Option<(Ident, Option<Ident>)>, Option<Ident>)> {
        match self {
            ContractFunctionState::Untouched => {
                // We are the first time a contract is handled on this function, so
                // we're responsible for
                //
                // 1. Generating a name for the check function
                // 2. Emitting the original, unchanged item and register the check
                //    function on it via attribute
                // 3. Renaming our item to the new name
                // 4. And (minor point) adding #[allow(dead_code)] and
                //    #[allow(unused_variables)] to the check function attributes

                let check_fn_name =
                    identifier_for_generated_function(item_fn, "check", a_short_hash);
                let replace_fn_name =
                    identifier_for_generated_function(item_fn, "replace", a_short_hash);
                let mut dummy_fn_name =
                    identifier_for_generated_function(item_fn, "replace_dummy", a_short_hash);
                let mut recursion_wrapper_name =
                    identifier_for_generated_function(item_fn, "recursion_wrapper", a_short_hash);

                // Constructing string literals explicitly here, because if we call
                // `stringify!` in the generated code that is passed on as that
                // expression to the next expansion of a contract, not as the
                // literal.
                let replace_fn_name_str =
                    syn::LitStr::new(&replace_fn_name.to_string(), Span::call_site());
                let dummy_fn_name_str =
                    syn::LitStr::new(&dummy_fn_name.to_string(), Span::call_site());
                let recursion_wrapper_name_str =
                    syn::LitStr::new(&recursion_wrapper_name.to_string(), Span::call_site());

                // The order of `attrs` and `kanitool::{checked_with,
                // is_contract_generated}` is important here, because macros are
                // expanded outside in. This way other contract annotations in `attrs`
                // sees those attributes and can use them to determine
                // `function_state`.
                //
                // We're emitting the original here but the same applies later when we
                // emit the check function.
                let mut attrs = vec![];
                swapped!(&mut item_fn.attrs, &mut attrs, {
                    swapped!(&mut item_fn.sig.ident, &mut dummy_fn_name, {
                        let sig = &item_fn.sig;
                        output.extend(quote!(
                            #[allow(dead_code, unused_variables)]
                            #sig {
                                unreachable!()
                            }
                        ));
                    });
                    swapped!(&mut item_fn.sig.ident, &mut recursion_wrapper_name, {
                        let sig = &item_fn.sig;
                        let args = exprs_for_args(&sig.inputs);
                        let also_args = args.clone();
                        let (call_check, call_replace) = if is_probably_impl_fn(sig) {
                            (quote!(Self::#check_fn_name), quote!(Self::#replace_fn_name))
                        } else {
                            (quote!(#check_fn_name), quote!(#replace_fn_name))
                        };
                        // This doesn't deal with the case where the inner body
                        // panics. In that case the boolean does not get reset.
                        output.extend(quote!(
                            #[allow(dead_code, unused_variables)]
                            #[kanitool::is_contract_generated(recursion_wrapper)]
                            #sig {
                                static mut REENTRY: bool = false;
                                if unsafe { REENTRY } {
                                    #call_replace(#(#args),*)
                                } else {
                                    unsafe { REENTRY = true };
                                    let result = #call_check(#(#also_args),*);
                                    unsafe { REENTRY = false };
                                    result
                                }
                            }
                        ));
                    });
                    output.extend(quote!(
                        #(#attrs)*
                        #[kanitool::checked_with = #recursion_wrapper_name_str]
                        #[kanitool::replaced_with = #replace_fn_name_str]
                        #[kanitool::memory_havoc_dummy = #dummy_fn_name_str]
                        #item_fn
                    ));
                });
                Some((Some((replace_fn_name, Some(dummy_fn_name))), Some(check_fn_name)))
            }
            ContractFunctionState::Original | Self::ReplaceDummy => None,
            ContractFunctionState::Check => Some((None, Some(item_fn.sig.ident.clone()))),
            ContractFunctionState::Replace => Some((Some((item_fn.sig.ident.clone(), None)), None)),
        }
    }
}

/// A visitor which injects a copy of the token stream it holds before every
/// `return` expression.
///
/// This is intended to be used with postconditions and for that purpose it also
/// performs a rewrite where the return value is first bound to `result` so the
/// postconditions can access it.
///
/// # Example
///
/// The expression `return x;` turns into
///
/// ```rs
/// { // Always opens a new block
///     let result = x;
///     <injected tokenstream>
///     return result;
/// }
/// ```
struct PostconditionInjector(TokenStream2);

impl VisitMut for PostconditionInjector {
    /// We leave this emtpy to stop the recursion here. We don't want to look
    /// inside the closure, because the return statements contained within are
    /// for a different function, duh.
    fn visit_expr_closure_mut(&mut self, _: &mut syn::ExprClosure) {}

    fn visit_expr_mut(&mut self, i: &mut Expr) {
        if let syn::Expr::Return(r) = i {
            let tokens = self.0.clone();
            let mut output = TokenStream2::new();
            if let Some(expr) = &mut r.expr {
                // In theory the return expression can contain itself a `return`
                // so we need to recurse here.
                self.visit_expr_mut(expr);
                output.extend(quote!(let result = #expr;));
                *expr = Box::new(Expr::Verbatim(quote!(result)));
            }
            *i = syn::Expr::Verbatim(quote!({
                #output
                #tokens
                #i
            }))
        } else {
            syn::visit_mut::visit_expr_mut(self, i)
        }
    }
}

/// A supporting function for creating shallow, unsafe copies of the arguments
/// for the postconditions.
///
/// This function
/// - Collects all [`Ident`]s found in the argument patterns
/// - Creates new names for them
/// - Replaces all occurrences of those idents in `attrs` with the new names and
/// - Returns the mapping of old names to new names
fn rename_argument_occurrences(sig: &syn::Signature, attr: &mut Expr) -> HashMap<Ident, Ident> {
    let mut arg_ident_collector = ArgumentIdentCollector::new();
    arg_ident_collector.visit_signature(&sig);

    let mk_new_ident_for = |id: &Ident| Ident::new(&format!("{}_renamed", id), Span::mixed_site());
    let arg_idents = arg_ident_collector
        .0
        .into_iter()
        .map(|i| {
            let new = mk_new_ident_for(&i);
            (i, new)
        })
        .collect::<HashMap<_, _>>();

    let mut ident_rewriter = Renamer(&arg_idents);
    ident_rewriter.visit_expr_mut(attr);
    arg_idents
}

struct ContractConditionsHandler<'a> {
    condition_type: ContractConditionsType,
    attr: Expr,
    body: Block,
    /// An unparsed copy of the original attribute which is used for the
    /// messages in `assert`
    attr_copy: &'a TokenStream2,
}

enum ContractConditionsType {
    Requires,
    Ensures { old_vars: Vec<Ident>, old_exprs: Vec<Expr>, arg_idents: HashMap<Ident, Ident> },
}

impl ContractConditionsType {
    fn new_ensures(sig: &Signature, attr: &mut Expr) -> Self {
        let old_replacer = {
            let mut vis = OldVisitor::new(OldLifter::new());
            vis.visit_expr_mut(attr);
            vis.into_inner()
        };
        let (old_vars, old_exprs): (Vec<_>, Vec<_>) =
            old_replacer.into_iter_exprs_and_idents().unzip();

        let arg_idents = rename_argument_occurrences(sig, attr);

        ContractConditionsType::Ensures { old_vars, old_exprs, arg_idents }
    }
}

impl<'a> ContractConditionsHandler<'a> {
    fn new(
        is_requires: bool,
        mut attr: Expr,
        fn_sig: &Signature,
        fn_body: Block,
        attr_copy: &'a TokenStream2,
    ) -> Self {
        let condition_type = if is_requires {
            ContractConditionsType::Requires
        } else {
            ContractConditionsType::new_ensures(fn_sig, &mut attr)
        };

        Self { condition_type, attr, body: fn_body, attr_copy }
    }

    fn make_check_body(&self) -> TokenStream2 {
        let attr = &self.attr;
        let call_to_prior = &self.body;
        match &self.condition_type {
            ContractConditionsType::Requires => quote!(
                kani::assume(#attr);
                #call_to_prior
            ),
            ContractConditionsType::Ensures { old_vars, old_exprs, arg_idents } => {
                let arg_names = arg_idents.values();
                let arg_names_2 = arg_names.clone();
                let arg_idents = arg_idents.keys();
                let attr = &self.attr;
                let attr_copy = self.attr_copy;

                // The code that enforces the postconditions and cleans up the shallow
                // argument copies (with `mem::forget`).
                let exec_postconditions = quote!(
                    kani::assert(#attr, stringify!(#attr_copy));
                    #(std::mem::forget(#arg_names_2);)*
                );

                // We make a copy here because we'll modify it. Technically not
                // necessary but could lead to weird results if
                // `make_replace_body` were called after this if we modified in
                // place.
                let mut call = call_to_prior.clone();

                let mut inject_conditions = PostconditionInjector(exec_postconditions.clone());
                inject_conditions.visit_block_mut(&mut call);
                quote!(
                    #(let #old_vars = #old_exprs;)*
                    #(let #arg_names = kani::untracked_deref(&#arg_idents);)*
                    let result = #call;
                    #exec_postconditions
                    result
                )
            }
        }
    }

    fn make_replace_body(&self, sig: &Signature, use_dummy_fn_call: Option<Ident>) -> TokenStream2 {
        let attr = &self.attr;
        let attr_copy = self.attr_copy;
        let call_to_prior = if let Some(dummy) = use_dummy_fn_call {
            let arg_exprs = exprs_for_args(&sig.inputs);
            if is_probably_impl_fn(sig) {
                quote!(
                    Self::#dummy(#(#arg_exprs),*)
                )
            } else {
                quote!(
                    #dummy(#(#arg_exprs),*)
                )
            }
        } else {
            self.body.to_token_stream()
        };
        match &self.condition_type {
            ContractConditionsType::Requires => quote!(
                kani::assert(#attr, stringify!(#attr_copy));
                #call_to_prior
            ),
            ContractConditionsType::Ensures { old_vars, old_exprs, arg_idents } => {
                let arg_names = arg_idents.values();
                let arg_values = arg_idents.keys();
                quote!(
                    #(let #old_vars = #old_exprs;)*
                    #(let #arg_names = kani::untracked_deref(&#arg_values);)*
                    let result = #call_to_prior;
                    kani::assume(#attr);
                    result
                )
            }
        }
    }
}

fn is_probably_impl_fn(sig: &Signature) -> bool {
    let mut self_detector = SelfDetector(false);
    self_detector.visit_signature(sig);
    self_detector.0
}

fn exprs_for_args<'a, T>(
    args: &'a Punctuated<FnArg, T>,
) -> impl Iterator<Item = Expr> + Clone + 'a {
    args.iter().map(|arg| match arg {
        FnArg::Receiver(_) => Expr::Verbatim(quote!(self)),
        FnArg::Typed(typed) => pat_to_expr(&typed.pat),
    })
}

struct SelfDetector(bool);

impl<'ast> Visit<'ast> for SelfDetector {
    fn visit_path(&mut self, i: &'ast syn::Path) {
        self.0 |= i.get_ident().map_or(false, |i| i == "self")
            || i.get_ident().map_or(false, |i| i == "Self")
    }
}

fn pat_to_expr(pat: &Pat) -> Expr {
    let mk_err = |typ| {
        pat.span()
            .unwrap()
            .error(format!("`{typ}` patterns are not supported for functions with contracts"))
            .emit();
        unreachable!()
    };
    match pat {
        Pat::Const(c) => Expr::Const(c.clone()),
        Pat::Ident(id) => Expr::Verbatim(id.ident.to_token_stream()),
        Pat::Lit(lit) => Expr::Lit(lit.clone()),
        Pat::Reference(rf) => Expr::Reference(ExprReference {
            attrs: vec![],
            and_token: rf.and_token,
            mutability: rf.mutability,
            expr: Box::new(pat_to_expr(&rf.pat)),
        }),
        Pat::Tuple(tup) => Expr::Tuple(ExprTuple {
            attrs: vec![],
            paren_token: tup.paren_token,
            elems: tup.elems.iter().map(pat_to_expr).collect(),
        }),
        Pat::Slice(slice) => Expr::Reference(ExprReference {
            attrs: vec![],
            and_token: Token!(&)(Span::call_site()),
            mutability: None,
            expr: Box::new(Expr::Array(ExprArray {
                attrs: vec![],
                bracket_token: slice.bracket_token,
                elems: slice.elems.iter().map(pat_to_expr).collect(),
            })),
        }),
        Pat::Path(pth) => Expr::Path(pth.clone()),
        Pat::Or(_) => mk_err("or"),
        Pat::Rest(_) => mk_err("rest"),
        Pat::Wild(_) => mk_err("wildcard"),
        Pat::Paren(inner) => pat_to_expr(&inner.pat),
        Pat::Range(_) => mk_err("range"),
        Pat::Struct(strct) => {
            if strct.rest.is_some() {
                mk_err("..");
            }
            Expr::Struct(ExprStruct {
                attrs: vec![],
                path: strct.path.clone(),
                brace_token: strct.brace_token,
                dot2_token: None,
                rest: None,
                qself: strct.qself.clone(),
                fields: strct
                    .fields
                    .iter()
                    .map(|field_pat| FieldValue {
                        attrs: vec![],
                        member: field_pat.member.clone(),
                        colon_token: field_pat.colon_token,
                        expr: pat_to_expr(&field_pat.pat),
                    })
                    .collect(),
            })
        }
        Pat::Verbatim(_) => mk_err("verbatim"),
        Pat::Type(_) => mk_err("type"),
        Pat::TupleStruct(_) => mk_err("tuple struct"),
        _ => mk_err("unknown"),
    }
}

/// The main meat of handling requires/ensures contracts.
///
/// Generates a `check_<fn_name>_<fn_hash>` function that assumes preconditions
/// and asserts postconditions. The check function is also marked as generated
/// with the `#[kanitool::is_contract_generated(check)]` attribute.
///
/// Decorates the original function with `#[kanitool::checked_by =
/// "check_<fn_name>_<fn_hash>"]
///
/// The check function is a copy of the original function with preconditions
/// added before the body and postconditions after as well as injected before
/// every `return` (see [`PostconditionInjector`]). Attributes on the original
/// function are also copied to the check function. Each clause (requires or
/// ensures) after the first will be ignored on the original function (detected
/// by finding the `kanitool::checked_with` attribute). On the check function
/// (detected by finding the `kanitool::is_contract_generated` attribute) it
/// expands into a new layer of pre- or postconditions. This state machine is
/// also explained in more detail in comments in the body of this macro.
///
/// In the check function all named arguments of the function are unsafely
/// shallow-copied with the `kani::untracked_deref` function to circumvent the
/// borrow checker for postconditions. We must ensure that those copies are not
/// dropped so after the postconditions we call `mem::forget` on each copy.
///
/// # Complete example
///
/// ```rs
/// #[kani::requires(divisor != 0)]
/// #[kani::ensures(result <= dividend)]
/// fn div(dividend: u32, divisor: u32) -> u32 {
///     dividend / divisor
/// }
/// ```
///
/// Turns into
///
/// ```rs
/// #[kanitool::checked_with = "div_check_965916"]
/// fn div(dividend: u32, divisor: u32) -> u32 { dividend / divisor }
///
/// #[allow(dead_code)]
/// #[allow(unused_variables)]
/// #[kanitool::is_contract_generated(check)]
/// fn div_check_965916(dividend: u32, divisor: u32) -> u32 {
///     let dividend_renamed = kani::untracked_deref(&dividend);
///     let divisor_renamed = kani::untracked_deref(&divisor);
///     let result = { kani::assume(divisor != 0); { dividend / divisor } };
///     kani::assert(result <= dividend_renamed, "result <= dividend");
///     std::mem::forget(dividend_renamed);
///     std::mem::forget(divisor_renamed);
///     result
/// }
/// ```
fn requires_ensures_alt(attr: TokenStream, item: TokenStream, is_requires: bool) -> TokenStream {
    let attr_copy = proc_macro2::TokenStream::from(attr.clone());
    let attr = parse_macro_input!(attr as Expr);

    let mut output = proc_macro2::TokenStream::new();

    let a_short_hash = short_hash_of_token_stream(&item);
    let mut item_fn = parse_macro_input!(item as ItemFn);

    // If we didn't find any other contract handling related attributes we
    // assume this function has not been touched by a contract before.
    let function_state = item_fn
        .attrs
        .iter()
        .find_map(ContractFunctionState::from_attribute)
        .unwrap_or(ContractFunctionState::Untouched);

    let Some((emit_replace, emit_check)) =
        function_state.prepare_header(&mut item_fn, &mut output, a_short_hash)
    else {
        // If we're the original function that means we're *not* the first time
        // that a contract attribute is handled on this function. This means
        // there must exist a generated check function somewhere onto which the
        // attributes have been copied and where they will be expanded into more
        // checks. So we just return outselves unchanged.
        return item_fn.into_token_stream().into();
    };

    let ItemFn { attrs, vis: _, mut sig, block } = item_fn;

    let handler = ContractConditionsHandler::new(is_requires, attr, &sig, *block, &attr_copy);

    let emit_common_header = |output: &mut TokenStream2| {
        if function_state.emit_tag_attr() {
            output.extend(quote!(
                #[allow(dead_code, unused_variables)]
            ));
        }
        output.extend(attrs.iter().flat_map(Attribute::to_token_stream));
    };

    if let Some((replace_name, dummy)) = emit_replace {
        emit_common_header(&mut output);

        if function_state.emit_tag_attr() {
            // If it's the first time we also emit this marker. Again, order is
            // important so this happens as the last emitted attribute.
            output.extend(quote!(#[kanitool::is_contract_generated(replace)]));
        }

        let body = handler.make_replace_body(&sig, dummy);

        sig.ident = replace_name;

        // Finally emit the check function itself.
        output.extend(quote!(
            #sig {
                #body
            }
        ));
    }

    if let Some(check_name) = emit_check {
        emit_common_header(&mut output);

        if function_state.emit_tag_attr() {
            // If it's the first time we also emit this marker. Again, order is
            // important so this happens as the last emitted attribute.
            output.extend(quote!(#[kanitool::is_contract_generated(check)]));
        }
        let body = handler.make_check_body();
        sig.ident = check_name;
        output.extend(quote!(
            #sig {
                #body
            }
        ))
    }

    output.into()
}

/// This is very similar to the kani_attribute macro, but it instead creates
/// key-value style attributes which I find a little easier to parse.
macro_rules! passthrough {
    ($name:ident, $allow_dead_code:ident) => {
        pub fn $name(attr: TokenStream, item: TokenStream) -> TokenStream {
            let args = proc_macro2::TokenStream::from(attr);
            let fn_item = proc_macro2::TokenStream::from(item);
            let name = Ident::new(stringify!($name), proc_macro2::Span::call_site());
            let extra_attrs = if $allow_dead_code {
                quote!(#[allow(dead_code)])
            } else {
                quote!()
            };
            quote!(
                #extra_attrs
                #[kanitool::#name = stringify!(#args)]
                #fn_item
            )
            .into()
        }
    }
}

passthrough!(proof_for_contract, true);
passthrough!(stub_verified, false);