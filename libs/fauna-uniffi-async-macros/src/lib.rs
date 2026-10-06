//! `#[fauna_uniffi_async::export]` — see the `fauna-uniffi-async` crate docs
//! for what it does and why. This crate is only the expansion.

use proc_macro::TokenStream;
use proc_macro2::{TokenStream as TokenStream2, TokenTree};
use quote::{format_ident, quote, quote_spanned};
use syn::spanned::Spanned;
use syn::{
    Attribute, FnArg, GenericArgument, ImplItem, ImplItemFn, Item, ItemFn, ItemImpl, Pat,
    PathArguments, ReturnType, Signature, Type, TypeReference,
};

/// Replaces `#[uniffi::export(async_runtime = "tokio")]` on an inherent
/// `impl` block or a free function; every `async fn` is exported through a
/// generated twin that runs it off the foreign poll stack. On a free function
/// any arguments (`default(x = None)`) pass through to `uniffi::export`.
#[proc_macro_attribute]
pub fn export(attr: TokenStream, item: TokenStream) -> TokenStream {
    let extra = TokenStream2::from(attr);
    let expanded = match syn::parse_macro_input!(item as Item) {
        Item::Impl(_) if !extra.is_empty() => Err(syn::Error::new(
            extra.span(),
            "`fauna_uniffi_async::export` on an `impl` block takes no arguments \
             (put `default(..)` on the method's `#[uniffi::method]`)",
        )),
        Item::Impl(item) => expand_impl(item),
        Item::Fn(item) => expand_fn(item, extra),
        other => Err(syn::Error::new(
            other.span(),
            "`fauna_uniffi_async::export` applies to an inherent `impl` block or a free function",
        )),
    };
    expanded.unwrap_or_else(|e| e.to_compile_error()).into()
}

fn expand_impl(item: ItemImpl) -> syn::Result<TokenStream2> {
    if let Some((_, path, _)) = &item.trait_ {
        return Err(syn::Error::new(
            path.span(),
            "`fauna_uniffi_async::export` does not apply to a trait impl",
        ));
    }
    let ItemImpl {
        attrs,
        generics,
        self_ty,
        items,
        ..
    } = item;
    let (impl_generics, _, where_clause) = generics.split_for_impl();

    let mut inline = Vec::new();
    let mut exported = Vec::new();
    for item in items {
        match item {
            ImplItem::Fn(f) if f.sig.asyncness.is_some() => {
                exported.push(method_twin(&f)?);
                inline.push(strip_uniffi_attrs_fn(f));
            }
            other => exported.push(quote!(#other)),
        }
    }

    Ok(quote! {
        #(#attrs)*
        impl #impl_generics #self_ty #where_clause {
            #(#inline)*
        }

        #(#attrs)*
        #[::uniffi::export(async_runtime = "tokio")]
        impl #impl_generics #self_ty #where_clause {
            #(#exported)*
        }
    })
}

fn expand_fn(mut item: ItemFn, extra: TokenStream2) -> syn::Result<TokenStream2> {
    if item.sig.asyncness.is_none() {
        return Ok(quote! {
            #[::uniffi::export(#extra)]
            #item
        });
    }
    let extra = if extra.is_empty() {
        extra
    } else {
        quote!(, #extra)
    };
    item.attrs.retain(|a| !is_uniffi_attr(a));
    let ItemFn {
        attrs, vis, sig, ..
    } = &item;
    let name = &sig.ident;
    let foreign_name = foreign_name(name);
    let twin = twin_ident(name);
    let (params, call_args) = owned_params(sig)?;
    let output = &sig.output;
    let docs = passthrough_attrs(attrs);
    Ok(quote! {
        #item

        #(#docs)*
        #[::uniffi::export(async_runtime = "tokio", name = #foreign_name #extra)]
        #vis async fn #twin(#(#params),*) #output {
            ::fauna_uniffi_async::off_foreign_stack(async move {
                #name(#(#call_args),*).await
            })
            .await
        }
    })
}

/// The exported twin of an `async fn` inside an `impl` block.
fn method_twin(f: &ImplItemFn) -> syn::Result<TokenStream2> {
    let sig = &f.sig;
    let name = &sig.ident;
    let twin = twin_ident(name);
    let vis = &f.vis;
    let docs = passthrough_attrs(&f.attrs);
    let (params, call_args) = owned_params(sig)?;
    let output: &ReturnType = &sig.output;
    let has_receiver = sig.receiver().is_some();
    let uniffi_attr = twin_uniffi_attr(f, has_receiver)?;

    let (receiver, call) = if has_receiver {
        (
            quote!(self: ::std::sync::Arc<Self>,),
            quote!(self.#name(#(#call_args),*).await),
        )
    } else {
        (quote!(), quote!(Self::#name(#(#call_args),*).await))
    };

    Ok(quote! {
        #(#docs)*
        #uniffi_attr
        #vis async fn #twin(#receiver #(#params),*) #output {
            ::fauna_uniffi_async::off_foreign_stack(async move { #call }).await
        }
    })
}

/// The twin carries the original's `uniffi::method` / `uniffi::constructor`
/// arguments (defaults, an explicit `name`), with `name` set to the original
/// Rust name when the original did not rename it.
fn twin_uniffi_attr(f: &ImplItemFn, has_receiver: bool) -> syn::Result<TokenStream2> {
    let foreign_name = foreign_name(&f.sig.ident);
    let existing = f.attrs.iter().find(|a| is_uniffi_attr(a));
    let (kind, args) = match existing {
        Some(attr) => {
            let kind = attr
                .path()
                .segments
                .last()
                .map(|s| s.ident.clone())
                .expect("a uniffi attribute has a path");
            let args = match &attr.meta {
                syn::Meta::List(list) => list.tokens.clone(),
                syn::Meta::Path(_) => TokenStream2::new(),
                syn::Meta::NameValue(nv) => {
                    return Err(syn::Error::new(
                        nv.span(),
                        "unexpected uniffi attribute form",
                    ));
                }
            };
            (kind, args)
        }
        None if has_receiver => (format_ident!("method"), TokenStream2::new()),
        None => {
            return Err(syn::Error::new(
                f.sig.span(),
                "an associated `async fn` without a receiver must say `#[uniffi::constructor]`",
            ));
        }
    };
    let args = if names_itself(&args) {
        args
    } else if args.is_empty() {
        quote!(name = #foreign_name)
    } else {
        quote!(name = #foreign_name, #args)
    };
    Ok(quote!(#[::uniffi::#kind(#args)]))
}

/// Whether a uniffi attribute's argument list already has a top-level `name = …`.
fn names_itself(args: &TokenStream2) -> bool {
    let tokens: Vec<TokenTree> = args.clone().into_iter().collect();
    tokens.windows(2).any(|w| {
        matches!((&w[0], &w[1]),
            (TokenTree::Ident(i), TokenTree::Punct(p)) if i == "name" && p.as_char() == '=')
    })
}

/// The twin's parameters (owned forms, original names) and the expressions the
/// twin passes to the inline fn.
fn owned_params(sig: &Signature) -> syn::Result<(Vec<TokenStream2>, Vec<TokenStream2>)> {
    let mut params = Vec::new();
    let mut call_args = Vec::new();
    for arg in &sig.inputs {
        let FnArg::Typed(pt) = arg else { continue };
        let ident = match &*pt.pat {
            Pat::Ident(p) => p.ident.clone(),
            other => {
                return Err(syn::Error::new(
                    other.span(),
                    "an exported argument must be a plain name (UniFFI names the foreign parameter after it)",
                ));
            }
        };
        let (ty, pass) = owned_form(&pt.ty, &ident);
        let span = pt.span();
        params.push(quote_spanned!(span=> #ident: #ty));
        call_args.push(pass);
    }
    Ok((params, call_args))
}

/// A borrowed `&T` becomes the owned form UniFFI itself lifts it through —
/// `<T as LiftRef>::LiftType` (`String` for `str`, `Arc<T>` for an object,
/// `T` for a record) — lent back to the inline fn by `Borrow`; likewise inside
/// an `Option`. Anything else is passed unchanged.
fn owned_form(ty: &Type, ident: &syn::Ident) -> (TokenStream2, TokenStream2) {
    if let Type::Reference(r) = ty {
        let elem = &r.elem;
        return (
            lift_type(r),
            quote!(::std::borrow::Borrow::<#elem>::borrow(&#ident)),
        );
    }
    if let Some(r) = option_ref(ty) {
        let elem = &r.elem;
        let owned = lift_type(r);
        return (
            quote!(::core::option::Option<#owned>),
            quote!(#ident.as_ref().map(::std::borrow::Borrow::<#elem>::borrow)),
        );
    }
    (quote!(#ty), quote!(#ident))
}

fn lift_type(r: &TypeReference) -> TokenStream2 {
    let elem = &r.elem;
    quote!(<#elem as ::uniffi::LiftRef<crate::UniFfiTag>>::LiftType)
}

fn option_ref(ty: &Type) -> Option<&TypeReference> {
    let Type::Path(p) = ty else { return None };
    let last = p.path.segments.last()?;
    if last.ident != "Option" {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &last.arguments else {
        return None;
    };
    match args.args.first()? {
        GenericArgument::Type(Type::Reference(r)) => Some(r),
        _ => None,
    }
}

/// Doc comments, `cfg`s and lint levels follow the fn onto its twin: the doc
/// is what UniFFI renders into the foreign docstring, the `cfg` must gate both
/// halves alike, and the twin repeats the signature a lint allowance was about
/// (`clippy::too_many_arguments`).
fn passthrough_attrs(attrs: &[Attribute]) -> Vec<&Attribute> {
    const PASSED: [&str; 4] = ["doc", "cfg", "allow", "deprecated"];
    attrs
        .iter()
        .filter(|a| PASSED.iter().any(|name| a.path().is_ident(name)))
        .collect()
}

fn strip_uniffi_attrs_fn(mut f: ImplItemFn) -> TokenStream2 {
    f.attrs.retain(|a| !is_uniffi_attr(a));
    quote!(#f)
}

fn is_uniffi_attr(attr: &Attribute) -> bool {
    attr.path()
        .segments
        .first()
        .is_some_and(|s| s.ident == "uniffi")
}

fn foreign_name(ident: &syn::Ident) -> String {
    let s = ident.to_string();
    s.strip_prefix("r#").map(str::to_owned).unwrap_or(s)
}

fn twin_ident(ident: &syn::Ident) -> syn::Ident {
    format_ident!("__uniffi_async_{}", foreign_name(ident))
}
