//! Golden canonical representations and derive ergonomics, through the public API.
use synctick::{StableHash, StateHasher, Wire, stable_hash};

// Reference FNV over explicitly specified bytes, without typed hashing or derives.
fn reference(bytes: &[u8]) -> u64 {
    bytes.iter().fold(14_695_981_039_346_656_037, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(1_099_511_628_211)
    })
}

#[test]
fn scalar_string_float_and_nested_golden_fingerprints() {
    assert_eq!(stable_hash(&()), 0xcbf2_9ce4_8422_2325);
    assert_eq!(reference(b"foobar"), 0x8594_4171_f739_67e8);
    assert_eq!(
        stable_hash(&(true, false, 513u16, -2i16)),
        0x8c52_c3cf_8a12_928c
    );
    assert_eq!(stable_hash(&-0.0f64), 0xa8c7_7832_2819_6045);
    assert_eq!(stable_hash("é"), 0x9da0_30d3_29b9_ed5f);
    let nested = vec![vec![513u16], vec![]];
    assert_eq!(stable_hash(&nested), 0xc942_1f6e_86f3_ace5);
    assert_eq!(
        stable_hash(&nested),
        reference(&[
            2, 0, 0, 0, 0, 0, 0, 0, // outer count
            1, 0, 0, 0, 0, 0, 0, 0, 1, 2, // first sequence
            0, 0, 0, 0, 0, 0, 0, 0, // second sequence
        ])
    );
    macro_rules! check_integer {
        ($($ty:ty),*) => {$(
            assert_eq!(stable_hash(&<$ty>::MIN), reference(&<$ty>::MIN.to_le_bytes()));
            assert_eq!(stable_hash(&<$ty>::MAX), reference(&<$ty>::MAX.to_le_bytes()));
        )*};
    }
    check_integer!(u8, u16, u32, u64, u128, i8, i16, i32, i64, i128);
}

#[test]
fn float_bits_are_not_normalized() {
    assert_ne!(stable_hash(&0.0f32), stable_hash(&-0.0f32));
    assert_ne!(stable_hash(&0.0f64), stable_hash(&-0.0f64));
    for bits in [0x7fc0_0001u32, 0x7fc0_0002, 0xff80_0000] {
        assert_eq!(
            stable_hash(&f32::from_bits(bits)),
            reference(&bits.to_le_bytes())
        );
    }
    for bits in [
        0x7ff8_0000_0000_0001u64,
        0x7ff8_0000_0000_0002,
        0xfff0_0000_0000_0000,
    ] {
        assert_eq!(
            stable_hash(&f64::from_bits(bits)),
            reference(&bits.to_le_bytes())
        );
    }
    assert_ne!(
        stable_hash(&f32::from_bits(0x7fc0_0001)),
        stable_hash(&f32::from_bits(0x7fc0_0002))
    );
}

#[test]
fn framing_and_streaming_are_consistent() {
    assert_ne!(stable_hash(&("ab", "c")), stable_hash(&("a", "bc")));
    assert_ne!(
        stable_hash(&(vec![1u8], vec![2u8, 3])),
        stable_hash(&(vec![1u8, 2], vec![3u8]))
    );
    assert_ne!(stable_hash(&Vec::<()>::new()), stable_hash(&vec![()]));
    let values = [1u16, 2, 3];
    assert_eq!(stable_hash(&values), stable_hash(values.as_slice()));
    assert_eq!(stable_hash(&values), stable_hash(&values.to_vec()));
    assert_eq!(stable_hash(&"abc".to_owned()), stable_hash("abc"));
    assert_eq!(stable_hash(&None::<u8>), reference(&[0]));
    assert_eq!(stable_hash(&Some(0u8)), reference(&[1, 0]));
    let mut hasher = StateHasher::default();
    hasher.field(&42u64);
    hasher.field("abc");
    assert_eq!(hasher.finish(), stable_hash(&(42u64, "abc")));
    let mut raw = StateHasher::new();
    raw.write_bytes(b"foo");
    raw.write_bytes(b"bar");
    assert_eq!(raw.finish(), reference(b"foobar"));
    // Hashing has no command payload cap and never invokes the wire encoder.
    let large = vec![0u8; synctick::codec::MAX_PAYLOAD_BYTES + 1];
    assert_eq!(stable_hash(&large), stable_hash(large.as_slice()));
}

#[derive(StableHash)]
struct Unit;
#[derive(StableHash)]
struct Empty {}
#[derive(StableHash)]
struct Tuple<T>(T, u16);
#[derive(StableHash)]
struct Named<'a, T> {
    value: T,
    label: &'a str,
}
#[derive(StableHash, Wire)]
enum Tagged<T> {
    #[wire(tag = 9)]
    Unit,
    #[wire(tag = 2)]
    Tuple(T, u16),
    #[wire(tag = 255)]
    Named { nested: Vec<Vec<T>> },
}
#[derive(StableHash)]
enum Reordered {
    #[stable_hash(tag = 255)]
    Named { nested: Vec<Vec<u16>> },
    #[stable_hash(tag = 2)]
    Tuple(u16, u16),
    #[stable_hash(tag = 9)]
    Unit,
}

#[test]
fn derives_hash_all_fields_and_explicit_tags() {
    assert_eq!(stable_hash(&Unit), stable_hash(&Empty {}));
    assert_eq!(stable_hash(&Tuple(513u16, 7)), reference(&[1, 2, 7, 0]));
    assert_eq!(
        stable_hash(&Named {
            value: 513u16,
            label: "x"
        }),
        reference(&[1, 2, 1, 0, 0, 0, 0, 0, 0, 0, b'x'])
    );
    assert_eq!(stable_hash(&Tagged::<u16>::Unit), reference(&[9]));
    assert_eq!(
        stable_hash(&Tagged::Tuple(513u16, 7)),
        reference(&[2, 1, 2, 7, 0])
    );
    let nested = vec![vec![513u16]];
    let bytes = [255, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 2];
    assert_eq!(
        stable_hash(&Tagged::Named {
            nested: nested.clone()
        }),
        reference(&bytes)
    );
    assert_eq!(stable_hash(&Reordered::Named { nested }), reference(&bytes));
    assert_eq!(
        stable_hash(&Reordered::Tuple(513, 7)),
        reference(&[2, 1, 2, 7, 0])
    );
    assert_eq!(stable_hash(&Reordered::Unit), reference(&[9]));
    assert_eq!(
        stable_hash(&(1u8, 2u8, 3u8, 4u8, 5u8, 6u8, 7u8, 8u8)),
        reference(&[1, 2, 3, 4, 5, 6, 7, 8])
    );
}

trait FieldType {
    type Field;
}
struct Container;
impl FieldType for Container {
    type Field = u16;
}
#[derive(StableHash)]
struct Associated<T: FieldType> {
    value: T::Field,
}
#[derive(StableHash)]
enum AssociatedEnum<T: FieldType> {
    #[stable_hash(tag = 3)]
    Value(T::Field),
}
#[test]
fn associated_field_bounds_do_not_require_container_to_hash() {
    assert_eq!(
        stable_hash(&Associated::<Container> { value: 513 }),
        reference(&[1, 2])
    );
    assert_eq!(
        stable_hash(&AssociatedEnum::<Container>::Value(513)),
        reference(&[3, 1, 2])
    );
}

#[test]
fn skipped_fields_contribute_no_bytes_or_bounds() {
    // The cache deliberately has no StableHash implementation.
    struct Cache;
    #[derive(StableHash)]
    struct State<T> {
        first: u16,
        #[stable_hash(skip)]
        cache: T,
        last: u8,
    }
    #[derive(StableHash)]
    struct TupleCache<T>(u16, #[stable_hash(skip)] T, u8);
    #[derive(StableHash)]
    struct OnlyCache<T>(#[stable_hash(skip)] T);
    #[derive(StableHash)]
    struct AssociatedCache<T: FieldType> {
        #[stable_hash(skip)]
        cache: T::Field,
    }
    struct CacheType;
    impl FieldType for CacheType {
        type Field = std::cell::Cell<u8>;
    }
    let mut state = State {
        first: 513,
        cache: Cache,
        last: 7,
    };
    let expected = reference(&[1, 2, 7]);
    assert_eq!(stable_hash(&state), expected);
    state.cache = Cache;
    assert_eq!(stable_hash(&state), expected);
    state.first += 1;
    assert_ne!(stable_hash(&state), expected);
    state.first -= 1;
    state.last += 1;
    assert_ne!(stable_hash(&state), expected);
    assert_eq!(stable_hash(&TupleCache(513, Cache, 7)), expected);
    assert_eq!(stable_hash(&OnlyCache(Cache)), reference(&[]));
    let associated = AssociatedCache::<CacheType> {
        cache: std::cell::Cell::new(1),
    };
    associated.cache.set(2);
    assert_eq!(stable_hash(&associated), reference(&[]));
}

#[test]
fn enum_skips_preserve_tags_and_payload_order() {
    #[derive(StableHash)]
    enum Cached<T> {
        #[stable_hash(tag = 1)]
        Named {
            first: u16,
            #[stable_hash(skip)]
            cache: T,
            last: u8,
        },
        #[stable_hash(tag = 2)]
        Tuple(#[stable_hash(skip)] T, u16, #[stable_hash(skip)] T, u8),
        #[stable_hash(tag = 3)]
        Only(#[stable_hash(skip)] T),
    }
    let mut named = Cached::Named {
        first: 513,
        cache: std::cell::Cell::new(1),
        last: 7,
    };
    assert_eq!(stable_hash(&named), reference(&[1, 1, 2, 7]));
    if let Cached::Named { cache, .. } = &mut named {
        cache.set(99);
    }
    assert_eq!(stable_hash(&named), reference(&[1, 1, 2, 7]));
    assert_eq!(
        stable_hash(&Cached::Tuple(
            std::cell::Cell::new(1),
            513,
            std::cell::Cell::new(2),
            7
        )),
        reference(&[2, 1, 2, 7])
    );
    assert_eq!(
        stable_hash(&Cached::Only(std::cell::Cell::new(1))),
        reference(&[3])
    );
}

#[test]
fn hash_skip_does_not_skip_wire_encoding() {
    #[derive(Wire, StableHash)]
    struct State {
        value: u8,
        #[stable_hash(skip)]
        cache: u16,
    }
    let first = State { value: 7, cache: 1 };
    let second = State { value: 7, cache: 2 };
    assert_eq!(stable_hash(&first), stable_hash(&second));
    assert_ne!(
        synctick::codec::encode(&first).unwrap(),
        synctick::codec::encode(&second).unwrap()
    );
}
