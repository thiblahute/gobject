use crate::{util, TypeDefinition, TypeDefinitionParser, Properties, TypeBase};
use darling::{
    util::{Flag, PathList, SpannedValue},
    FromMeta,
};
use heck::ToUpperCamelCase;
use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote, quote_spanned, ToTokens};
use syn::spanned::Spanned;

#[derive(Debug, Default, FromMeta)]
#[darling(default)]
struct Attrs {
    pub name: Option<syn::Ident>,
    pub ns: Option<syn::Ident>,
    pub ext_trait: Option<syn::Ident>,
    pub parent_trait: Option<syn::Path>,
    pub wrapper: Option<bool>,
    #[darling(rename = "abstract")]
    pub abstract_: SpannedValue<Flag>,
    #[darling(rename = "final")]
    pub final_: SpannedValue<Flag>,
    pub extends: PathList,
    pub implements: PathList,
}

impl Attrs {
    fn validate(&self, def: &TypeDefinition, errors: &mut Vec<darling::Error>) {
        use crate::validations::*;

        if self.name.is_none() {
            util::push_error(
                errors,
                def.span(),
                "Class must have a `name = \"...\"` parameter or a #[properties] struct",
            );
        }
        let abstract_ = ("abstract", check_flag(&self.abstract_));
        let final_ = ("final", check_flag(&self.final_));
        only_one([&abstract_, &final_], errors);
    }
}

#[derive(Debug)]
pub struct ClassOptions(Attrs);

impl ClassOptions {
    pub fn parse(tokens: TokenStream, errors: &mut Vec<darling::Error>) -> Self {
        Self(util::parse_list(tokens, errors))
    }
}

#[derive(Debug)]
pub struct ClassDefinition {
    pub inner: TypeDefinition,
    pub ns: Option<syn::Ident>,
    pub ext_trait: Option<syn::Ident>,
    pub parent_trait: Option<syn::Path>,
    pub wrapper: bool,
    pub abstract_: bool,
    pub final_: bool,
    pub extends: Vec<syn::Path>,
    pub implements: Vec<syn::Path>,
    pub extra_class_init_stmts: Vec<TokenStream>,
    pub extra_instance_init_stmts: Vec<TokenStream>,
}

impl ClassDefinition {
    pub fn type_parser() -> TypeDefinitionParser {
        let mut parser = TypeDefinitionParser::new();
        parser
            .add_custom_method("properties")
            .add_custom_method("signals")
            .add_custom_method("set_property")
            .add_custom_method("property")
            .add_custom_method("constructed")
            .add_custom_method("dispose")
            .add_custom_method("type_init")
            .add_custom_method("new")
            .add_custom_method("with_class")
            .add_custom_method("class_init")
            .add_custom_method("instance_init");
        parser
    }
    pub fn from_type(
        def: TypeDefinition,
        opts: ClassOptions,
        crate_ident: syn::Ident,
        errors: &mut Vec<darling::Error>,
    ) -> Self {
        let attrs = opts.0;
        attrs.validate(&def, errors);

        let mut class = Self {
            inner: def,
            ns: attrs.ns,
            ext_trait: attrs.ext_trait,
            parent_trait: attrs.parent_trait,
            wrapper: attrs.wrapper.unwrap_or(true),
            abstract_: attrs.abstract_.is_some(),
            final_: attrs.final_.is_some(),
            extends: (*attrs.extends).clone(),
            implements: (*attrs.implements).clone(),
            extra_class_init_stmts: Vec::new(),
            extra_instance_init_stmts: Vec::new(),
        };

        if let Some(name) = attrs.name {
            class.inner.set_name(name);
        }
        class.inner.set_crate_ident(crate_ident);

        let extra = class.extra_private_items();

        let (_, items) = class
            .inner
            .module
            .content
            .get_or_insert_with(Default::default);
        items.extend(extra.into_iter());

        class
    }
    #[inline]
    fn derived_method<F>(&self, method: &str, func: F) -> Option<TokenStream>
    where
        F: FnOnce(&str) -> Option<TokenStream>,
    {
        self.inner
            .has_custom_method(method)
            .then(|| func(format!("{}_derived", method).as_str()))
            .flatten()
    }
    fn extra_private_items(&self) -> Vec<syn::Item> {
        let derived_methods = [
            self.derived_method("properties", |n| self.inner.properties_method(n)),
            self.derived_method("signals", |n| self.inner.signals_method(n)),
            self.derived_method("set_property", |n| self.set_property_method(n)),
            self.derived_method("property", |n| self.property_method(n)),
            self.derived_method("class_init", |n| self.class_init_method(n)),
            self.derived_method("instance_init", |n| self.instance_init_method(n)),
        ]
        .into_iter()
        .filter_map(|t| t)
        .collect::<Vec<_>>();
        let derived_methods = (!derived_methods.is_empty())
            .then(|| self.inner.name.as_ref())
            .flatten()
            .map(|name| {
                let head = if let Some(generics) = &self.inner.generics {
                    let (impl_generics, type_generics, where_clause) = generics.split_for_impl();
                    quote! { impl #impl_generics #name #type_generics #where_clause }
                } else {
                    quote! { impl #name }
                };
                quote! {
                    #head {
                        #(#derived_methods)*
                    }
                }
            });
        self.inner
            .extra_private_items()
            .into_iter()
            .chain(
                [
                    self.object_subclass_impl(),
                    self.object_impl_impl(),
                    self.class_struct_definition(),
                    derived_methods,
                ]
                .into_iter()
                .filter_map(|t| t),
            )
            .map(syn::Item::Verbatim)
            .collect()
    }
    fn parent_type(&self) -> Option<TokenStream> {
        let glib = self.inner.glib()?;
        Some(
            self.extends
                .first()
                .map(|p| p.to_token_stream())
                .unwrap_or_else(|| quote! { #glib::Object }),
        )
    }
    #[inline]
    fn wrapper(&self) -> Option<TokenStream> {
        if !self.wrapper {
            return None;
        }
        let mut inherits = Vec::new();
        if !self.extends.is_empty() {
            inherits.push(quote! { @extends });
            for extend in &*self.extends {
                inherits.push(extend.to_token_stream());
            }
        }
        if !self.implements.is_empty() {
            inherits.push(quote! { @implements });
            for implement in &*self.implements {
                inherits.push(implement.to_token_stream());
            }
        }
        let mod_name = &self.inner.module.ident;
        let name = self.inner.name.as_ref()?;
        let glib = self.inner.glib()?;
        let generics = self.inner.generics.as_ref();
        Some(quote! {
            #glib::wrapper! {
                pub struct #name #generics(ObjectSubclass<#mod_name::#name #generics>) #(#inherits),*;
            }
        })
    }
    #[inline]
    fn ext_trait(&self) -> Option<syn::Ident> {
        if self.final_ {
            return None;
        }
        let name = self.inner.name.as_ref()?;
        Some(
            self.ext_trait
                .clone()
                .unwrap_or_else(|| format_ident!("{}Ext", name)),
        )
    }
    fn class_init_method(&self, method_name: &str) -> Option<TokenStream> {
        let glib = self.inner.glib()?;
        let class_ident = syn::Ident::new("____class", Span::mixed_site());
        let method_name = format_ident!("{}", method_name);
        let body = self.inner.type_init_body(&class_ident);
        let extra = &self.extra_class_init_stmts;
        if body.is_none() && extra.is_empty() {
            return None;
        }
        Some(quote! {
            fn #method_name(#class_ident: &mut <Self as #glib::subclass::types::ObjectSubclass>::Class) {
                #body
                #(#extra)*
            }
        })
    }
    fn instance_init_method(&self, method_name: &str) -> Option<TokenStream> {
        let glib = self.inner.glib()?;
        let obj_ident = syn::Ident::new("____obj", Span::mixed_site());
        let method_name = format_ident!("{}", method_name);
        let extra = &self.extra_instance_init_stmts;
        if extra.is_empty() {
            return None;
        }
        Some(quote! {
            fn #method_name(#obj_ident: &#glib::subclass::types::InitializingObject<Self>) {
                #(#extra)*
            }
        })
    }
    fn class_struct_definition(&self) -> Option<TokenStream> {
        let fields = self.inner.type_struct_fields();
        if fields.is_empty() {
            return None;
        }
        let name = self.inner.name.as_ref()?;
        let generics = self.inner.generics.as_ref()?;
        let class_name = format_ident!("{}Class", name);
        let glib = self.inner.glib()?;
        let parent_type = self.parent_type()?;
        let head = if let Some(generics) = &self.inner.generics {
            let (impl_generics, type_generics, where_clause) = generics.split_for_impl();
            quote! {
                unsafe impl #impl_generics #glib::subclass::types::ClassStruct
                    for #class_name #type_generics #where_clause
            }
        } else {
            quote! {
                unsafe impl #glib::subclass::types::ClassStruct for #class_name
            }
        };
        Some(quote! {
            #[repr(C)]
            pub struct #class_name #generics {
                pub parent_class: <<#parent_type as #glib::Object::ObjectSubclassIs>::Subclass as #glib::subclass::types::ObjectSubclass>::Class,
                #(pub #fields),*
            }
            #head {
                type Type = #name #generics;
            }
        })
    }
    #[inline]
    fn object_subclass_impl(&self) -> Option<TokenStream> {
        let glib = self.inner.glib()?;
        let name = self.inner.name.as_ref()?;
        let gtype_name = if let Some(ns) = &self.ns {
            format!("{}{}", ns, name)
        } else {
            name.to_string()
        }
        .to_upper_camel_case();
        let abstract_ = self.abstract_;
        let parent_type = self.parent_type()?;
        let interfaces = &self.implements;
        let class_struct_type = self.class_struct_definition().map(|_| {
            let class_name = format_ident!("{}Class", name);
            quote! { type Class = #class_name; }
        });
        let class_init = self
            .inner
            .custom_method("class_init")
            .or_else(|| self.class_init_method("class_init"));
        let instance_init = self
            .inner
            .custom_method("instance_init")
            .or_else(|| self.instance_init_method("instance_init"));
        let extra = self
            .inner
            .custom_methods(&["type_init", "new", "with_class"]);
        Some(quote! {
            #[#glib::object_subclass]
            impl #glib::subclass::types::ObjectSubclass for #name {
                const NAME: &'static ::std::primitive::str = #gtype_name;
                const ABSTRACT: bool = #abstract_;
                type Type = super::#name;
                type ParentType = #parent_type;
                type Interfaces = (#(#interfaces,)*);
                #class_struct_type
                #class_init
                #instance_init
                #extra
            }
        })
    }
    fn unimplemented_property(glib: &TokenStream) -> TokenStream {
        quote! {
            unimplemented!(
                "invalid property id {} for \"{}\" of type '{}' in '{}'",
                id,
                pspec.name(),
                pspec.type_().name(),
                <<Self as #glib::subclass::types::ObjectSubclass>::Type as #glib::object::ObjectExt>::type_(
                    obj
                ).name()
            )
        }
    }
    fn set_property_method(&self, method_name: &str) -> Option<TokenStream> {
        if self.inner.properties.is_empty() {
            return None;
        }
        let go = self.inner.crate_ident.as_ref()?;
        let glib = self.inner.glib()?;
        let set_impls = self
            .inner
            .properties
            .iter()
            .enumerate()
            .filter_map(|(index, prop)| prop.set_impl(index, go));
        let method_name = format_ident!("{}", method_name);
        let properties_path = self.inner.method_path("properties")?;
        let unimplemented = Self::unimplemented_property(&glib);
        Some(quote! {
            fn #method_name(
                &self,
                obj: &<Self as #glib::subclass::types::ObjectSubclass>::Type,
                id: usize,
                value: &#glib::Value,
                pspec: &#glib::ParamSpec
            ) {
                let properties = #properties_path();
                #(#set_impls)*
                #unimplemented
            }
        })
    }
    fn property_method(&self, method_name: &str) -> Option<TokenStream> {
        if self.inner.properties.is_empty() {
            return None;
        }
        let go = self.inner.crate_ident.as_ref()?;
        let glib = self.inner.glib()?;
        let get_impls = self
            .inner
            .properties
            .iter()
            .enumerate()
            .filter_map(|(index, prop)| prop.get_impl(index, go));
        let method_name = format_ident!("{}", method_name);
        let properties_path = self.inner.method_path("properties")?;
        let unimplemented = Self::unimplemented_property(&glib);
        Some(quote! {
            fn #method_name(
                &self,
                obj: &<Self as #glib::subclass::types::ObjectSubclass>::Type,
                id: usize,
                pspec: &#glib::ParamSpec
            ) -> #glib::Value {
                let properties = #properties_path();
                #(#get_impls)*
                #unimplemented
            }
        })
    }
    #[inline]
    fn object_impl_impl(&self) -> Option<TokenStream> {
        let glib = self.inner.glib()?;
        let name = self.inner.name.as_ref()?;
        let properties = self
            .inner
            .custom_method("properties")
            .or_else(|| self.inner.properties_method("properties"));
        let signals = self
            .inner
            .custom_method("signals")
            .or_else(|| self.inner.signals_method("signals"));
        let set_property = self
            .inner
            .custom_method("set_property")
            .or_else(|| self.set_property_method("set_property"));
        let property = self
            .inner
            .custom_method("property")
            .or_else(|| self.property_method("property"));
        let extra = self.inner.custom_methods(&["constructed", "dispose"]);
        Some(quote! {
            impl #glib::subclass::object::ObjectImpl for #name {
                #properties
                #set_property
                #property
                #signals
                #extra
            }
        })
    }
    #[inline]
    fn is_subclassable_impl(&self) -> Option<TokenStream> {
        let glib = self.inner.glib()?;
        let name = self.inner.name.as_ref()?;
        let type_ident = syn::Ident::new("____Object", Span::mixed_site());
        let class_ident = syn::Ident::new("____class", Span::mixed_site());
        let body = self.inner.child_type_init_body(&type_ident, &class_ident)?;
        let trait_name = format_ident!("{}Impl", name);
        let head = if let Some(generics) = &self.inner.generics {
            let (impl_generics, type_generics, where_clause) = generics.split_for_impl();
            quote! {
                unsafe impl #impl_generics #glib::subclass::types::IsSubclassable<#type_ident>
                    for #name #type_generics #where_clause
            }
        } else {
            quote! {
                unsafe impl<#type_ident: #trait_name> #glib::subclass::types::IsSubclassable<#type_ident> for #name
            }
        };
        Some(quote! {
            #head {
                fn class_init(#class_ident: &mut #glib::Class<Self>) {
                    <Self as #glib::subclass::types::IsSubclassableExt>::parent_class_init::<T>(
                        #glib::Cast::upcast_ref_mut(#class_ident)
                    );
                    let #class_ident = ::std::convert::AsMut::as_mut(#class_ident);
                    #body
                }
            }
        })
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

impl ToTokens for ClassDefinition {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let glib = unwrap_or_return!(self.inner.glib(), ());
        let module = &self.inner.module;

        let wrapper = self.wrapper();
        let trait_name = self.ext_trait();
        let public_methods = self.inner.public_methods(trait_name.as_ref());
        let is_subclassable = self.is_subclassable_impl();
        let parent_trait = self
            .parent_trait
            .as_ref()
            .map(|p| p.to_token_stream())
            .unwrap_or_else(|| quote! { #glib::subclass::object::ObjectImpl });
        let virtual_traits = self.inner.virtual_traits(&parent_trait);

        let class = quote_spanned! { module.span() =>
            #module
            #wrapper
            #public_methods
            #is_subclassable
            #virtual_traits
        };
        class.to_tokens(tokens);
    }
}

pub fn derived_class_properties(
    input: &syn::DeriveInput,
    go: &syn::Ident,
    errors: &mut Vec<darling::Error>,
) -> TokenStream {
    let Properties {
        final_type,
        properties,
    } = Properties::from_derive_input(input, TypeBase::Class, false, errors);
    let glib = quote! { #go::glib };
    let name = &input.ident;
    let generics = &input.generics;

    let (impl_generics, type_generics, where_clause) = generics.split_for_impl();
    let ty = quote! { #name #type_generics };
    let properties_path = quote! { #ty::derived_properties };
    let wrapper_ty = quote! { <#ty as #glib::subclass::types::ObjectSubclass>::Type };
    let trait_name = final_type.is_none().then(|| format_ident!("{}PropertiesExt", input.ident));

    let mut items = Vec::new();
    for (index, prop) in properties.iter().enumerate() {
        for item in prop.method_definitions(index, &wrapper_ty, &properties_path, go) {
            items.push(item);
        }
    }

    let public_methods = if let Some(trait_name) = trait_name {
        let type_ident = format_ident!("____Object");
        let mut generics = generics.clone();
        let param = util::parse(
            quote! { #type_ident: #glib::IsA<#wrapper_ty> },
            &mut vec![],
        )
        .unwrap();
        generics.params.push(param);
        let (impl_generics, _, where_clause) = generics.split_for_impl();

        let mut protos = Vec::new();
        for prop in &properties {
            for proto in prop.method_prototypes(go) {
                protos.push(util::make_stmt(proto));
            }
        }

        quote! {
            pub trait #trait_name: 'static {
                #(#protos)*
            }
            impl #impl_generics #trait_name for #type_ident #where_clause {
                #(#items)*
            }
        }
    } else {
        let final_type = final_type.as_ref().unwrap();
        quote! {
            impl #impl_generics #final_type #type_generics #where_clause {
                #(#items)*
            }
        }
    };

    let defs = properties.iter().map(|p| p.definition(go));
    let set_impls = properties
        .iter()
        .enumerate()
        .filter_map(|(index, prop)| prop.set_impl(index, go));
    let get_impls = properties
        .iter()
        .enumerate()
        .filter_map(|(index, prop)| prop.get_impl(index, go));
    let unimplemented = ClassDefinition::unimplemented_property(&glib);

    quote! {
        #public_methods
        impl #impl_generics #ty #where_clause {
            fn derived_properties() -> &'static [#glib::ParamSpec] {
                static PROPS: #glib::once_cell::sync::Lazy<::std::vec::Vec<#glib::ParamSpec>> =
                    #glib::once_cell::sync::Lazy::new(|| {
                        vec![#(#defs),*]
                    });
                ::std::convert::AsRef::as_ref(::std::ops::Deref::deref(&PROPS))
            }
            fn derived_set_property(
                &self,
                obj: &<Self as #glib::subclass::types::ObjectSubclass>::Type,
                id: usize,
                value: &#glib::Value,
                pspec: &#glib::ParamSpec
            ) {
                let properties = #properties_path();
                #(#set_impls)*
                #unimplemented
            }
            fn derived_get_property(
                &self,
                obj: &<Self as #glib::subclass::types::ObjectSubclass>::Type,
                id: usize,
                pspec: &#glib::ParamSpec
            ) -> #glib::Value {
                let properties = #properties_path();
                #(#get_impls)*
                #unimplemented
            }
        }
    }
}
