//! Stable field traversal, separate from the wire encoder and its size budgets.
use crate::shared::{core_path, reject_attributes, variant_tag};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{Data, DeriveInput, Fields, Index, Member, parse_quote};

const ATTRIBUTES: &[&str] = &["stable_hash", "wire"];

pub fn expand(input: &DeriveInput) -> syn::Result<TokenStream> {
    let tags = validate(input)?;
    let mut generics = input.generics.clone();
    let core = core_path()?;
    let name = &input.ident;
    let mut add_fields = |fields: &Fields| {
        for field in fields {
            let ty = &field.ty;
            generics
                .make_where_clause()
                .predicates
                .push(parse_quote!(#ty: #core::StableHash));
        }
    };
    let body = match &input.data {
        Data::Struct(data) => {
            add_fields(&data.fields);
            let members = data.fields.iter().enumerate().map(|(i, field)| {
                field.ident.as_ref().map_or_else(
                    || Member::Unnamed(Index::from(i)),
                    |name| Member::Named(name.clone()),
                )
            });
            quote!(#(hasher.field(&self.#members);)*)
        }
        Data::Enum(data) => {
            let mut arms = Vec::new();
            for (variant, tag) in data.variants.iter().zip(tags) {
                add_fields(&variant.fields);
                let name = &variant.ident;
                let bindings: Vec<_> = (0..variant.fields.len())
                    .map(|i| format_ident!("__hash_field_{i}"))
                    .collect();
                let pattern = match &variant.fields {
                    Fields::Unit => quote!(Self::#name),
                    Fields::Unnamed(_) => quote!(Self::#name(#(#bindings),*)),
                    Fields::Named(fields) => {
                        let members = fields.named.iter().map(|field| &field.ident);
                        quote!(Self::#name { #(#members: #bindings),* })
                    }
                };
                arms.push(quote!(#pattern => {
                    hasher.field(&#tag);
                    #(hasher.field(#bindings);)*
                }));
            }
            quote!(match self { #(#arms,)* })
        }
        Data::Union(_) => {
            return Err(syn::Error::new_spanned(
                input,
                "StableHash cannot be derived for unions",
            ));
        }
    };
    let (impl_generics, type_generics, where_clause) = generics.split_for_impl();
    Ok(quote! {
        impl #impl_generics #core::StableHash for #name #type_generics #where_clause {
            fn hash_into(&self, hasher: &mut #core::StateHasher) {
                #body
            }
        }
    })
}

/// Validate before resolving dependencies so schema diagnostics are self-contained.
fn validate(input: &DeriveInput) -> syn::Result<Vec<u8>> {
    reject_attributes(&input.attrs, ATTRIBUTES)?;
    let validate_fields = |fields: &Fields| -> syn::Result<()> {
        for field in fields {
            reject_attributes(&field.attrs, ATTRIBUTES)?;
        }
        Ok(())
    };
    match &input.data {
        Data::Struct(data) => {
            validate_fields(&data.fields)?;
            Ok(Vec::new())
        }
        Data::Enum(data) => {
            if data.variants.is_empty() {
                return Err(syn::Error::new_spanned(
                    input,
                    "StableHash requires at least one enum variant",
                ));
            }
            let mut seen = std::collections::HashSet::new();
            data.variants
                .iter()
                .map(|variant| {
                    let tag = variant_tag(variant, ATTRIBUTES)?;
                    if !seen.insert(tag) {
                        return Err(syn::Error::new_spanned(variant, "duplicate enum hash tag"));
                    }
                    validate_fields(&variant.fields)?;
                    Ok(tag)
                })
                .collect()
        }
        Data::Union(_) => Err(syn::Error::new_spanned(
            input,
            "StableHash cannot be derived for unions",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_ambiguous_and_unsupported_schemas() {
        let cases: [(DeriveInput, &str); 11] = [
            (
                parse_quote!(
                    enum E {}
                ),
                "at least one",
            ),
            (parse_quote!(union U { value: u64 }), "unions"),
            (
                parse_quote!(
                    enum E {
                        A,
                    }
                ),
                "every variant requires",
            ),
            (
                parse_quote!(
                    enum E {
                        #[stable_hash(tag = 256)]
                        A,
                    }
                ),
                "out of range",
            ),
            (
                parse_quote!(
                    enum E {
                        #[stable_hash(tag = 1)]
                        A = 1,
                    }
                ),
                "instead of Rust discriminants",
            ),
            (
                parse_quote!(
                    enum E {
                        #[stable_hash(tag = 1)]
                        A,
                        #[wire(tag = 1)]
                        B,
                    }
                ),
                "duplicate enum hash tag",
            ),
            (
                parse_quote!(
                    enum E {
                        #[stable_hash(tag = 1)]
                        #[wire(tag = 1)]
                        A,
                    }
                ),
                "exactly one tag",
            ),
            (
                parse_quote!(
                    enum E {
                        #[stable_hash(tag = 1, tag = 2)]
                        A,
                    }
                ),
                "duplicate wire tag attribute",
            ),
            (
                parse_quote!(
                    enum E {
                        #[stable_hash(skip)]
                        A,
                    }
                ),
                "expected tag",
            ),
            (
                parse_quote!(
                    #[stable_hash(tag = 1)]
                    struct S;
                ),
                "belong on enum variants",
            ),
            (
                parse_quote!(
                    struct S {
                        #[stable_hash(skip)]
                        value: u8,
                    }
                ),
                "belong on enum variants",
            ),
        ];
        for (input, expected) in cases {
            let error = expand(&input).unwrap_err().to_string();
            assert!(
                error.contains(expected),
                "expected {expected:?}, got {error:?}"
            );
        }
    }
}
