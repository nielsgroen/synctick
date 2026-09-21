//! Derives for stable session payloads and deterministic state fingerprints.
use proc_macro::TokenStream;
use syn::{DeriveInput, parse_macro_input};

mod hashing;
mod shared;
mod wire;

/// Generate bounded codec passes for structs and explicitly tagged enums.
#[proc_macro_derive(Wire, attributes(wire))]
pub fn derive_wire(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    wire::expand(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Hash fields in declaration order, with explicit byte tags for enums.
///
/// Fields marked `#[stable_hash(skip)]` contribute no bytes and need no hash bound.
/// Only skip recomputable caches or presentation state, never authoritative state.
/// Tags may use `stable_hash` or reuse a `wire` tag attribute, but not both.
#[proc_macro_derive(StableHash, attributes(stable_hash, wire))]
pub fn derive_stable_hash(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    hashing::expand(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
