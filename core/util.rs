use heck::ToKebabCase;
use proc_macro2::{Span, TokenStream};
use quote::quote;
use std::cell::RefCell;
use syn::parse::{Parse, ParseStream, Parser};

#[derive(Default)]
pub struct Errors {
    errors: RefCell<Vec<darling::Error>>,
}

impl Errors {
    #[inline]
    pub fn new() -> Self {
        Default::default()
    }
    #[inline]
    pub fn push<T: std::fmt::Display>(&self, span: Span, message: T) {
        self.push_syn(syn::Error::new(span, message));
    }
    #[inline]
    pub fn push_spanned<T, U>(&self, tokens: T, message: U)
    where
        T: quote::ToTokens,
        U: std::fmt::Display,
    {
        self.push_syn(syn::Error::new_spanned(tokens, message));
    }
    #[inline]
    pub fn push_syn(&self, error: syn::Error) {
        self.push_darling(error.into());
    }
    #[inline]
    pub fn push_darling(&self, error: darling::Error) {
        self.errors.borrow_mut().push(error);
    }
    pub fn into_compile_errors(self) -> Option<TokenStream> {
        let errors = self.errors.take();
        (!errors.is_empty()).then(|| darling::Error::multiple(errors).write_errors())
    }
}

pub fn parse<T: Parse>(input: TokenStream, errors: &Errors) -> Option<T> {
    match <T as Parse>::parse.parse2(input) {
        Ok(t) => Some(t),
        Err(e) => {
            errors.push_syn(e);
            None
        }
    }
}

#[derive(Default)]
struct AttributeArgs(syn::AttributeArgs);

impl Parse for AttributeArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut metas = Vec::new();

        loop {
            if input.is_empty() {
                break;
            }
            let value = input.parse()?;
            metas.push(value);
            if input.is_empty() {
                break;
            }
            input.parse::<syn::Token![,]>()?;
        }

        Ok(Self(metas))
    }
}

pub fn parse_list<T>(input: TokenStream, errors: &Errors) -> T
where
    T: darling::FromMeta + Default,
{
    let args = parse::<AttributeArgs>(input, errors).unwrap_or_default().0;
    match T::from_list(&args) {
        Ok(args) => args,
        Err(e) => {
            errors.push_darling(e);
            Default::default()
        }
    }
}

#[derive(Default)]
struct ParenAttributeArgs(syn::AttributeArgs);

impl Parse for ParenAttributeArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let args = if input.peek(syn::token::Paren) {
            let content;
            syn::parenthesized!(content in input);
            content.parse::<AttributeArgs>()?.0
        } else {
            Default::default()
        };
        input.parse::<syn::parse::Nothing>()?;
        Ok(Self(args))
    }
}

pub fn parse_paren_list<T>(input: TokenStream, errors: &Errors) -> T
where
    T: darling::FromMeta + Default,
{
    let args = parse::<ParenAttributeArgs>(input, errors)
        .unwrap_or_default()
        .0;
    match T::from_list(&args) {
        Ok(args) => args,
        Err(e) => {
            errors.push_darling(e);
            Default::default()
        }
    }
}

pub(crate) fn format_name(ident: &syn::Ident) -> String {
    let ident = ident.to_string();
    let mut s = ident.as_str();
    while let Some(n) = s.strip_prefix('_') {
        s = n;
    }
    s.to_kebab_case()
}

pub(crate) fn is_valid_name(name: &str) -> bool {
    let mut iter = name.chars();
    if let Some(c) = iter.next() {
        if !c.is_ascii_alphabetic() {
            return false;
        }
        for c in iter {
            if !c.is_ascii_alphanumeric() && c != '-' && c != '_' {
                return false;
            }
        }
        true
    } else {
        false
    }
}

pub(crate) fn arg_reference(arg: &syn::FnArg) -> Option<TokenStream> {
    match arg {
        syn::FnArg::Receiver(syn::Receiver {
            reference,
            mutability,
            ..
        }) => {
            let (and, lifetime) = reference.as_ref()?;
            Some(quote! { #and #lifetime #mutability })
        }
        syn::FnArg::Typed(pat) => match &*pat.ty {
            syn::Type::Reference(syn::TypeReference {
                and_token,
                lifetime,
                mutability,
                ..
            }) => Some(quote! { #and_token #lifetime #mutability }),
            _ => None,
        },
    }
}

#[inline]
pub(crate) fn signature_args(sig: &syn::Signature) -> impl Iterator<Item = &syn::Ident> + Clone {
    sig.inputs.iter().filter_map(arg_name)
}

#[inline]
pub(crate) fn arg_name(arg: &syn::FnArg) -> Option<&syn::Ident> {
    if let syn::FnArg::Typed(syn::PatType { pat, .. }) = arg {
        if let syn::Pat::Ident(syn::PatIdent { ident, .. }) = pat.as_ref() {
            return Some(ident);
        }
    }
    None
}
