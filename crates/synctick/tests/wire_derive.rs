use synctick::{Wire, codec};

#[derive(Debug, PartialEq, Eq, Wire)]
struct Setup {
    seed: u64,
    balances: Vec<u64>,
}

#[derive(Debug, PartialEq, Eq, Wire)]
struct Batch<T>
where
    T: PartialEq,
{
    label: String,
    commands: Vec<Vec<T>>,
}

#[derive(Debug, PartialEq, Eq, Wire)]
struct Transfer<T>(u64, Vec<T>);

#[test]
fn derived_layout_preserves_existing_wallet_bytes() {
    let setup = Setup {
        seed: 42,
        balances: vec![100, 200],
    };
    let expected = [
        42, 0, 0, 0, 0, 0, 0, 0, // seed, u64 LE
        2, 0, 0, 0, // collection count, u32 LE
        100, 0, 0, 0, 0, 0, 0, 0, 200, 0, 0, 0, 0, 0, 0, 0,
    ];
    assert_eq!(Setup::MIN_SIZE, 12);
    assert_eq!(codec::encode(&setup).unwrap(), expected);
    assert_eq!(codec::decode::<Setup>(&expected).unwrap(), setup);

    let transfer = Transfer(1, vec![3u64, 7]);
    let expected = [
        1, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0,
    ];
    assert_eq!(codec::encode(&transfer).unwrap(), expected);
    assert_eq!(codec::decode::<Transfer<u64>>(&expected).unwrap(), transfer);
}

#[test]
fn generic_nested_payloads_reject_truncation_and_trailing_bytes() {
    let batch = Batch {
        label: "commands".into(),
        commands: vec![vec![Transfer(1, vec![3u64, 7])]],
    };
    let bytes = codec::encode(&batch).unwrap();
    assert_eq!(
        codec::decode::<Batch<Transfer<u64>>>(&bytes).unwrap(),
        batch
    );
    for end in 0..bytes.len() {
        assert!(codec::decode::<Batch<Transfer<u64>>>(&bytes[..end]).is_err());
    }
    let mut trailing = bytes;
    trailing.push(0);
    assert!(codec::decode::<Batch<Transfer<u64>>>(&trailing).is_err());
}

struct DecodeMustNotRun;
impl Wire for DecodeMustNotRun {
    const MIN_SIZE: usize = 1;
    fn validate(reader: &mut codec::Decoder<'_>) -> codec::Result<()> {
        <() as Wire>::validate(reader)
    }
    fn decode(_: &mut codec::Decoder<'_>) -> codec::Result<Self> {
        panic!("malformed later fields must be rejected before decoding any field")
    }
    fn encode(&self, writer: &mut codec::Encoder) -> codec::Result<()> {
        ().encode(writer)
    }
}

#[derive(Wire)]
struct Guarded {
    first: DecodeMustNotRun,
    nested: Vec<Vec<String>>,
}

#[test]
fn derived_validation_checks_all_fields_before_materializing_any() {
    let mut hostile = vec![0, 1, 0, 0, 0]; // marker, one outer element
    hostile.extend(u32::MAX.to_le_bytes()); // impossible inner count
    assert!(codec::decode::<Guarded>(&hostile).is_err());
    assert!(
        codec::encode(&Setup {
            seed: 1,
            balances: vec![0; codec::MAX_PAYLOAD_BYTES / 8],
        })
        .is_err()
    );
}

trait FieldType {
    type Field;
}

// The derive must bound the field type, not require the container T: Wire.
#[derive(Wire)]
struct Associated<T: FieldType> {
    value: T::Field,
}

struct Container;
impl FieldType for Container {
    type Field = u16;
}

#[test]
fn bounds_apply_to_associated_field_types() {
    let value = Associated::<Container> { value: 513 };
    assert_eq!(codec::encode(&value).unwrap(), [1, 2]);
    assert_eq!(
        codec::decode::<Associated<Container>>(&[1, 2])
            .unwrap()
            .value,
        513
    );
}

#[derive(Debug, PartialEq, Eq, Wire)]
enum Command<T> {
    #[wire(tag = 9)]
    Idle,
    #[wire(tag = 0)]
    Transfer(T, u16),
    #[wire(tag = 255)]
    Batch {
        commands: Vec<Vec<T>>,
        label: String,
    },
}

#[test]
fn enum_tags_and_payloads_have_stable_layouts() {
    assert_eq!(Command::<u64>::MIN_SIZE, 1);
    for (command, expected) in [
        (Command::Idle, vec![9]),
        (Command::Transfer(513u16, 7), vec![0, 1, 2, 7, 0]),
        (
            Command::Batch {
                commands: vec![vec![513]],
                label: "x".into(),
            },
            vec![255, 1, 0, 0, 0, 1, 0, 0, 0, 1, 2, 1, 0, 0, 0, b'x'],
        ),
    ] {
        let bytes = codec::encode(&command).unwrap();
        assert_eq!(bytes, expected);
        assert_eq!(codec::decode::<Command<u16>>(&bytes).unwrap(), command);
        for end in 0..bytes.len() {
            assert!(codec::decode::<Command<u16>>(&bytes[..end]).is_err());
        }
        let mut trailing = bytes;
        trailing.push(0);
        assert!(codec::decode::<Command<u16>>(&trailing).is_err());
    }
    assert!(codec::decode::<Command<u16>>(&[1]).is_err());
}

#[derive(Wire)]
enum GuardedEnum {
    #[wire(tag = 3)]
    Payload(DecodeMustNotRun, Vec<Vec<String>>),
}

#[test]
fn enum_validation_finishes_before_materializing_payloads() {
    let mut hostile = vec![3, 0, 1, 0, 0, 0];
    hostile.extend(u32::MAX.to_le_bytes());
    assert!(codec::decode::<GuardedEnum>(&hostile).is_err());
    assert!(
        codec::encode(&Command::<u8>::Batch {
            commands: vec![vec![0; codec::MAX_PAYLOAD_BYTES]],
            label: String::new(),
        })
        .is_err()
    );
}

#[derive(Wire)]
enum AssociatedEnum<T: FieldType> {
    #[wire(tag = 8)]
    Value { value: T::Field },
    #[wire(tag = 2)]
    Larger(u64),
}

#[test]
fn enum_minimum_size_and_associated_type_bounds_follow_payloads() {
    assert_eq!(AssociatedEnum::<Container>::MIN_SIZE, 3);
    let value = AssociatedEnum::<Container>::Value { value: 513 };
    assert_eq!(codec::encode(&value).unwrap(), [8, 1, 2]);
    assert!(matches!(
        codec::decode::<AssociatedEnum<Container>>(&[8, 1, 2]).unwrap(),
        AssociatedEnum::Value { value: 513 }
    ));
}

#[test]
fn variant_reordering_preserves_tags_and_collection_layouts() {
    #[derive(Wire)]
    enum Before {
        #[wire(tag = 17)]
        First,
        #[wire(tag = 4)]
        Second,
    }
    #[derive(Wire, Debug, PartialEq)]
    enum After {
        #[wire(tag = 4)]
        Second,
        #[wire(tag = 17)]
        First,
    }
    let bytes = codec::encode(&vec![Before::First, Before::Second]).unwrap();
    assert_eq!(bytes, [2, 0, 0, 0, 17, 4]);
    assert_eq!(
        codec::decode::<Vec<After>>(&bytes).unwrap(),
        [After::First, After::Second]
    );
    assert!(codec::decode::<Vec<After>>(&[2, 0, 0, 0, 17]).is_err());
}
