use sync::{StableHash, Wire, codec, stable_hash};

#[derive(Debug, PartialEq, Wire, StableHash)]
struct Setup {
    seed: u64,
    names: Vec<String>,
}

#[derive(Debug, PartialEq, Wire, StableHash)]
struct Pair<T>(T, u16);

#[derive(Debug, PartialEq, Wire, StableHash)]
enum Command<T> {
    #[wire(tag = 3)]
    Idle,
    #[wire(tag = 7)]
    Batch { values: Vec<T> },
    #[wire(tag = 12)]
    Pair(T, u16),
}

#[derive(StableHash)]
enum HashOnly {
    #[stable_hash(tag = 5)]
    Value(u64),
}

#[test]
fn derives_resolve_the_dependency_alias_for_structs_and_enum_payloads() {
    let setup = Setup {
        seed: 42,
        names: vec!["wallet".into()],
    };
    let bytes = codec::encode(&setup).unwrap();
    let restored: Setup = codec::decode(&bytes).unwrap();
    assert_eq!(setup, restored);
    assert_eq!(stable_hash(&setup), stable_hash(&restored));
    let pair = Pair(513u16, 7);
    assert_eq!(codec::encode(&pair).unwrap(), [1, 2, 7, 0]);
    assert_eq!(stable_hash(&pair), stable_hash(&(513u16, 7u16)));
    for command in [
        Command::Idle,
        Command::Batch {
            values: vec![1u16, 2],
        },
        Command::Pair(513, 7),
    ] {
        let bytes = codec::encode(&command).unwrap();
        let restored: Command<u16> = codec::decode(&bytes).unwrap();
        assert_eq!(command, restored);
        assert_eq!(stable_hash(&command), stable_hash(&restored));
    }
    assert_eq!(
        stable_hash(&HashOnly::Value(42)),
        stable_hash(&(5u8, 42u64))
    );
}
