//! Bounded game payloads. Implement `Wire` with the same field order in all
//! three operations. Validation must traverse the full shape without allocating.
//!
//! Use these helpers for collections; arbitrary Serde decoding is not a substitute.
use std::mem::size_of;

pub use synctick_derive::Wire;

pub const MAX_PAYLOAD_BYTES: usize = 64 * 1024;
const MAX_ALLOCATION_BYTES: usize = 4 * 1024 * 1024;
const MAX_DEPTH: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("codec: {0}")]
pub struct CodecError(pub &'static str);
pub type Result<T> = std::result::Result<T, CodecError>;

/// A game-owned wire type.
///
/// Derive for nonempty named or tuple structs. Each field must implement
/// `Wire`; generic field bounds are generated automatically. All passes use
/// declaration order, with no struct tags or padding. Reordering fields changes
/// the wire format. Enums require a unique `#[wire(tag = N)]` on each variant,
/// with a literal byte tag (0..=255). Unit, tuple and named variants are supported.
/// The tag precedes fields in declaration order; variant order does not affect
/// the format. Rust discriminants are rejected to avoid conflicting tag schemes.
/// Unknown tags are rejected during both validation and decoding.
/// Empty structs are rejected; a unit field encodes an explicit one-byte marker.
///
/// ```
/// use synctick::{Wire, codec};
///
/// #[derive(Wire)]
/// struct Setup { seed: u64, names: Vec<String> }
/// let setup = Setup { seed: 42, names: vec!["Earth".into()] };
/// let bytes = codec::encode(&setup).unwrap();
/// let restored: Setup = codec::decode(&bytes).unwrap();
/// assert_eq!(restored.names, setup.names);
/// ```
///
/// ```
/// #[derive(synctick::Wire)]
/// enum Command {
///     #[wire(tag = 0)]
///     Wait,
///     #[wire(tag = 7)]
///     Rotate { cell: u32 },
/// }
/// assert_eq!(synctick::codec::encode(&Command::Rotate { cell: 1 }).unwrap(),
///            [7, 1, 0, 0, 0]);
/// ```
///
/// ```compile_fail
/// #[derive(synctick::Wire)]
/// enum Command { Spawn }
/// ```
///
/// ```compile_fail
/// #[derive(synctick::Wire)]
/// struct Empty;
/// ```
///
/// Implementations must be deterministic and use the
/// provided readers for all untrusted lengths. `MIN_SIZE` must be a positive
/// lower bound on the encoded size of one value.
///
/// Custom implementations are
/// trusted game code; the framework cannot sandbox arbitrary Rust allocations.
pub trait Wire: Sized {
    const MIN_SIZE: usize;
    /// # Errors
    /// Returns malformed or over-budget payload errors, without allocating.
    fn validate(reader: &mut Decoder<'_>) -> Result<()>;
    /// # Errors
    /// Returns decoding or allocation errors. Called only after validation.
    fn decode(reader: &mut Decoder<'_>) -> Result<Self>;
    /// # Errors
    /// Returns an error if the encoded payload exceeds its budget.
    fn encode(&self, writer: &mut Encoder) -> Result<()>;
}

#[derive(Default)]
pub struct Encoder(Vec<u8>);
impl Encoder {
    /// # Errors
    /// Rejects payloads exceeding the byte budget and allocation failures.
    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.len() > MAX_PAYLOAD_BYTES.saturating_sub(self.0.len()) {
            return Err(CodecError("payload byte budget exceeded"));
        }
        self.0
            .try_reserve(bytes.len())
            .map_err(|_| CodecError("allocation failed"))?;
        self.0.extend_from_slice(bytes);
        Ok(())
    }
}

pub struct Decoder<'a> {
    remaining: &'a [u8],
    allocation: usize,
    depth: usize,
}
impl<'a> Decoder<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self {
            remaining: bytes,
            allocation: 0,
            depth: 0,
        }
    }
    /// # Errors
    /// Rejects truncated fields.
    pub const fn take(&mut self, count: usize) -> Result<&'a [u8]> {
        if count > self.remaining.len() {
            return Err(CodecError("truncated field"));
        }
        let (bytes, remaining) = self.remaining.split_at(count);
        self.remaining = remaining;
        Ok(bytes)
    }
    fn count<T: Wire>(&mut self) -> Result<usize> {
        let count = usize::try_from(u32::decode(self)?)
            .map_err(|_| CodecError("platform length overflow"))?;
        if T::MIN_SIZE == 0 || count > self.remaining.len() / T::MIN_SIZE {
            return Err(CodecError("collection length exceeds remaining payload"));
        }
        let allocation = count
            .checked_mul(size_of::<T>().max(1))
            .ok_or(CodecError("allocation overflow"))?;
        self.allocation = self
            .allocation
            .checked_add(allocation)
            .ok_or(CodecError("allocation overflow"))?;
        if self.allocation > MAX_ALLOCATION_BYTES {
            return Err(CodecError("allocation budget exceeded"));
        }
        Ok(count)
    }
    const fn enter(&mut self) -> Result<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(CodecError("nesting budget exceeded"));
        }
        Ok(())
    }
    const fn finish(self) -> Result<()> {
        if self.remaining.is_empty() {
            Ok(())
        } else {
            Err(CodecError("trailing bytes"))
        }
    }
}

/// Validate without constructing the game value.
/// # Errors
/// Rejects oversized, malformed, truncated and trailing data.
pub fn validate<T: Wire>(bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_PAYLOAD_BYTES {
        return Err(CodecError("payload byte budget exceeded"));
    }
    let mut reader = Decoder::new(bytes);
    T::validate(&mut reader)?;
    reader.finish()
}
/// # Errors
/// Rejects invalid data before materializing any game collections.
pub fn decode<T: Wire>(bytes: &[u8]) -> Result<T> {
    validate::<T>(bytes)?;
    let mut reader = Decoder::new(bytes);
    let value = T::decode(&mut reader)?;
    reader.finish()?;
    Ok(value)
}
/// # Errors
/// Rejects oversized payloads or a codec that emits an invalid shape.
pub fn encode<T: Wire>(value: &T) -> Result<Vec<u8>> {
    let mut writer = Encoder::default();
    value.encode(&mut writer)?;
    validate::<T>(&writer.0)?;
    Ok(writer.0)
}

macro_rules! scalar {
    ($($ty:ty),*) => { $(
        impl Wire for $ty {
            const MIN_SIZE: usize = size_of::<Self>();
            fn validate(reader: &mut Decoder<'_>) -> Result<()> { reader.take(Self::MIN_SIZE)?; Ok(()) }
            fn decode(reader: &mut Decoder<'_>) -> Result<Self> {
                let mut bytes = [0; size_of::<Self>()];
                bytes.copy_from_slice(reader.take(Self::MIN_SIZE)?);
                Ok(Self::from_le_bytes(bytes))
            }
            fn encode(&self, writer: &mut Encoder) -> Result<()> { writer.write(&self.to_le_bytes()) }
        }
    )* };
}
scalar!(u8, u16, u32, u64, i32, i64);
impl Wire for () {
    const MIN_SIZE: usize = 1;
    fn validate(reader: &mut Decoder<'_>) -> Result<()> {
        if u8::decode(reader)? != 0 {
            return Err(CodecError("invalid unit"));
        }
        Ok(())
    }
    fn decode(reader: &mut Decoder<'_>) -> Result<Self> {
        Self::validate(reader)
    }
    fn encode(&self, writer: &mut Encoder) -> Result<()> {
        0u8.encode(writer)
    }
}
impl<T: Wire> Wire for Vec<T> {
    const MIN_SIZE: usize = 4;
    fn validate(reader: &mut Decoder<'_>) -> Result<()> {
        reader.enter()?;
        let count = reader.count::<T>()?;
        for _ in 0..count {
            T::validate(reader)?;
        }
        reader.depth -= 1;
        Ok(())
    }
    fn decode(reader: &mut Decoder<'_>) -> Result<Self> {
        reader.enter()?;
        let count = reader.count::<T>()?;
        let mut values = Self::new();
        values
            .try_reserve_exact(count)
            .map_err(|_| CodecError("allocation failed"))?;
        for _ in 0..count {
            values.push(T::decode(reader)?);
        }
        reader.depth -= 1;
        Ok(values)
    }
    fn encode(&self, writer: &mut Encoder) -> Result<()> {
        u32::try_from(self.len())
            .map_err(|_| CodecError("collection too long"))?
            .encode(writer)?;
        for value in self {
            value.encode(writer)?;
        }
        Ok(())
    }
}
impl Wire for String {
    const MIN_SIZE: usize = 4;
    fn validate(reader: &mut Decoder<'_>) -> Result<()> {
        let count = reader.count::<u8>()?;
        std::str::from_utf8(reader.take(count)?).map_err(|_| CodecError("invalid UTF-8"))?;
        Ok(())
    }
    fn decode(reader: &mut Decoder<'_>) -> Result<Self> {
        let count = reader.count::<u8>()?;
        let text =
            std::str::from_utf8(reader.take(count)?).map_err(|_| CodecError("invalid UTF-8"))?;
        let mut value = Self::new();
        value
            .try_reserve_exact(count)
            .map_err(|_| CodecError("allocation failed"))?;
        value.push_str(text);
        Ok(value)
    }
    fn encode(&self, writer: &mut Encoder) -> Result<()> {
        u32::try_from(self.len())
            .map_err(|_| CodecError("string too long"))?
            .encode(writer)?;
        writer.write(self.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_works_inside_core_with_an_explicit_unit_marker() {
        #[derive(Debug, PartialEq, Eq, Wire)]
        struct Marker(());

        assert_eq!(Marker::MIN_SIZE, 1);
        assert_eq!(encode(&Marker(())).unwrap(), [0]);
        assert_eq!(decode::<Marker>(&[0]).unwrap(), Marker(()));
        assert!(decode::<Marker>(&[1]).is_err());
    }

    #[test]
    fn nested_payloads_validate_before_decode() {
        let value = vec![vec!["hello".to_owned(), "world".to_owned()]];
        let bytes = encode(&value).unwrap();
        assert_eq!(decode::<Vec<Vec<String>>>(&bytes).unwrap(), value);
        for end in 0..bytes.len() {
            assert!(decode::<Vec<Vec<String>>>(&bytes[..end]).is_err());
        }
        let mut trailing = bytes;
        trailing.push(0);
        assert!(decode::<Vec<Vec<String>>>(&trailing).is_err());
        let mut hostile = vec![1, 0, 0, 0];
        hostile.extend(u32::MAX.to_le_bytes());
        assert!(decode::<Vec<Vec<String>>>(&hostile).is_err());
        assert!(encode(&vec![0u8; MAX_PAYLOAD_BYTES]).is_err());
        assert!(decode::<String>(&[1, 0, 0, 0, 255]).is_err());
    }
}
