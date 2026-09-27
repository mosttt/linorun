#![forbid(unsafe_code)]

//! Component authoring and typed service adapters for Linorun.

mod component_attr;
mod provider_impl;

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::{format_ident, quote};
use syn::{
    Attribute, Expr, Ident, LitStr, Pat, Token, Type, Visibility, braced, bracketed, parenthesized,
    parse::{Parse, ParseStream},
    parse_macro_input,
    punctuated::Punctuated,
};

/// Turns an async component function into a reusable, zero-argument component factory.
///
/// The function accepts a `ComponentContext` and optionally `Arc<Config>`, and returns
/// `Result<(), ActivationFailure>`. Requirements and provided services are optional.
/// Each call to the generated factory shares the same static descriptor.
///
/// ```
/// use linorun_core::{ActivationFailure, ComponentContext};
/// use linorun_macros::component;
///
/// #[component(id = "app/worker@1")]
/// async fn worker(context: ComponentContext) -> Result<(), ActivationFailure> {
///     let _ = context;
///     Ok(())
/// }
///
/// let component = worker();
/// ```
#[proc_macro_attribute]
pub fn component(attributes: TokenStream, item: TokenStream) -> TokenStream {
    let attributes = parse_macro_input!(attributes as component_attr::ComponentArgs);
    let function = parse_macro_input!(item as syn::ItemFn);
    component_attr::expand(attributes, function)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Implements a generated service provider with ordinary async methods.
///
/// Apply to a non-generic trait impl. Methods take `&self`, a `CallContext`, and
/// owned arguments. The macro generates the object-safe boxed Future signatures.
///
/// ```
/// use linorun_core::CallContext;
/// use linorun_macros::{service_contract, service_impl};
///
/// service_contract! {
///     contract Counter {
///         id: "app/counter@1",
///         methods: [async fn value() -> usize;],
///     }
/// }
///
/// struct CounterService;
///
/// #[service_impl]
/// impl CounterProvider for CounterService {
///     async fn value(&self, _: CallContext<()>) -> usize {
///         42
///     }
/// }
/// ```
#[proc_macro_attribute]
pub fn service_impl(attributes: TokenStream, item: TokenStream) -> TokenStream {
    if !attributes.is_empty() {
        return syn::Error::new(Span::call_site(), "service_impl takes no arguments")
            .into_compile_error()
            .into();
    }
    let implementation = parse_macro_input!(item as syn::ItemImpl);
    provider_impl::expand(implementation)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

mod keyword {
    syn::custom_keyword!(contract);
    syn::custom_keyword!(requires);
    syn::custom_keyword!(provides);
    syn::custom_keyword!(intercept);
    syn::custom_keyword!(identity);
    syn::custom_keyword!(merge);
    syn::custom_keyword!(methods);
}

/// Defines one process-wide `LazyLock<ComponentDescriptor>`.
///
/// ```
/// use linorun_core::ComponentDescriptor;
/// use linorun_macros::component_descriptor;
///
/// component_descriptor! {
///     pub static WORKER_DESCRIPTOR: () {
///         id: "app/worker@1",
///         requires: [],
///         provides: [],
///     }
/// }
///
/// let _: &'static ComponentDescriptor = &WORKER_DESCRIPTOR;
/// ```
#[proc_macro]
pub fn component_descriptor(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as ComponentDescriptorInput);
    input.expand().into()
}

/// Defines a contract marker, object-safe provider trait, typed client and erased adapter.
///
/// ```
/// use linorun_macros::service_contract;
///
/// #[derive(Clone, Default)]
/// pub struct LoggerIntercept;
///
/// pub fn merge_logger(_: &LoggerIntercept, _: &LoggerIntercept) -> LoggerIntercept {
///     LoggerIntercept
/// }
///
/// service_contract! {
///     pub contract Logger {
///         id: "app/logger@1",
///         intercept: LoggerIntercept,
///         identity: LoggerIntercept::default(),
///         merge: merge_logger,
///         methods: [
///             async fn info(message: String) -> Result<(), String>;
///         ],
///     }
/// }
/// ```
#[proc_macro]
pub fn service_contract(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as ServiceContractInput);
    input.expand().into()
}

struct RequirementInput {
    contract: Type,
    declared: Option<Expr>,
}

impl Parse for RequirementInput {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let contract = input.parse()?;
        let declared = if input.peek(Token![=>]) {
            input.parse::<Token![=>]>()?;
            Some(input.parse()?)
        } else {
            None
        };
        Ok(Self { contract, declared })
    }
}

struct ComponentDescriptorInput {
    visibility: Visibility,
    name: Ident,
    config: Type,
    id: LitStr,
    requirements: Punctuated<RequirementInput, Token![,]>,
    provides: Punctuated<Type, Token![,]>,
}

impl Parse for ComponentDescriptorInput {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let visibility = input.parse()?;
        input.parse::<Token![static]>()?;
        let name = input.parse()?;
        input.parse::<Token![:]>()?;
        let config = input.parse()?;
        let body;
        braced!(body in input);

        parse_named(&body, "id")?;
        let id = body.parse()?;
        body.parse::<Token![,]>()?;
        body.parse::<keyword::requires>()?;
        body.parse::<Token![:]>()?;
        let requirements_body;
        bracketed!(requirements_body in body);
        let requirements =
            Punctuated::<RequirementInput, Token![,]>::parse_terminated(&requirements_body)?;
        body.parse::<Token![,]>()?;
        body.parse::<keyword::provides>()?;
        body.parse::<Token![:]>()?;
        let provides_body;
        bracketed!(provides_body in body);
        let provides = Punctuated::<Type, Token![,]>::parse_terminated(&provides_body)?;
        if body.peek(Token![,]) {
            body.parse::<Token![,]>()?;
        }
        if !body.is_empty() {
            return Err(body.error("unexpected component descriptor field"));
        }

        Ok(Self {
            visibility,
            name,
            config,
            id,
            requirements,
            provides,
        })
    }
}

impl ComponentDescriptorInput {
    fn expand(self) -> proc_macro2::TokenStream {
        let Self {
            visibility,
            name,
            config,
            id,
            requirements,
            provides,
        } = self;
        let requirements = requirements.iter().map(|requirement| {
            let contract = &requirement.contract;
            match &requirement.declared {
                Some(declared) => quote! {
                    ::linorun_core::Requirement::new::<#contract>(::core::option::Option::Some(#declared))
                },
                None => quote! {
                    ::linorun_core::Requirement::new::<#contract>(::core::option::Option::None)
                },
            }
        });
        let provides = provides.iter().map(|contract| {
            quote! { <#contract as ::linorun_core::ServiceContract>::ID }
        });
        quote! {
            #visibility static #name: ::std::sync::LazyLock<::linorun_core::ComponentDescriptor> =
                ::std::sync::LazyLock::new(|| {
                    ::linorun_core::ComponentDescriptor::new::<#config>(
                        #id,
                        ::std::vec![#(#requirements),*],
                        ::std::vec![#(#provides),*],
                    )
                });
        }
    }
}

fn parse_named(input: ParseStream<'_>, expected: &str) -> syn::Result<()> {
    let name: Ident = input.parse()?;
    if name != expected {
        return Err(syn::Error::new(
            name.span(),
            format!("expected `{expected}`"),
        ));
    }
    input.parse::<Token![:]>()?;
    Ok(())
}

struct ServiceMethod {
    attributes: Vec<Attribute>,
    name: Ident,
    arguments: Punctuated<syn::PatType, Token![,]>,
    output: Type,
}

impl Parse for ServiceMethod {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let attributes = input.call(Attribute::parse_outer)?;
        input.parse::<Token![async]>()?;
        input.parse::<Token![fn]>()?;
        let name = input.parse()?;
        let arguments_body;
        parenthesized!(arguments_body in input);
        let arguments = Punctuated::<syn::PatType, Token![,]>::parse_terminated(&arguments_body)?;
        for argument in &arguments {
            let Pat::Ident(_) = &*argument.pat else {
                return Err(syn::Error::new_spanned(
                    &argument.pat,
                    "service method arguments must use identifier patterns",
                ));
            };
        }
        input.parse::<Token![->]>()?;
        let output = input.parse()?;
        input.parse::<Token![;]>()?;
        Ok(Self {
            attributes,
            name,
            arguments,
            output,
        })
    }
}

struct ServiceContractInput {
    visibility: Visibility,
    name: Ident,
    id: LitStr,
    intercept: Type,
    identity: Expr,
    merge: Expr,
    methods: Vec<ServiceMethod>,
}

impl Parse for ServiceContractInput {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let visibility = input.parse()?;
        input.parse::<keyword::contract>()?;
        let name = input.parse()?;
        let body;
        braced!(body in input);

        parse_named(&body, "id")?;
        let id = body.parse()?;
        body.parse::<Token![,]>()?;
        let (intercept, identity, merge) = if body.peek(keyword::intercept) {
            body.parse::<keyword::intercept>()?;
            body.parse::<Token![:]>()?;
            let intercept = body.parse()?;
            body.parse::<Token![,]>()?;
            body.parse::<keyword::identity>()?;
            body.parse::<Token![:]>()?;
            let identity = body.parse()?;
            body.parse::<Token![,]>()?;
            body.parse::<keyword::merge>()?;
            body.parse::<Token![:]>()?;
            let merge = body.parse()?;
            body.parse::<Token![,]>()?;
            (intercept, identity, merge)
        } else {
            (
                syn::parse_quote!(()),
                syn::parse_quote!({}),
                syn::parse_quote!(|_: &(), _: &()| {}),
            )
        };
        body.parse::<keyword::methods>()?;
        body.parse::<Token![:]>()?;
        let methods_body;
        bracketed!(methods_body in body);
        let mut methods = Vec::new();
        while !methods_body.is_empty() {
            methods.push(methods_body.parse()?);
            if methods_body.peek(Token![,]) {
                methods_body.parse::<Token![,]>()?;
            }
        }
        if body.peek(Token![,]) {
            body.parse::<Token![,]>()?;
        }
        if !body.is_empty() {
            return Err(body.error("unexpected service contract field"));
        }
        if methods.is_empty() {
            return Err(syn::Error::new(
                Span::call_site(),
                "a service contract must define at least one method",
            ));
        }

        Ok(Self {
            visibility,
            name,
            id,
            intercept,
            identity,
            merge,
            methods,
        })
    }
}

impl ServiceContractInput {
    fn expand(self) -> proc_macro2::TokenStream {
        let Self {
            visibility,
            name,
            id,
            intercept,
            identity,
            merge,
            methods,
        } = self;
        let provider = format_ident!("{}Provider", name);
        let client = format_ident!("{}Client", name);
        let endpoint = format_ident!("__{}Endpoint", name, span = Span::mixed_site());
        let method_lifetime = syn::Lifetime::new("'__linorun", Span::mixed_site());
        let call_argument = Ident::new("__linorun_call", Span::mixed_site());
        let endpoint_local = Ident::new("__linorun_endpoint", Span::mixed_site());
        let call_local = Ident::new("__linorun_call", Span::mixed_site());
        let adapter_local = Ident::new("__linorun_adapter", Span::mixed_site());

        let provider_methods = methods.iter().map(|method| {
            let attributes = &method.attributes;
            let method_name = &method.name;
            let arguments = &method.arguments;
            let output = &method.output;
            quote! {
                #(#attributes)*
                fn #method_name<#method_lifetime>(
                    &#method_lifetime self,
                    #call_argument: ::linorun_core::CallContext<#intercept>,
                    #arguments
                ) -> ::core::pin::Pin<::std::boxed::Box<
                    dyn ::core::future::Future<Output = #output> + ::core::marker::Send + #method_lifetime
                >>;
            }
        });
        let client_methods = methods.iter().map(|method| {
            let attributes = &method.attributes;
            let method_name = &method.name;
            let arguments = &method.arguments;
            let output = &method.output;
            let argument_names = method.arguments.iter().map(|argument| {
                let Pat::Ident(pattern) = &*argument.pat else {
                    unreachable!("argument patterns were validated while parsing")
                };
                &pattern.ident
            });
            quote! {
                #(#attributes)*
                pub async fn #method_name(&self, #arguments) -> #output {
                    let (#endpoint_local, #call_local) = self.reference.dispatch_parts();
                    let #adapter_local = #endpoint_local
                        .as_any()
                        .downcast_ref::<#endpoint>()
                        .expect("generated Linorun service adapter invariant");
                    #adapter_local
                        .provider
                        .#method_name(#call_local, #(#argument_names),*)
                        .await
                }
            }
        });

        quote! {
            #visibility struct #name;

            #visibility trait #provider: ::core::marker::Send + ::core::marker::Sync + 'static {
                #(#provider_methods)*
            }

            #[derive(Clone)]
            #visibility struct #client {
                reference: ::linorun_core::ServiceRef<#name>,
            }

            impl #client {
                #(#client_methods)*
            }

            struct #endpoint {
                provider: ::std::sync::Arc<dyn #provider>,
            }

            impl ::linorun_core::ErasedEndpoint for #endpoint {
                fn as_any(&self) -> &(dyn ::core::any::Any + ::core::marker::Send + ::core::marker::Sync) {
                    self
                }
            }

            impl ::linorun_core::ServiceContract for #name {
                const ID: &'static str = #id;

                type Provider = ::std::sync::Arc<dyn #provider>;
                type Intercept = #intercept;
                type Client = #client;

                fn identity_intercept() -> Self::Intercept {
                    #identity
                }

                fn merge_intercept(
                    base: &Self::Intercept,
                    overlay: &Self::Intercept,
                ) -> Self::Intercept {
                    (#merge)(base, overlay)
                }

                fn erase(provider: Self::Provider) -> ::std::sync::Arc<dyn ::linorun_core::ErasedEndpoint> {
                    ::std::sync::Arc::new(#endpoint { provider })
                }

                fn client(reference: ::linorun_core::ServiceRef<Self>) -> Self::Client {
                    #client { reference }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_interception_and_method_docs_generate_valid_items() {
        let contract: ServiceContractInput = syn::parse_quote! {
            pub contract Logger {
                id: "test/logger@1",
                methods: [
                    /// Records one message.
                    async fn info(message: String) -> Result<(), String>;
                ],
            }
        };
        let generated: syn::File = syn::parse2(contract.expand()).unwrap();
        let provider = generated
            .items
            .iter()
            .find_map(|item| match item {
                syn::Item::Trait(item) => Some(item),
                _ => None,
            })
            .unwrap();
        let syn::TraitItem::Fn(method) = &provider.items[0] else {
            panic!("provider method")
        };
        assert!(
            method
                .attrs
                .iter()
                .any(|attribute| attribute.path().is_ident("doc"))
        );
        let client = generated
            .items
            .iter()
            .find_map(|item| match item {
                syn::Item::Impl(item) if item.trait_.is_none() => Some(item),
                _ => None,
            })
            .unwrap();
        let syn::ImplItem::Fn(method) = &client.items[0] else {
            panic!("client method")
        };
        assert!(
            method
                .attrs
                .iter()
                .any(|attribute| attribute.path().is_ident("doc"))
        );
    }

    #[test]
    fn explicit_interception_still_requires_its_algebra() {
        assert!(syn::parse_str::<ServiceContractInput>(
            "contract Logger { id: \"a\", intercept: Metadata, methods: [async fn info() -> ();] }"
        ).is_err());
    }
}
