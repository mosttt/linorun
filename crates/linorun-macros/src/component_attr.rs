use std::collections::HashSet;

use proc_macro2::{Span, TokenStream, TokenTree};
use quote::quote;
use syn::{
    FnArg, GenericArgument, Ident, ItemFn, LitStr, Pat, PathArguments, ReturnType, Token, Type,
    bracketed,
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
};

use crate::{ComponentDescriptorInput, RequirementInput};

pub(crate) struct ComponentArgs {
    id: LitStr,
    requirements: Punctuated<RequirementInput, Token![,]>,
    provides: Punctuated<Type, Token![,]>,
    identifiers: HashSet<String>,
}

impl Parse for ComponentArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut identifiers = HashSet::new();
        collect_identifiers(input.fork().parse()?, &mut identifiers);
        let mut id = None;
        let mut requirements = None;
        let mut provides = None;
        while !input.is_empty() {
            let field: Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            match field.to_string().as_str() {
                "id" if id.is_none() => id = Some(input.parse()?),
                "requires" if requirements.is_none() => {
                    let items;
                    bracketed!(items in input);
                    requirements = Some(Punctuated::parse_terminated(&items)?);
                }
                "provides" if provides.is_none() => {
                    let items;
                    bracketed!(items in input);
                    provides = Some(Punctuated::parse_terminated(&items)?);
                }
                _ => {
                    return Err(syn::Error::new_spanned(
                        field,
                        "expected a unique id, requires, or provides field",
                    ));
                }
            }
            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }
        Ok(Self {
            id: id.ok_or_else(|| input.error("component requires id = \"app/name@1\""))?,
            requirements: requirements.unwrap_or_default(),
            provides: provides.unwrap_or_default(),
            identifiers,
        })
    }
}

pub(crate) fn expand(args: ComponentArgs, function: ItemFn) -> syn::Result<TokenStream> {
    let signature = &function.sig;
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
            "component requires a non-generic async function",
        ));
    }
    if !(1..=2).contains(&signature.inputs.len()) {
        return Err(syn::Error::new_spanned(
            &signature.inputs,
            "component takes a ComponentContext and optionally Arc<Config>",
        ));
    }
    let mut arguments = signature.inputs.iter();
    let context = component_argument(arguments.next().expect("one argument required"))?;
    if !named_type(&context.ty, "ComponentContext") {
        return Err(syn::Error::new_spanned(
            &context.ty,
            "the first component argument must be ComponentContext",
        ));
    }
    let config_argument = arguments.next().map(component_argument).transpose()?;
    let config_type = match config_argument {
        Some(argument) => arc_inner_type(&argument.ty).ok_or_else(|| {
            syn::Error::new_spanned(&argument.ty, "the config argument must be Arc<Config>")
        })?,
        None => syn::parse_quote!(()),
    };
    let ReturnType::Type(_, output) = &signature.output else {
        return Err(syn::Error::new_spanned(
            signature,
            "component must return Result<(), ActivationFailure>",
        ));
    };
    if !activation_result(output) {
        return Err(syn::Error::new_spanned(
            output,
            "component must return Result<(), ActivationFailure>",
        ));
    }

    let mut identifiers = args.identifiers;
    collect_identifiers(quote!(#function), &mut identifiers);
    // Mixed-site hygiene does not prevent generated items from shadowing caller names.
    let mut helper = |base: &str| {
        let mut name = base.to_owned();
        let mut suffix = 0;
        while !identifiers.insert(name.clone()) {
            suffix += 1;
            name = format!("{base}_{suffix}");
        }
        Ident::new(&name, Span::mixed_site())
    };
    let component_type = helper("__LinorunComponent");
    let descriptor_name = helper("__LINORUN_DESCRIPTOR");
    let descriptor = ComponentDescriptorInput {
        visibility: syn::Visibility::Inherited,
        name: descriptor_name.clone(),
        config: config_type.clone(),
        id: args.id,
        requirements: args.requirements,
        provides: args.provides,
    }
    .expand();
    let context_local = helper("__linorun_context");
    let config_local = helper("__linorun_config");
    let setup_name = helper("__linorun_setup");
    let config_call = config_argument.map(|_| quote!(, #config_local));
    let attributes = &function.attrs;
    let visibility = &function.vis;
    let name = &signature.ident;
    let inputs = &signature.inputs;
    let output = &signature.output;
    let body = &function.block;
    Ok(quote! {
        #(#attributes)*
        #visibility fn #name() -> impl ::linorun_core::Component<Config = #config_type> {
            #descriptor
            struct #component_type;
            async fn #setup_name(#inputs) #output #body

            impl ::linorun_core::Component for #component_type {
                type Config = #config_type;

                fn descriptor(&self) -> &'static ::linorun_core::ComponentDescriptor {
                    &#descriptor_name
                }

                fn activate(
                    &self,
                    #context_local: ::linorun_core::ComponentContext,
                    #config_local: ::std::sync::Arc<Self::Config>,
                ) -> ::linorun_core::Activation {
                    ::linorun_core::Activation::run(#setup_name(#context_local #config_call))
                }
            }

            #component_type
        }
    })
}

fn collect_identifiers(tokens: TokenStream, identifiers: &mut HashSet<String>) {
    for token in tokens {
        match token {
            TokenTree::Ident(ident) => {
                identifiers.insert(ident.to_string().trim_start_matches("r#").to_owned());
            }
            TokenTree::Group(group) => collect_identifiers(group.stream(), identifiers),
            _ => {}
        }
    }
}

fn component_argument(argument: &FnArg) -> syn::Result<&syn::PatType> {
    if let FnArg::Typed(argument) = argument
        && let Pat::Ident(pattern) = &*argument.pat
        && pattern.by_ref.is_none()
        && pattern.subpat.is_none()
    {
        return Ok(argument);
    }
    Err(syn::Error::new_spanned(
        argument,
        "component arguments must use owned identifier patterns, without self",
    ))
}

fn named_type(ty: &Type, name: &str) -> bool {
    matches!(ty, Type::Path(path) if path.qself.is_none()
        && path.path.segments.last().is_some_and(|segment|
            segment.ident == name && matches!(segment.arguments, PathArguments::None)))
}

fn type_arguments<'a>(ty: &'a Type, name: &str) -> Option<&'a syn::AngleBracketedGenericArguments> {
    let Type::Path(path) = ty else { return None };
    let segment = path.path.segments.last()?;
    if path.qself.is_some() || segment.ident != name {
        return None;
    }
    let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return None;
    };
    Some(arguments)
}

fn arc_inner_type(ty: &Type) -> Option<Type> {
    let arguments = type_arguments(ty, "Arc")?;
    if arguments.args.len() != 1 {
        return None;
    }
    let GenericArgument::Type(config) = arguments.args.first()? else {
        return None;
    };
    Some(config.clone())
}

fn activation_result(ty: &Type) -> bool {
    let Some(arguments) = type_arguments(ty, "Result") else {
        return false;
    };
    if arguments.args.len() != 2 {
        return false;
    }
    let mut arguments = arguments.args.iter();
    matches!(arguments.next(), Some(GenericArgument::Type(Type::Tuple(tuple))) if tuple.elems.is_empty())
        && matches!(arguments.next(), Some(GenericArgument::Type(error)) if named_type(error, "ActivationFailure"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_component_signatures_have_specific_errors() {
        for (source, expected) in [
            (
                "fn worker(ctx: ComponentContext) -> Result<(), ActivationFailure> { Ok(()) }",
                "non-generic async",
            ),
            (
                "async fn worker<T>(ctx: ComponentContext) -> Result<(), ActivationFailure> { Ok(()) }",
                "non-generic async",
            ),
            (
                "async fn worker() -> Result<(), ActivationFailure> { Ok(()) }",
                "takes a ComponentContext",
            ),
            (
                "async fn worker(ctx: &ComponentContext) -> Result<(), ActivationFailure> { Ok(()) }",
                "first component argument",
            ),
            (
                "async fn worker(ctx: ComponentContext, config: Config) -> Result<(), ActivationFailure> { Ok(()) }",
                "Arc<Config>",
            ),
            (
                "async fn worker(ctx: ComponentContext) -> () {}",
                "Result<(), ActivationFailure>",
            ),
            (
                "async fn worker((ctx,): (ComponentContext,)) -> Result<(), ActivationFailure> { Ok(()) }",
                "identifier patterns",
            ),
        ] {
            let args = syn::parse_str("id = \"test/worker@1\"").unwrap();
            let error = expand(args, syn::parse_str(source).unwrap()).unwrap_err();
            assert!(error.to_string().contains(expected), "{error}");
        }
    }

    #[test]
    fn descriptor_fields_reject_duplicates_and_require_identity() {
        for source in [
            "",
            "requires = []",
            "id = \"a\", id = \"b\"",
            "id = \"a\", unknown = []",
        ] {
            assert!(syn::parse_str::<ComponentArgs>(source).is_err(), "{source}");
        }
        assert!(
            syn::parse_str::<ComponentArgs>(
                "provides = [Logger], id = \"test/worker@1\", requires = [Logger => ()],"
            )
            .is_ok()
        );
    }
}
