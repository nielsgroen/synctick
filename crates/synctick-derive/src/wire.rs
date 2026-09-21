//! Bounded wire validation, decoding and encoding generation.
use crate::shared::{core_path, reject_attributes, variant_tag};
use quote::{format_ident, quote};
use syn::{Data, DeriveInput, Fields, Index, Member, parse_quote};

pub fn expand(input: &DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    reject_attributes(&input.attrs, &["wire"])?;
    match &input.data {
        Data::Enum(data) => expand_enum(input, data),
        Data::Struct(data) => {
            for field in &data.fields {
                reject_attributes(&field.attrs, &["wire"])?;
            }
            expand_struct(input)
        }
        Data::Union(_) => Err(syn::Error::new_spanned(
            &input.ident,
            "Wire cannot be derived for unions; implement Wire manually",
        )),
    }
}

fn expand_enum(input: &DeriveInput, data: &syn::DataEnum) -> syn::Result<proc_macro2::TokenStream> {
    if data.variants.is_empty() {
        return Err(syn::Error::new_spanned(
            input,
            "Wire requires at least one enum variant",
        ));
    }
    let mut seen = std::collections::HashSet::new();
    let mut tags = Vec::new();
    for variant in &data.variants {
        let tag = variant_tag(variant, &["wire"])?;
        if !seen.insert(tag) {
            return Err(syn::Error::new_spanned(variant, "duplicate wire tag"));
        }
        for field in &variant.fields {
            reject_attributes(&field.attrs, &["wire"])?;
        }
        tags.push(tag);
    }
    let core = core_path()?;
    let name = &input.ident;
    let mut generics = input.generics.clone();
    let mut validation = Vec::new();
    let mut decoding = Vec::new();
    let mut encoding = Vec::new();
    let mut sizes = Vec::new();
    for (variant, tag) in data.variants.iter().zip(tags) {
        let variant_name = &variant.ident;
        let types: Vec<_> = variant.fields.iter().map(|field| &field.ty).collect();
        for ty in &types {
            generics
                .make_where_clause()
                .predicates
                .push(parse_quote!(#ty: #core::codec::Wire));
        }
        let bindings: Vec<_> = (0..types.len())
            .map(|i| format_ident!("__wire_field_{i}"))
            .collect();
        let values = types
            .iter()
            .map(|ty| quote!(<#ty as #core::codec::Wire>::decode(reader)?));
        let (pattern, construct) = match &variant.fields {
            Fields::Unit => (quote!(Self::#variant_name), quote!(Self::#variant_name)),
            Fields::Unnamed(_) => (
                quote!(Self::#variant_name(#(#bindings),*)),
                quote!(Self::#variant_name(#(#values),*)),
            ),
            Fields::Named(fields) => {
                let members: Vec<_> = fields.named.iter().map(|field| &field.ident).collect();
                (
                    quote!(Self::#variant_name { #(#members: #bindings),* }),
                    quote!(Self::#variant_name { #(#members: #values),* }),
                )
            }
        };
        validation.push(quote!(#tag => {
            #(<#types as #core::codec::Wire>::validate(reader)?;)*
            ::core::result::Result::Ok(())
        }));
        decoding.push(quote!(#tag => ::core::result::Result::Ok(#construct)));
        encoding.push(quote!(#pattern => {
            <u8 as #core::codec::Wire>::encode(&#tag, writer)?;
            #(<#types as #core::codec::Wire>::encode(#bindings, writer)?;)*
            ::core::result::Result::Ok(())
        }));
        sizes.push(quote!(1usize #(+ <#types as #core::codec::Wire>::MIN_SIZE)*));
    }
    let (impl_generics, type_generics, where_clause) = generics.split_for_impl();
    // A complete byte-tag space is already exhaustive; emitting a wildcard
    // would produce an unreachable-pattern warning in the consuming crate.
    let unknown_tag = (data.variants.len() < 256).then(|| {
        quote! {
            _ => ::core::result::Result::Err(#core::codec::CodecError("unknown enum tag")),
        }
    });
    Ok(quote! {
        impl #impl_generics #core::codec::Wire for #name #type_generics #where_clause {
            const MIN_SIZE: usize = {
                let mut smallest = usize::MAX;
                #(if #sizes < smallest { smallest = #sizes; })*
                smallest
            };
            fn validate(reader: &mut #core::codec::Decoder<'_>) -> #core::codec::Result<()> {
                match <u8 as #core::codec::Wire>::decode(reader)? {
                    #(#validation,)*
                    #unknown_tag
                }
            }
            fn decode(reader: &mut #core::codec::Decoder<'_>) -> #core::codec::Result<Self> {
                match <u8 as #core::codec::Wire>::decode(reader)? {
                    #(#decoding,)*
                    #unknown_tag
                }
            }
            fn encode(&self, writer: &mut #core::codec::Encoder) -> #core::codec::Result<()> {
                match self { #(#encoding,)* }
            }
        }
    })
}

fn expand_struct(input: &DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(&input.ident, "expected a struct"));
    };
    if data.fields.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "Wire requires a nonempty struct; add a unit field to encode an explicit marker",
        ));
    }
    let core = core_path()?;
    let name = &input.ident;
    let types: Vec<_> = data.fields.iter().map(|field| &field.ty).collect();
    let members: Vec<_> = data
        .fields
        .iter()
        .enumerate()
        .map(|(index, field)| {
            field.ident.as_ref().map_or_else(
                || Member::Unnamed(Index::from(index)),
                |ident| Member::Named(ident.clone()),
            )
        })
        .collect();
    let mut generics = input.generics.clone();
    for ty in &types {
        generics
            .make_where_clause()
            .predicates
            .push(parse_quote!(#ty: #core::codec::Wire));
    }
    let (impl_generics, type_generics, where_clause) = generics.split_for_impl();
    let decoded = types
        .iter()
        .map(|ty| quote!(<#ty as #core::codec::Wire>::decode(reader)?));
    let construct = match &data.fields {
        Fields::Named(_) => quote!(Self { #(#members: #decoded),* }),
        Fields::Unnamed(_) => quote!(Self(#(#decoded),*)),
        Fields::Unit => unreachable!("empty structs rejected above"),
    };
    Ok(quote! {
        impl #impl_generics #core::codec::Wire for #name #type_generics #where_clause {
            const MIN_SIZE: usize = 0 #(+ <#types as #core::codec::Wire>::MIN_SIZE)*;
            fn validate(reader: &mut #core::codec::Decoder<'_>) -> #core::codec::Result<()> {
                #(<#types as #core::codec::Wire>::validate(reader)?;)*
                ::core::result::Result::Ok(())
            }
            fn decode(reader: &mut #core::codec::Decoder<'_>) -> #core::codec::Result<Self> {
                ::core::result::Result::Ok(#construct)
            }
            fn encode(&self, writer: &mut #core::codec::Encoder) -> #core::codec::Result<()> {
                #(<#types as #core::codec::Wire>::encode(&self.#members, writer)?;)*
                ::core::result::Result::Ok(())
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_enum_schemas_are_rejected() {
        let cases: [(DeriveInput, &str); 9] = [
            (
                parse_quote!(
                    enum Empty {}
                ),
                "at least one",
            ),
            (
                parse_quote!(
                    enum Missing {
                        A,
                    }
                ),
                "every variant requires",
            ),
            (
                parse_quote!(
                    enum Duplicate {
                        #[wire(tag = 1)]
                        A,
                        #[wire(tag = 1)]
                        B,
                    }
                ),
                "duplicate wire tag",
            ),
            (
                parse_quote!(
                    enum DuplicateAttribute {
                        #[wire(tag = 1, tag = 2)]
                        A,
                    }
                ),
                "duplicate wire tag attribute",
            ),
            (
                parse_quote!(
                    enum Overflow {
                        #[wire(tag = 256)]
                        A,
                    }
                ),
                "out of range",
            ),
            (
                parse_quote!(
                    enum Unknown {
                        #[wire(value = 0)]
                        A,
                    }
                ),
                "expected tag",
            ),
            (
                parse_quote!(
                    enum Discriminant {
                        #[wire(tag = 1)]
                        A = 1,
                    }
                ),
                "instead of Rust discriminants",
            ),
            (
                parse_quote!(
                    #[wire(tag = 1)]
                    enum Misplaced {
                        A,
                    }
                ),
                "belong on enum variants",
            ),
            (
                parse_quote!(
                    enum FieldAttribute {
                        #[wire(tag = 1)]
                        A(#[wire(tag = 2)] u8),
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

    #[test]
    fn unsupported_shapes_have_actionable_errors() {
        let union = parse_quote!(union Value { word: u64 });
        assert!(
            expand(&union)
                .unwrap_err()
                .to_string()
                .contains("implement Wire manually")
        );
        for input in [
            parse_quote!(
                struct Empty;
            ),
            parse_quote!(
                struct Empty {}
            ),
            parse_quote!(
                struct Empty();
            ),
        ] {
            assert!(
                expand(&input)
                    .unwrap_err()
                    .to_string()
                    .contains("nonempty struct")
            );
        }
    }
}
