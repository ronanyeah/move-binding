pub mod functions;

pub use move_core_types::u256::U256;
use serde::Serialize;
use std::str::FromStr;
pub use sui_sdk_types::Address;
pub use sui_sdk_types::Identifier;
pub use sui_sdk_types::StructTag;
pub use sui_sdk_types::TypeTag;

pub const MOVE_STDLIB: Address = {
    let mut address = [0u8; 32];
    address[31] = 1;
    Address::new(address)
};

pub trait MoveType: Serialize {
    fn type_() -> TypeTag;
}

pub trait MoveStruct: Serialize {
    fn struct_type() -> StructTag;
}

impl<T: MoveStruct> MoveType for T {
    fn type_() -> TypeTag {
        TypeTag::Struct(Self::struct_type().into())
    }
}

// todo: simplify with macros
impl MoveType for u8 {
    fn type_() -> TypeTag {
        TypeTag::U8
    }
}
impl MoveType for u16 {
    fn type_() -> TypeTag {
        TypeTag::U16
    }
}
impl MoveType for u32 {
    fn type_() -> TypeTag {
        TypeTag::U32
    }
}
impl MoveType for u64 {
    fn type_() -> TypeTag {
        TypeTag::U64
    }
}
impl MoveType for u128 {
    fn type_() -> TypeTag {
        TypeTag::U128
    }
}

impl MoveType for U256 {
    fn type_() -> TypeTag {
        TypeTag::U256
    }
}

impl MoveType for Address {
    fn type_() -> TypeTag {
        TypeTag::Address
    }
}

impl MoveType for bool {
    fn type_() -> TypeTag {
        TypeTag::Bool
    }
}

impl MoveType for String {
    fn type_() -> TypeTag {
        TypeTag::Struct(Box::new(StructTag {
            address: MOVE_STDLIB,
            module: Identifier::from_str("string").unwrap(),
            name: Identifier::from_str("String").unwrap(),
            type_params: vec![],
        }))
    }
}

impl MoveType for &str {
    fn type_() -> TypeTag {
        String::type_()
    }
}

impl<T: MoveType> MoveType for Option<T> {
    fn type_() -> TypeTag {
        TypeTag::Struct(Box::new(StructTag {
            address: MOVE_STDLIB,
            module: Identifier::from_str("option").unwrap(),
            name: Identifier::from_str("Option").unwrap(),
            type_params: vec![T::type_()],
        }))
    }
}

impl<T: MoveType> MoveType for Vec<T> {
    fn type_() -> TypeTag {
        TypeTag::Vector(Box::new(T::type_()))
    }
}

pub trait Key: MoveStruct {
    fn id(&self) -> &Address;
}
