use proc_macro2::{Ident, Span, TokenStream};
use quote::quote;
use syn::{FnArg, GenericParam, ImplItem, ItemImpl, Pat, ReturnType, Type};

pub(crate) fn expand(mut implementation: ItemImpl) -> syn::Result<TokenStream> {
    if implementation.trait_.is_none()
        || implementation.unsafety.is_some()
        || implementation.defaultness.is_some()
        || !implementation.generics.params.is_empty()
        || implementation.generics.where_clause.is_some()
    {
        return Err(syn::Error::new_spanned(
            &implementation,
            "service_impl requires a non-generic provider trait impl",
        ));
    }
    for item in &mut implementation.items {
        let ImplItem::Fn(method) = item else {
            return Err(syn::Error::new_spanned(
                item,
                "service_impl supports async methods only",
            ));
        };
        let signature = &mut method.sig;
        if signature.asyncness.is_none()
            || signature.constness.is_some()
            || signature.unsafety.is_some()
            || signature.abi.is_some()
            || signature.variadic.is_some()
            || !signature.generics.params.is_empty()
            || signature.generics.where_clause.is_some()
        {
            return Err(syn::Error::new_spanned(
                signature,
                "service_impl methods must be non-generic async functions",
            ));
        }
        let Some(FnArg::Receiver(receiver)) = signature.inputs.first_mut() else {
            return Err(syn::Error::new_spanned(
                signature,
                "service methods require &self",
            ));
        };
        if receiver.reference.is_none()
            || receiver.mutability.is_some()
            || receiver.colon_token.is_some()
            || receiver
                .reference
                .as_ref()
                .is_some_and(|(_, lifetime)| lifetime.is_some())
        {
            return Err(syn::Error::new_spanned(
                receiver,
                "service methods require &self without an explicit lifetime",
            ));
        }
        let lifetime = syn::Lifetime::new("'__linorun", Span::mixed_site());
        receiver.reference = Some((syn::token::And::default(), Some(lifetime.clone())));
        receiver.ty = Box::new(syn::parse_quote!(&#lifetime Self));
        if signature.inputs.len() < 2 {
            return Err(syn::Error::new_spanned(
                signature,
                "service methods require a CallContext argument after &self",
            ));
        }
        for argument in signature.inputs.iter().skip(1) {
            let FnArg::Typed(argument) = argument else {
                return Err(syn::Error::new_spanned(
                    argument,
                    "unexpected service receiver",
                ));
            };
            let valid_pattern = matches!(&*argument.pat, Pat::Wild(_))
                || matches!(&*argument.pat, Pat::Ident(pattern) if pattern.by_ref.is_none() && pattern.subpat.is_none());
            if !valid_pattern || matches!(&*argument.ty, Type::Reference(_)) {
                return Err(syn::Error::new_spanned(
                    argument,
                    "service arguments must be owned values with identifier or wildcard patterns",
                ));
            }
        }
        let FnArg::Typed(call) = &signature.inputs[1] else {
            unreachable!()
        };
        if !matches!(&*call.ty, Type::Path(path) if path.path.segments.last().is_some_and(|segment| segment.ident == "CallContext"))
        {
            return Err(syn::Error::new_spanned(
                &call.ty,
                "the first service argument after &self must be CallContext<Intercept>",
            ));
        }
        let mut bindings = Vec::new();
        for (index, argument) in signature.inputs.iter_mut().skip(1).enumerate() {
            let FnArg::Typed(argument) = argument else {
                unreachable!()
            };
            let outer = Ident::new(&format!("__linorun_argument_{index}"), Span::mixed_site());
            let inner = if matches!(&*argument.pat, Pat::Wild(_)) {
                let held = Ident::new(&format!("__linorun_held_{index}"), Span::mixed_site());
                syn::parse_quote!(#held)
            } else {
                argument.pat.clone()
            };
            *argument.pat = syn::parse_quote!(#outer);
            // Native async functions retain every argument, including unused and wildcard values.
            bindings.push(quote!(let #inner = #outer;));
        }
        let output: Type = match &signature.output {
            ReturnType::Default => syn::parse_quote!(()),
            ReturnType::Type(_, output) => (**output).clone(),
        };
        signature.asyncness = None;
        signature
            .generics
            .params
            .push(GenericParam::Lifetime(syn::LifetimeParam::new(
                lifetime.clone(),
            )));
        signature.output = syn::parse_quote!(
            -> ::core::pin::Pin<::std::boxed::Box<
                dyn ::core::future::Future<Output = #output> + ::core::marker::Send + #lifetime
            >>
        );
        let body = &method.block;
        method.block = syn::parse_quote!({
            ::std::boxed::Box::pin(async move {
                #(#bindings)*
                #body
            })
        });
    }
    Ok(quote!(#implementation))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_provider_signatures_have_specific_errors() {
        for (source, expected) in [
            ("impl Service {}", "provider trait impl"),
            ("impl<T> ServiceProvider for Service<T> {}", "non-generic"),
            (
                "impl ServiceProvider for Service { fn call(&self, ctx: CallContext<()>) {} }",
                "async functions",
            ),
            (
                "impl ServiceProvider for Service { async fn call<T>(&self, ctx: CallContext<()>) {} }",
                "non-generic",
            ),
            (
                "impl ServiceProvider for Service { async fn call(&mut self, ctx: CallContext<()>) {} }",
                "require &self",
            ),
            (
                "impl ServiceProvider for Service { async fn call(&self) {} }",
                "CallContext argument",
            ),
            (
                "impl ServiceProvider for Service { async fn call(&self, value: String) {} }",
                "CallContext<Intercept>",
            ),
            (
                "impl ServiceProvider for Service { async fn call(&self, ctx: CallContext<()>, value: &str) {} }",
                "owned values",
            ),
        ] {
            let error = expand(syn::parse_str(source).unwrap()).unwrap_err();
            assert!(error.to_string().contains(expected), "{error}");
        }
    }
}
