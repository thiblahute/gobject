use crate::{
    property::{Properties, Property},
    public_method::PublicMethod,
    signal::Signal,
    util::{self, Errors},
    virtual_method::VirtualMethod,
};
use heck::ToKebabCase;
use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote, quote_spanned, ToTokens};
use std::{cell::RefCell, collections::HashMap};
use syn::{parse_quote, spanned::Spanned};

#[derive(Debug)]
pub struct TypeDefinition {
    pub module: syn::ItemMod,
    pub base: TypeBase,
    pub vis: syn::Visibility,
    pub inner_vis: syn::Visibility,
    pub concurrency: Concurrency,
    pub name: Option<syn::Ident>,
    pub crate_ident: syn::Ident,
    pub generics: Option<syn::Generics>,
    pub properties_item_index: Option<usize>,
    pub methods_item_index: Option<usize>,
    pub properties: Vec<Property>,
    pub signals: Vec<Signal>,
    pub public_methods: Vec<PublicMethod>,
    pub virtual_methods: Vec<VirtualMethod>,
    pub wrapper_methods: Vec<syn::Signature>,
    custom_stmts: RefCell<HashMap<String, Vec<syn::Stmt>>>,
}

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Copy, Clone)]
pub enum TypeMode {
    Subclass,
    Wrapper,
}

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Copy, Clone)]
pub enum TypeContext {
    Internal,
    External,
}

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Copy, Clone)]
pub enum TypeBase {
    Class,
    Interface,
}

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Copy, Clone)]
pub enum Concurrency {
    None,
    SendSync,
}

impl ToTokens for Concurrency {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        match self {
            Self::None => {}
            Self::SendSync => (quote! { + Send + Sync }).to_tokens(tokens),
        }
    }
}

macro_rules! unwrap_or_return {
    ($opt:expr, $ret:expr) => {
        match $opt {
            Some(val) => val,
            None => return $ret,
        }
    };
}

#[inline]
fn type_ident(ty: &syn::Type) -> Option<&syn::Ident> {
    if let syn::Type::Path(syn::TypePath { path, .. }) = ty {
        if path.leading_colon.is_none() && path.segments.len() == 1 {
            return Some(&path.segments[0].ident);
        }
    }
    None
}

#[inline]
fn extract_attr(attrs: &mut Vec<syn::Attribute>, name: &str) -> Option<syn::Attribute> {
    let attr_index = attrs.iter().position(|a| a.path.is_ident(name));
    attr_index.map(|attr_index| attrs.remove(attr_index))
}

#[inline]
fn is_marker(path: &syn::Path, marker: &str) -> bool {
    if path.is_ident(marker) {
        return true;
    }
    let path = path
        .to_token_stream()
        .into_iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join("");
    path == format!("std::marker::{}", marker)
        || path == format!("::std::marker::{}", marker)
        || path == format!("core::marker::{}", marker)
        || path == format!("::core::marker::{}", marker)
}

impl TypeDefinition {
    pub fn parse(
        module: syn::ItemMod,
        base: TypeBase,
        name: Option<syn::Ident>,
        crate_ident: syn::Ident,
        errors: &Errors,
    ) -> Self {
        let mut item = syn::Item::Mod(module);
        super::closures(&mut item, crate_ident.clone(), errors);
        let module = match item {
            syn::Item::Mod(m) => m,
            _ => unreachable!(),
        };
        let mut def = Self {
            module,
            base,
            vis: syn::Visibility::Inherited,
            inner_vis: parse_quote! { pub(super) },
            concurrency: Concurrency::None,
            name,
            crate_ident,
            generics: None,
            properties_item_index: None,
            methods_item_index: None,
            properties: Vec::new(),
            signals: Vec::new(),
            public_methods: Vec::new(),
            virtual_methods: Vec::new(),
            wrapper_methods: Vec::new(),
            custom_stmts: RefCell::new(HashMap::new()),
        };
        if def.module.content.is_none() {
            errors.push_spanned(
                &def.module,
                "Module must have a body to use the class macro",
            );
            return def;
        }
        let glib = def.glib();
        let (_, items) = def.module.content.as_mut().unwrap();
        let mut struct_ = None;
        let mut impl_ = None;
        if let Some(name) = &def.name {
            for (index, item) in items.iter_mut().enumerate() {
                match item {
                    syn::Item::Struct(s) if struct_.is_none() && &s.ident == name => {
                        let attr = extract_attr(&mut s.attrs, "properties")
                            .unwrap_or_else(|| parse_quote! { #[properties] });
                        struct_ = Some((index, attr));
                    }
                    syn::Item::Impl(i)
                        if impl_.is_none()
                            && i.trait_.is_none()
                            && type_ident(&*i.self_ty) == Some(name) =>
                    {
                        extract_attr(&mut i.attrs, "methods");
                        impl_ = Some(index);
                    }
                    _ => {}
                }
                if struct_.is_some() && impl_.is_some() {
                    break;
                }
            }
        } else {
            {
                let mut first_struct = None;
                let mut struct_name = None;
                for (index, item) in items.iter_mut().enumerate() {
                    if let syn::Item::Struct(s) = item {
                        if let Some(attr) = extract_attr(&mut s.attrs, "properties") {
                            struct_ = Some((index, attr));
                            struct_name = Some(s.ident.clone());
                            break;
                        } else if first_struct.is_none() {
                            first_struct = Some(index);
                            struct_name = Some(s.ident.clone());
                        }
                    }
                }
                if struct_.is_none() {
                    if let Some(index) = first_struct {
                        struct_ = Some((index, parse_quote! { #[properties] }));
                    }
                }
                if struct_.is_some() {
                    def.name = struct_name;
                }
            }
            if let Some(name) = &def.name {
                for (index, item) in items.iter_mut().enumerate() {
                    if let syn::Item::Impl(i) = item {
                        if impl_.is_none()
                            && i.trait_.is_none()
                            && type_ident(&*i.self_ty) == Some(name)
                        {
                            extract_attr(&mut i.attrs, "methods");
                            impl_ = Some(index);
                        }
                    }
                }
            } else {
                let mut first_impl = None;
                let mut impl_name = None;
                for (index, item) in items.iter_mut().enumerate() {
                    if let syn::Item::Impl(i) = item {
                        if impl_.is_none() && i.trait_.is_none() {
                            if extract_attr(&mut i.attrs, "methods").is_some() {
                                impl_ = Some(index);
                                impl_name = type_ident(&*i.self_ty).cloned();
                                break;
                            } else if first_impl.is_none() {
                                first_impl = Some(index);
                                impl_name = type_ident(&*i.self_ty).cloned();
                            }
                        }
                    }
                }
                if impl_.is_none() {
                    if let Some(index) = first_impl {
                        impl_ = Some(index);
                    }
                }
                if impl_.is_some() {
                    def.name = impl_name;
                }
            }
        }
        if let Some((index, attr)) = struct_ {
            def.properties_item_index = Some(index);
            let struct_ = match &mut items[index] {
                syn::Item::Struct(s) => s,
                _ => unreachable!(),
            };
            match &struct_.vis {
                syn::Visibility::Inherited => {
                    struct_.vis = def.inner_vis.clone();
                }
                syn::Visibility::Restricted(syn::VisRestricted { path, .. })
                    if path.segments.len() == 1 && path.segments[0].ident == "self" =>
                {
                    struct_.vis = def.inner_vis.clone();
                }
                syn::Visibility::Restricted(syn::VisRestricted { path, .. })
                    if path.segments.len() > 1 && path.segments[0].ident == "super" =>
                {
                    let path = syn::Path {
                        leading_colon: None,
                        segments: FromIterator::from_iter(path.segments.iter().skip(1).cloned()),
                    };
                    def.vis = parse_quote! { pub(in #path) };
                    def.inner_vis = struct_.vis.clone();
                }
                _ => {
                    def.vis = struct_.vis.clone();
                    def.inner_vis = struct_.vis.clone();
                }
            };
            def.generics = Some(struct_.generics.clone());
            def.name = Some(struct_.ident.clone());
            let mut input: syn::DeriveInput = struct_.clone().into();
            input.attrs.insert(0, attr);
            let Properties {
                properties,
                mut fields,
                ..
            } = Properties::from_derive_input(&input, Some(base), errors);
            if base == TypeBase::Interface {
                match &mut fields {
                    syn::Fields::Named(f) => {
                        let fields: syn::FieldsNamed = parse_quote! {
                            { ____parent: #glib::gobject_ffi::GTypeInterface }
                        };
                        f.named.insert(0, fields.named.into_iter().next().unwrap());
                    }
                    syn::Fields::Unnamed(f) => {
                        let fields: syn::FieldsUnnamed = parse_quote! {
                            (#glib::gobject_ffi::GTypeInterface)
                        };
                        f.unnamed
                            .insert(0, fields.unnamed.into_iter().next().unwrap());
                    }
                    _ => {}
                };
            }
            struct_.fields = fields;
            def.properties.extend(properties);
            if def.base == TypeBase::Class {
                let struct_name = struct_.ident.clone();
                let mut send = false;
                let mut sync = false;
                for item in items.iter() {
                    if let syn::Item::Impl(i) = item {
                        if type_ident(&*i.self_ty) == Some(&struct_name) {
                            if let Some((_, path, _)) = &i.trait_ {
                                if is_marker(path, "Send") {
                                    send = true;
                                } else if is_marker(path, "Sync") {
                                    sync = true;
                                }
                            }
                        }
                    }
                }
                if send && sync {
                    def.concurrency = Concurrency::SendSync;
                }
            }
        } else {
            def.vis = def.module.vis.clone();
            match &def.vis {
                syn::Visibility::Inherited => {}
                syn::Visibility::Restricted(syn::VisRestricted { path, .. })
                    if path.segments.len() == 1 && path.segments[0].ident == "self" => {}
                syn::Visibility::Restricted(syn::VisRestricted { path, .. })
                    if !path.segments.is_empty() && path.segments[0].ident == "super" =>
                {
                    def.inner_vis = parse_quote! { pub(in super::#path) }
                }
                _ => def.inner_vis = def.vis.clone(),
            }
        }
        if let Some(index) = impl_ {
            def.methods_item_index = Some(index);
            let impl_ = match &mut items[index] {
                syn::Item::Impl(i) => i,
                _ => unreachable!(),
            };
            if def.generics.is_none() {
                def.generics = Some(impl_.generics.clone());
            }
            def.signals
                .extend(Signal::many_from_items(&mut impl_.items, base, errors));
            def.public_methods.extend(PublicMethod::many_from_items(
                &mut impl_.items,
                base,
                errors,
            ));
            def.virtual_methods.extend(VirtualMethod::many_from_items(
                &mut impl_.items,
                base,
                errors,
            ));
        }
        for item in items.iter_mut() {
            if let syn::Item::Impl(i) = item {
                if i.trait_.is_none() && extract_attr(&mut i.attrs, "wrapper_methods").is_some() {
                    Self::extract_wrapper_methods(
                        i,
                        &mut def.wrapper_methods,
                        def.base,
                        &glib,
                        errors,
                    );
                }
            }
        }
        def
    }
    pub fn glib(&self) -> TokenStream {
        let go = &self.crate_ident;
        quote! { #go::glib }
    }
    pub fn type_(&self, from: TypeMode, to: TypeMode, ctx: TypeContext) -> Option<TokenStream> {
        use TypeBase::*;
        use TypeContext::*;
        use TypeMode::*;

        let name = self.name.as_ref()?;
        let glib = self.glib();
        let generics = self.generics.as_ref();

        let recv = match ctx {
            Internal => quote! { Self },
            External => quote! { #name #generics },
        };

        match (from, to, self.base) {
            (Subclass, Subclass, _) | (Wrapper, Wrapper, _) => Some(recv),
            (Subclass, Wrapper, Class) => Some(quote! {
                <#recv as #glib::subclass::types::ObjectSubclass>::Type
            }),
            (Subclass, Wrapper, Interface) => Some(quote! {
                super::#name #generics
            }),
            (Wrapper, Subclass, Class) => Some(quote! {
                <#recv as #glib::object::ObjectSubclassIs>::Subclass
            }),
            (Wrapper, Subclass, Interface) => Some(quote! {
                <#recv as #glib::object::ObjectType>::GlibClassType
            }),
        }
    }
    pub fn properties_item(&self) -> Option<&syn::ItemStruct> {
        let index = self.properties_item_index?;
        match self.module.content.as_ref()?.1.get(index)? {
            syn::Item::Struct(s) => Some(s),
            _ => None,
        }
    }
    pub fn properties_item_mut(&mut self) -> Option<&mut syn::ItemStruct> {
        let index = self.properties_item_index?;
        match self.module.content.as_mut()?.1.get_mut(index)? {
            syn::Item::Struct(s) => Some(s),
            _ => None,
        }
    }
    pub fn methods_item(&self) -> Option<&syn::ItemImpl> {
        let index = self.methods_item_index?;
        match self.module.content.as_ref()?.1.get(index)? {
            syn::Item::Impl(i) => Some(i),
            _ => None,
        }
    }
    pub fn methods_item_mut(&mut self) -> Option<&mut syn::ItemImpl> {
        let index = self.methods_item_index?;
        match self.module.content.as_mut()?.1.get_mut(index)? {
            syn::Item::Impl(i) => Some(i),
            _ => None,
        }
    }
    pub fn ensure_items(&mut self) -> &mut Vec<syn::Item> {
        &mut self.module.content.get_or_insert_with(Default::default).1
    }
    pub fn has_method(&self, method: &str) -> bool {
        self.find_method(&format_ident!("{}", method)).is_some()
    }
    pub fn find_method(&self, ident: &syn::Ident) -> Option<&syn::ImplItemMethod> {
        self.methods_item()?
            .items
            .iter()
            .find_map(|item| match item {
                syn::ImplItem::Method(m) if m.sig.ident == *ident => Some(m),
                _ => None,
            })
    }
    pub fn add_custom_stmt(&self, name: &str, stmt: syn::Stmt) {
        let mut stmts = self.custom_stmts.borrow_mut();
        if let Some(stmts) = stmts.get_mut(name) {
            stmts.push(stmt);
        } else {
            stmts.insert(name.to_owned(), vec![stmt]);
        }
    }
    pub fn has_custom_stmts(&self, name: &str) -> bool {
        self.custom_stmts.borrow().contains_key(name)
    }

    pub fn custom_stmts_for(&self, name: &str) -> Option<TokenStream> {
        self.custom_stmts
            .borrow()
            .get(name)
            .map(|stmts| quote! {{ #(#stmts)* };})
    }
    fn extract_wrapper_methods(
        item: &mut syn::ItemImpl,
        methods: &mut Vec<syn::Signature>,
        base: TypeBase,
        glib: &TokenStream,
        errors: &Errors,
    ) {
        for item in &mut item.items {
            if let syn::ImplItem::Method(method) = item {
                methods.push(method.sig.clone());
                if base == TypeBase::Class {
                    let index = method
                        .attrs
                        .iter()
                        .position(|attr| attr.path.is_ident("constructor"));
                    if let Some(index) = index {
                        let attr = method.attrs.remove(index);
                        Self::check_constructor(method, attr, errors);
                        Self::fill_in_constructor(method, glib);
                    }
                }
            }
        }
    }
    fn check_constructor(method: &syn::ImplItemMethod, attr: syn::Attribute, errors: &Errors) {
        if !attr.tokens.is_empty() {
            errors.push_spanned(&attr.tokens, "Unknown tokens on constructor");
        }
        if let Some(recv) = method.sig.receiver() {
            errors.push_spanned(recv, "`self` not allowed on constructor");
        }
        if matches!(&method.sig.output, syn::ReturnType::Default) {
            errors.push_spanned(&method.sig, "Constructor must have a return type");
        }
        for arg in &method.sig.inputs {
            if let syn::FnArg::Typed(syn::PatType { pat, .. }) = arg {
                match pat.as_ref() {
                    syn::Pat::Ident(_) => {}
                    p => errors.push_spanned(p, "Constructor argument must be an ident"),
                }
            }
        }
    }
    fn fill_in_constructor(method: &mut syn::ImplItemMethod, glib: &TokenStream) {
        if method.block.stmts.is_empty() {
            let args = util::signature_args(&method.sig).map(|ident| {
                let name = ident.to_string().to_kebab_case();
                quote! { (#name, &#ident) }
            });
            method.attrs.push(parse_quote! { #[inline] });
            method.block = parse_quote! {
                {
                    #glib::Object::new::<Self>(&[#(#args),*])
                        .expect("Failed to construct object")
                }
            };
        }
    }
    pub fn method_wrapper<F>(&self, name: &str, sig_func: F) -> Option<TokenStream>
    where
        F: FnOnce(&syn::Ident) -> syn::Signature,
    {
        let has_method = self.has_method(name);
        let custom = self.custom_stmts_for(name);
        if !has_method && custom.is_none() {
            return None;
        }
        let ident = format_ident!("{}", name);
        let sig = sig_func(&ident);
        let call_user_method = has_method.then(|| {
            let input_names = sig.inputs.iter().map(|arg| match arg {
                syn::FnArg::Receiver(_) => quote! { self },
                syn::FnArg::Typed(arg) => match &*arg.pat {
                    syn::Pat::Ident(syn::PatIdent { ident, .. }) => quote! { #ident },
                    _ => unimplemented!(),
                },
            });
            quote! { Self::#ident(#(#input_names),*) }
        });
        Some(quote! {
            #sig {
                #custom
                #call_user_method
            }
        })
    }
    pub fn trait_head_with_params(
        &self,
        ty: &syn::Path,
        trait_: TokenStream,
        params: Option<impl IntoIterator<Item = syn::GenericParam>>,
    ) -> TokenStream {
        if let Some(params) = params {
            let type_generics = self.generics.as_ref().map(|g| g.split_for_impl().1);
            let where_clause = self.generics.as_ref().map(|g| g.split_for_impl().2);
            let mut generics = self.generics.clone().unwrap_or_default();
            generics.params.extend(params);
            let (impl_generics, _, _) = generics.split_for_impl();
            quote! {
                impl #impl_generics #trait_ for #ty #type_generics #where_clause
            }
        } else if let Some(generics) = &self.generics {
            let (impl_generics, type_generics, where_clause) = generics.split_for_impl();
            quote! {
                impl #impl_generics #trait_ for #ty #type_generics #where_clause
            }
        } else {
            quote! {
                impl #trait_ for #ty
            }
        }
    }
    #[inline]
    pub fn trait_head(&self, ty: &syn::Path, trait_: TokenStream) -> TokenStream {
        self.trait_head_with_params(ty, trait_, None::<[syn::GenericParam; 0]>)
    }
    pub(crate) fn properties_method(&self) -> Option<TokenStream> {
        let has_method = self.has_method("properties");
        let custom = self.custom_stmts_for("properties");
        if self.properties.is_empty() && !has_method && custom.is_none() {
            return None;
        }
        let go = &self.crate_ident;
        let glib = self.glib();
        let sub_ty = self.type_(
            TypeMode::Subclass,
            TypeMode::Subclass,
            TypeContext::External,
        )?;
        let defs = self.properties.iter().map(|p| p.definition(go));
        let extra = has_method.then(|| {
            quote_spanned! { Span::mixed_site() =>
                properties.extend(#sub_ty::properties());
            }
        });
        let base_index_set = (self.base == TypeBase::Class
            && !self.properties.is_empty()
            && extra.is_some())
        .then(|| {
            quote_spanned! { Span::mixed_site() =>
                _GENERATED_PROPERTIES_BASE_INDEX.set(properties.len()).unwrap();
            }
        });
        Some(quote_spanned! { Span::mixed_site() =>
            fn properties() -> &'static [#glib::ParamSpec] {
                static PROPS: #glib::once_cell::sync::Lazy<::std::vec::Vec<#glib::ParamSpec>> =
                    #glib::once_cell::sync::Lazy::new(|| {
                        let mut properties = ::std::vec::Vec::<#glib::ParamSpec>::new();
                        #extra
                        #custom
                        #base_index_set
                        properties.extend([#(#defs),*]);
                        properties
                    });
                ::std::convert::AsRef::as_ref(::std::ops::Deref::deref(&PROPS))
            }
        })
    }
    pub(crate) fn signals_method(&self) -> Option<TokenStream> {
        let has_method = self.has_method("signals");
        let custom = self.custom_stmts_for("signals");
        if self.signals.is_empty() && !has_method && custom.is_none() {
            return None;
        }
        let glib = self.glib();
        let ty = self.type_(TypeMode::Subclass, TypeMode::Wrapper, TypeContext::External)?;
        let sub_ty = self.type_(
            TypeMode::Subclass,
            TypeMode::Subclass,
            TypeContext::External,
        )?;
        let defs = self
            .signals
            .iter()
            .map(|s| s.definition(&ty, &sub_ty, &glib));
        let extra = has_method.then(|| {
            quote_spanned! { Span::mixed_site() =>
                signals.extend(#sub_ty::signals());
            }
        });
        Some(quote_spanned! { Span::mixed_site() =>
            fn signals() -> &'static [#glib::subclass::Signal] {
                static SIGNALS: #glib::once_cell::sync::Lazy<::std::vec::Vec<#glib::subclass::Signal>> =
                    #glib::once_cell::sync::Lazy::new(|| {
                        let mut signals = ::std::vec::Vec::<#glib::subclass::Signal>::new();
                        #extra
                        #custom
                        signals.extend([#(#defs),*]);
                        signals
                    });
                ::std::convert::AsRef::as_ref(::std::ops::Deref::deref(&SIGNALS))
            }
        })
    }
    fn public_method_prototypes(&self) -> Vec<TokenStream> {
        let go = &self.crate_ident;
        let glib = self.glib();
        self.properties
            .iter()
            .flat_map(|p| p.method_prototypes(self.concurrency, go))
            .chain(
                self.signals
                    .iter()
                    .flat_map(|s| s.method_prototypes(self.concurrency, &glib)),
            )
            .chain(self.public_methods.iter().map(|m| m.prototype()))
            .chain(self.virtual_methods.iter().map(|m| m.prototype(&glib)))
            .collect()
    }
    pub(crate) fn method_path(&self, method: &str, from: TypeMode) -> Option<TokenStream> {
        let glib = self.glib();
        let subclass_ty = self.type_(from, TypeMode::Subclass, TypeContext::External)?;
        let method = format_ident!("{}", method);
        Some(match self.base {
            TypeBase::Class => quote! {
                <#subclass_ty as #glib::subclass::object::ObjectImpl>::#method
            },
            TypeBase::Interface => quote! {
                <#subclass_ty as #glib::subclass::prelude::ObjectInterface>::#method
            },
        })
    }
    pub(crate) fn public_method_definitions(
        &self,
    ) -> Option<impl Iterator<Item = TokenStream> + '_> {
        let ty = self.type_(TypeMode::Subclass, TypeMode::Wrapper, TypeContext::External)?;

        let properties = {
            let go = self.crate_ident.clone();
            let ty = ty.clone();
            let properties_path = self.method_path("properties", TypeMode::Subclass)?;
            self.properties.iter().enumerate().flat_map(move |(i, p)| {
                p.method_definitions(i, &ty, self.concurrency, &properties_path, &go)
            })
        };
        let signals = {
            let glib = self.glib();
            self.signals
                .iter()
                .flat_map(move |s| s.method_definitions(self.concurrency, &glib))
        };
        let public_methods = {
            let glib = self.glib();
            let ty = ty.clone();
            let sub_ty = self.type_(
                TypeMode::Subclass,
                TypeMode::Subclass,
                TypeContext::External,
            )?;
            self.public_methods
                .iter()
                .map(move |m| m.definition(&ty, &sub_ty, &glib))
        };
        let virtual_methods = {
            let glib = self.glib();
            self.virtual_methods
                .iter()
                .map(move |m| m.definition(&ty, &glib))
        };
        Some(
            properties
                .chain(signals)
                .chain(public_methods)
                .chain(virtual_methods),
        )
    }
    pub(crate) fn public_methods(&self, trait_name: Option<&syn::Ident>) -> Option<TokenStream> {
        let glib = self.glib();
        let mut items = self.public_method_definitions()?.peekable();
        items.peek()?;
        let name = self.name.as_ref()?;
        let type_ident = format_ident!("____Object");
        let vis = &self.inner_vis;
        if let Some(generics) = self.generics.as_ref() {
            let (impl_generics, type_generics, where_clause) = generics.split_for_impl();
            if let Some(trait_name) = trait_name {
                let mut generics = generics.clone();
                let param = parse_quote! { #type_ident: #glib::IsA<super::#name #type_generics> };
                generics.params.push(param);
                let (impl_generics, _, _) = generics.split_for_impl();
                let protos = self.public_method_prototypes();
                Some(quote! {
                    #vis trait #trait_name: 'static {
                        #(#protos;)*
                    }
                    impl #impl_generics #trait_name for #type_ident #where_clause {
                        #(#items)*
                    }
                })
            } else {
                Some(quote! {
                    impl #impl_generics super::#name #type_generics #where_clause {
                        #(pub #items)*
                    }
                })
            }
        } else if let Some(trait_name) = trait_name {
            let protos = self.public_method_prototypes();
            Some(quote! {
                #vis trait #trait_name: 'static {
                    #(#protos;)*
                }
                impl<#type_ident: #glib::IsA<super::#name>> #trait_name for #type_ident {
                    #(#items)*
                }
            })
        } else {
            Some(quote! {
                impl super::#name {
                    #(pub #items)*
                }
            })
        }
    }
    #[inline]
    fn default_vtable_assignments(&self, class_ident: &TokenStream) -> Option<TokenStream> {
        if self.virtual_methods.is_empty() {
            return None;
        }
        let glib = self.glib();
        let name = self.name.as_ref()?;
        let ty = self.type_(TypeMode::Subclass, TypeMode::Wrapper, TypeContext::External)?;
        let ty = parse_quote! { #ty };
        Some(FromIterator::from_iter(self.virtual_methods.iter().map(
            |m| m.set_default_trampoline(name, &ty, class_ident, &glib),
        )))
    }
    pub(crate) fn type_init_body(&self, class_ident: &TokenStream) -> Option<TokenStream> {
        let glib = self.glib();
        let wrapper_ty =
            self.type_(TypeMode::Subclass, TypeMode::Wrapper, TypeContext::External)?;
        let sub_ty = self.type_(
            TypeMode::Subclass,
            TypeMode::Subclass,
            TypeContext::External,
        )?;
        let set_vtable = self.default_vtable_assignments(class_ident);
        let object_class = quote_spanned! { Span::mixed_site() => ____object_class };
        let overrides = self
            .signals
            .iter()
            .filter_map(|signal| {
                signal.class_init_override(&wrapper_ty, &sub_ty, &object_class, &glib)
            })
            .collect::<Vec<_>>();
        if set_vtable.is_none() && overrides.is_empty() {
            return None;
        }
        let deref_class = (!overrides.is_empty()).then(|| {
            quote! {
                let #object_class = &mut *#class_ident;
                let #object_class = #glib::Class::upcast_ref_mut::<#glib::Object>(#object_class);
            }
        });
        Some(quote! {
            #set_vtable
            {
                #deref_class
                #(#overrides)*
            }
        })
    }
    #[inline]
    fn subclassed_vtable_assignments(
        &self,
        type_ident: &syn::Ident,
        class_ident: &syn::Ident,
        trait_name: &syn::Ident,
    ) -> Option<TokenStream> {
        if self.virtual_methods.is_empty() {
            return None;
        }
        let glib = self.glib();
        let ty = self.type_(TypeMode::Wrapper, TypeMode::Wrapper, TypeContext::External)?;
        let ty = parse_quote! { #ty };
        Some(FromIterator::from_iter(self.virtual_methods.iter().map(
            |m| m.set_subclassed_trampoline(&ty, trait_name, type_ident, class_ident, &glib),
        )))
    }
    pub(crate) fn child_type_init_body(
        &self,
        type_ident: &syn::Ident,
        class_ident: &syn::Ident,
        trait_name: &syn::Ident,
    ) -> Option<TokenStream> {
        self.subclassed_vtable_assignments(type_ident, class_ident, trait_name)
    }
    pub(crate) fn type_struct_fields(&self) -> Vec<TokenStream> {
        let ty = unwrap_or_return!(
            self.type_(TypeMode::Subclass, TypeMode::Wrapper, TypeContext::External),
            Vec::new()
        );
        let ty = parse_quote! { #ty };
        self.virtual_methods
            .iter()
            .map(|method| method.vtable_field(&ty))
            .collect()
    }
    #[inline]
    fn impl_trait(
        &self,
        trait_name: &syn::Ident,
        ext_trait_name: &syn::Ident,
        parent_trait: Option<TokenStream>,
    ) -> Option<TokenStream> {
        let glib = self.glib();
        let vis = &self.vis;
        let parent_trait = parent_trait.unwrap_or_else(|| {
            quote! {
                #glib::subclass::object::ObjectImpl
            }
        });
        let virtual_methods_default = self
            .virtual_methods
            .iter()
            .map(|m| m.default_definition(ext_trait_name, &glib));
        Some(quote! {
            #vis trait #trait_name: #parent_trait + 'static {
                #(#virtual_methods_default)*
            }
        })
    }
    #[inline]
    fn impl_ext_trait(
        &self,
        trait_name: &syn::Ident,
        ext_trait_name: &syn::Ident,
    ) -> Option<TokenStream> {
        if self.virtual_methods.is_empty() {
            return None;
        }
        let glib = self.glib();
        let ty = self.type_(TypeMode::Wrapper, TypeMode::Wrapper, TypeContext::External)?;
        let ty = parse_quote! { #ty };
        let type_ident = syn::Ident::new("____Object", Span::mixed_site());
        let vis = &self.vis;
        let parent_method_protos = self
            .virtual_methods
            .iter()
            .map(|m| m.parent_prototype(&glib));
        let parent_method_definitions = self
            .virtual_methods
            .iter()
            .map(|m| m.parent_definition(&ty, &glib));
        Some(quote! {
            #vis trait #ext_trait_name: #glib::subclass::types::ObjectSubclass {
                #(#parent_method_protos;)*
            }
            impl<#type_ident: #trait_name> #ext_trait_name for #type_ident {
                #(#parent_method_definitions)*
            }
        })
    }
    pub(crate) fn virtual_traits(
        &self,
        trait_name: Option<&syn::Ident>,
        ext_trait_name: Option<&syn::Ident>,
        parent_trait: Option<TokenStream>,
    ) -> Option<TokenStream> {
        let trait_name = trait_name?;
        let ext_trait_name = ext_trait_name?;
        let impl_trait = self.impl_trait(trait_name, ext_trait_name, parent_trait);
        let impl_ext_trait = self.impl_ext_trait(trait_name, ext_trait_name);
        Some(quote! {
            #impl_trait
            #impl_ext_trait
        })
    }
    fn private_methods(&self) -> Vec<TokenStream> {
        let mut methods = Vec::new();
        let glib = self.glib();

        for signal in &self.signals {
            if let Some(chain) = signal.chain_definition(&glib) {
                methods.push(chain);
            }
        }

        methods
    }
    pub(crate) fn extra_private_items(&self) -> Vec<TokenStream> {
        let mut items = Vec::new();

        let name = unwrap_or_return!(self.name.as_ref(), items);
        let glib = self.glib();
        let wrapper_ty = unwrap_or_return!(
            self.type_(TypeMode::Subclass, TypeMode::Wrapper, TypeContext::External),
            items
        );

        for signal in &self.signals {
            items.push(signal.signal_id_cell_definition(&wrapper_ty, &glib));
        }

        let private_methods = self.private_methods();

        if !private_methods.is_empty() {
            let head = if let Some(generics) = self.generics.as_ref() {
                let (impl_generics, type_generics, where_clause) = generics.split_for_impl();
                quote! { impl #impl_generics #name #type_generics #where_clause }
            } else {
                quote! { impl #name }
            };
            items.push(quote! {
                #head {
                    #(#private_methods)*
                }
            });
        }

        items
    }
}

impl Spanned for TypeDefinition {
    fn span(&self) -> proc_macro2::Span {
        self.module.span()
    }
}
