//! Dependency resolution and explicit enum tag validation shared by derives.
use proc_macro_crate::{FoundCrate, crate_name};
use proc_macro2::Span;
use syn::{Path, parse_quote};

pub fn core_path() -> syn::Result<Path> {
    match crate_name("synctick") {
        // synctick declares an alias to itself; this also works in its
        // integration tests and examples, where `crate` means the test/example.
        Ok(FoundCrate::Itself) => Ok(parse_quote!(::synctick)),
        Ok(FoundCrate::Name(name)) => {
            let ident = syn::Ident::new(&name, Span::call_site());
            Ok(parse_quote!(::#ident))
        }
        Err(error) => Err(syn::Error::new(Span::call_site(), error)),
    }
}

pub fn reject_attributes(attrs: &[syn::Attribute], names: &[&str]) -> syn::Result<()> {
    for attr in attrs {
        if names.iter().any(|name| attr.path().is_ident(name)) {
            return Err(syn::Error::new_spanned(
                attr,
                "tag attributes belong on enum variants",
            ));
        }
    }
    Ok(())
}

pub fn variant_tag(variant: &syn::Variant, names: &[&str]) -> syn::Result<u8> {
    if let Some((_, value)) = &variant.discriminant {
        return Err(syn::Error::new_spanned(
            value,
            format!("use #[{}(tag = N)] instead of Rust discriminants", names[0]),
        ));
    }
    let mut tag = None;
    let mut attribute_seen = false;
    for attr in &variant.attrs {
        if names.iter().any(|name| attr.path().is_ident(name)) {
            if attribute_seen {
                return Err(syn::Error::new_spanned(
                    attr,
                    "duplicate wire tag attribute; use exactly one tag attribute",
                ));
            }
            attribute_seen = true;
            attr.parse_nested_meta(|meta| {
                if !meta.path.is_ident("tag") {
                    return Err(meta.error("expected tag = N"));
                }
                if tag.is_some() {
                    return Err(meta.error("duplicate wire tag attribute"));
                }
                let literal: syn::LitInt = meta.value()?.parse()?;
                tag = Some(literal.base10_parse::<u8>().map_err(|_| {
                    syn::Error::new_spanned(&literal, "wire tag out of range; expected 0..=255")
                })?);
                Ok(())
            })?;
        }
    }
    tag.ok_or_else(|| {
        syn::Error::new_spanned(
            variant,
            format!(
                "every variant requires #[{}(tag = N)] with a tag in 0..=255",
                names[0]
            ),
        )
    })
}
