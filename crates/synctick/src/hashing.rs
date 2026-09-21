//! Stable state fingerprints for a known game schema, independent of the wire codec.
//!
//! Hashing streams bytes without allocating or imposing payload limits. Games own
//! state selection and ordering: sort ECS entities by logical ID, and exclude
//! presentation effects and recomputable caches. This is not cryptographic hashing.
//!
//! Integers use their fixed-width little-endian representations, booleans use 0/1,
//! and floats use exact IEEE bits (including signed zero and NaN payloads). Strings
//! have a u64 byte length followed by UTF-8; sequences have a u64 element count
//! followed by elements. Arrays, slices and vectors share a representation. Structs
//! and tuples concatenate fields in declaration order. Unit contributes no bytes.
//! Options use a one-byte 0/1 tag followed by the present value. No type names or
//! schema identifiers are hashed. Lengths fit u64 on supported Rust targets.
//!
//! Changing field order, enum tags, representations, or the algorithm changes the
//! fingerprint contract: bump the game's compatibility version. Do not substitute
//! `std::hash::Hash`, whose representations are not guaranteed portable or stable.

pub use synctick_derive::StableHash;

/// Feed all authoritative fields into a stable fingerprint.
///
/// Derive for structs and enums; every field participates. Enum variants require
/// explicit unique byte tags, using `#[stable_hash(tag = N)]` or `#[wire(tag = N)]`
/// (not both). The latter shares a tag with `Wire` without depending on encoding.
/// Generic bounds apply to field types. There is no field-skipping attribute.
///
/// ```
/// use synctick::{StableHash, stable_hash};
/// #[derive(StableHash)]
/// struct State { tick: u64, balances: Vec<u64> }
/// let state = State { tick: 1, balances: vec![10, 20] };
/// assert_eq!(stable_hash(&state), stable_hash(&(1u64, &[10u64, 20])));
/// ```
///
/// Pointer-sized integers and unordered collections deliberately lack implementations:
///
/// ```compile_fail
/// synctick::stable_hash(&1usize);
/// ```
/// ```compile_fail
/// synctick::stable_hash(&std::collections::HashMap::<u64, u64>::new());
/// ```
/// ```compile_fail
/// #[derive(synctick::StableHash)]
/// enum MissingTag { Value }
/// ```
pub trait StableHash {
    /// Append this value's canonical representation to the stream.
    fn hash_into(&self, hasher: &mut StateHasher);
}

/// Streaming FNV-1a 64-bit fingerprint with fixed standard offset basis and prime.
///
/// Built-in implementations allocate nothing. Custom implementations must preserve
/// stable ordering and frame variable-length data explicitly.
pub struct StateHasher(u64);

impl Default for StateHasher {
    fn default() -> Self {
        Self::new()
    }
}

impl StateHasher {
    /// Begin an empty fingerprint with the standard FNV-1a offset basis.
    #[must_use]
    pub const fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    /// Append a typed field using its canonical representation.
    pub fn field<T: StableHash + ?Sized>(&mut self, value: &T) {
        value.hash_into(self);
    }

    /// Append raw bytes without framing. Call boundaries do not affect the hash.
    ///
    /// For variable-length data, hash a fixed-width length first, or use `field`
    /// with a string or slice. Unframed concatenation can hide field boundaries.
    pub fn write_bytes(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 = (self.0 ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
        }
    }

    /// Finish the fingerprint, consuming the builder.
    #[must_use]
    pub const fn finish(self) -> u64 {
        self.0
    }
}

/// Fingerprint a value's canonical representation using fixed FNV-1a 64-bit.
#[must_use]
pub fn stable_hash<T: StableHash + ?Sized>(value: &T) -> u64 {
    let mut hasher = StateHasher::new();
    hasher.field(value);
    hasher.finish()
}

macro_rules! integers {
    ($($ty:ty),* $(,)?) => {$ (
        impl StableHash for $ty {
            fn hash_into(&self, hasher: &mut StateHasher) {
                hasher.write_bytes(&self.to_le_bytes());
            }
        }
    )*};
}
integers!(u8, u16, u32, u64, u128, i8, i16, i32, i64, i128);

impl StableHash for bool {
    fn hash_into(&self, hasher: &mut StateHasher) {
        hasher.field(&u8::from(*self));
    }
}
impl StableHash for f32 {
    fn hash_into(&self, hasher: &mut StateHasher) {
        hasher.field(&self.to_bits());
    }
}
impl StableHash for f64 {
    fn hash_into(&self, hasher: &mut StateHasher) {
        hasher.field(&self.to_bits());
    }
}
impl StableHash for () {
    fn hash_into(&self, _: &mut StateHasher) {}
}
impl<T: StableHash + ?Sized> StableHash for &T {
    fn hash_into(&self, hasher: &mut StateHasher) {
        T::hash_into(self, hasher);
    }
}
impl<T: StableHash + ?Sized> StableHash for &mut T {
    fn hash_into(&self, hasher: &mut StateHasher) {
        T::hash_into(self, hasher);
    }
}
impl StableHash for str {
    fn hash_into(&self, hasher: &mut StateHasher) {
        hasher.field(&(self.len() as u64));
        hasher.write_bytes(self.as_bytes());
    }
}
impl StableHash for String {
    fn hash_into(&self, hasher: &mut StateHasher) {
        self.as_str().hash_into(hasher);
    }
}
impl<T: StableHash> StableHash for [T] {
    fn hash_into(&self, hasher: &mut StateHasher) {
        hasher.field(&(self.len() as u64));
        for value in self {
            hasher.field(value);
        }
    }
}
impl<T: StableHash> StableHash for Vec<T> {
    fn hash_into(&self, hasher: &mut StateHasher) {
        self.as_slice().hash_into(hasher);
    }
}
impl<T: StableHash, const N: usize> StableHash for [T; N] {
    fn hash_into(&self, hasher: &mut StateHasher) {
        self.as_slice().hash_into(hasher);
    }
}
impl<T: StableHash> StableHash for Option<T> {
    fn hash_into(&self, hasher: &mut StateHasher) {
        match self {
            None => hasher.field(&0u8),
            Some(value) => {
                hasher.field(&1u8);
                hasher.field(value);
            }
        }
    }
}

macro_rules! tuples {
    ($(($($ty:ident : $index:tt),+)),+ $(,)?) => {$ (
        impl<$($ty: StableHash),+> StableHash for ($($ty,)+) {
            fn hash_into(&self, hasher: &mut StateHasher) {
                $(hasher.field(&self.$index);)+
            }
        }
    )+};
}
tuples!(
    (A:0), (A:0, B:1), (A:0, B:1, C:2), (A:0, B:1, C:2, D:3),
    (A:0, B:1, C:2, D:3, E:4), (A:0, B:1, C:2, D:3, E:4, F:5),
    (A:0, B:1, C:2, D:3, E:4, F:5, G:6),
    (A:0, B:1, C:2, D:3, E:4, F:5, G:6, H:7),
);
