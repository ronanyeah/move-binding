use crate::move_codegen::BINDING_REGISTRY;
use crate::move_codegen::TAG_REGISTRY;
use itertools::Itertools;
use move_binary_format::normalized::Type;
use move_core_types::account_address::AccountAddress;

pub trait ToRustType {
    fn to_rust_type(&self) -> String;
    fn is_ref(&self) -> bool;
    fn to_arg_type(&self) -> String;
}

pub trait ToRustTypeWithContext {
    fn to_rust_type_with_context(&self, type_params: &[move_symbol_pool::Symbol]) -> String;
    fn to_arg_type_with_context(&self, type_params: &[move_symbol_pool::Symbol]) -> String;
}

impl ToRustType for Type<move_binary_format::normalized::RcIdentifier> {
    fn to_rust_type(&self) -> String {
        match self {
            Self::Bool => "bool".to_string(),
            Self::U8 => "u8".to_string(),
            Self::U16 => "u16".to_string(),
            Self::U32 => "u32".to_string(),
            Self::U64 => "u64".to_string(),
            Self::U128 => "u128".to_string(),
            Self::U256 => "move_types::U256".to_string(),
            Self::Address => "Address".to_string(),
            Self::Signer => "Address".to_string(),
            t @ Self::Datatype { .. } => try_resolve_known_types(t),
            Self::Vector(t) => {
                format!("Vec<{}>", t.to_rust_type())
            }
            Self::Reference(false, t) => {
                format!("&'static {}", t.to_rust_type())
            }
            Self::Reference(true, t) => {
                format!("&'static mut {}", t.to_rust_type())
            }
            Self::TypeParameter(index) => format!("T{index}"),
        }
    }

    fn is_ref(&self) -> bool {
        match self {
            Self::Reference(_, _) => true,
            _ => false,
        }
    }

    fn to_arg_type(&self) -> String {
        match self {
            Self::Reference(false, t) => {
                format!("Ref<'a, {}>", t.to_rust_type())
            }
            Self::Reference(true, t) => {
                format!("MutRef<'a, {}>", t.to_rust_type())
            }
            _ => format!("Arg<{}>", self.to_rust_type()),
        }
    }
}

impl ToRustTypeWithContext for move_model_2::summary::Type {
    fn to_rust_type_with_context(&self, type_params: &[move_symbol_pool::Symbol]) -> String {
        match self {
            Self::Bool => "bool".to_string(),
            Self::U8 => "u8".to_string(),
            Self::U16 => "u16".to_string(),
            Self::U32 => "u32".to_string(),
            Self::U64 => "u64".to_string(),
            Self::U128 => "u128".to_string(),
            Self::U256 => "move_types::U256".to_string(),
            Self::Address => "Address".to_string(),
            Self::Signer => "Address".to_string(),
            t @ Self::Datatype { .. } => {
                try_resolve_known_types_summary_with_context(t, type_params)
            }
            Self::Vector(t) => {
                format!("Vec<{}>", t.to_rust_type_with_context(type_params))
            }
            Self::Reference(mutable, to) => {
                if *mutable {
                    format!("&'static mut {}", to.to_rust_type_with_context(type_params))
                } else {
                    format!("&'static {}", to.to_rust_type_with_context(type_params))
                }
            }
            Self::TypeParameter(index) => format!("T{index}"),
            Self::NamedTypeParameter(name) => {
                // Find the index of this type parameter name
                if let Some(index) = type_params.iter().position(|param| param == name) {
                    format!("T{index}")
                } else {
                    // Fallback if not found
                    name.as_str().to_string()
                }
            }
            Self::Tuple(_) => unimplemented!(),
            Self::Fun(_, _) => unimplemented!(),
            Self::Any => unimplemented!(),
        }
    }

    fn to_arg_type_with_context(&self, type_params: &[move_symbol_pool::Symbol]) -> String {
        match self {
            Self::Reference(mutable, to) => {
                if *mutable {
                    format!("MutRef<'a, {}>", to.to_rust_type_with_context(type_params))
                } else {
                    format!("Ref<'a, {}>", to.to_rust_type_with_context(type_params))
                }
            }
            _ => format!("Arg<{}>", self.to_rust_type_with_context(type_params)),
        }
    }
}

impl ToRustType for move_model_2::summary::Type {
    fn to_rust_type(&self) -> String {
        match self {
            Self::Bool => "bool".to_string(),
            Self::U8 => "u8".to_string(),
            Self::U16 => "u16".to_string(),
            Self::U32 => "u32".to_string(),
            Self::U64 => "u64".to_string(),
            Self::U128 => "u128".to_string(),
            Self::U256 => "move_types::U256".to_string(),
            Self::Address => "Address".to_string(),
            Self::Signer => "Address".to_string(),
            t @ Self::Datatype { .. } => try_resolve_known_types_summary(t),
            Self::Vector(t) => {
                format!("Vec<{}>", t.to_rust_type())
            }
            Self::Reference(mutable, to) => {
                if *mutable {
                    format!("&'static {}", to.to_rust_type())
                } else {
                    format!("&'static mut {}", to.to_rust_type())
                }
            }
            Self::TypeParameter(index) => format!("T{index}"),
            Self::NamedTypeParameter(_name) => {
                // Named type parameters should be mapped to T0, T1, etc.
                // This requires context about the containing struct/function's type parameters
                // For now, fallback to the name itself as a placeholder
                "TypeParam".to_string()
            }
            Self::Tuple(_) => unimplemented!(),
            Self::Fun(_, _) => unimplemented!(),
            Self::Any => unimplemented!(),
        }
    }

    fn is_ref(&self) -> bool {
        matches!(self, Self::Reference { .. })
    }

    fn to_arg_type(&self) -> String {
        match self {
            Self::Reference(mutable, to) => {
                if *mutable {
                    format!("Ref<'a, {}>", to.to_rust_type())
                } else {
                    format!("MutRef<'a, {}>", to.to_rust_type())
                }
            }
            _ => format!("Arg<{}>", self.to_rust_type()),
        }
    }
}

fn try_resolve_known_types_summary(_type: &move_model_2::summary::Type) -> String {
    try_resolve_known_types_summary_with_context(_type, &[])
}

fn try_resolve_known_types_summary_with_context(
    _type: &move_model_2::summary::Type,
    type_params: &[move_symbol_pool::Symbol],
) -> String {
    if let move_model_2::summary::Type::Datatype(datatype) = _type {
        let address = &datatype.module.address;
        let module = &datatype.module.name;
        let name = &datatype.name;
        let type_arguments = &datatype.type_arguments;

        let account_addr = {
            let cache = TAG_REGISTRY.read().unwrap();
            let addr = cache.get(&address).cloned().unwrap();
            addr
        };

        match (&account_addr, module.as_str(), name.as_str()) {
            (&AccountAddress::ONE, "type_name", "TypeName") => "String".to_string(),
            (&AccountAddress::ONE, "string", "String") => "String".to_string(),
            (&AccountAddress::ONE, "ascii", "String") => "String".to_string(),
            (&AccountAddress::ONE, "option", "Option") => {
                format!(
                    "Option<{}>",
                    type_arguments[0]
                        .argument
                        .to_rust_type_with_context(type_params)
                )
            }

            (&AccountAddress::TWO, "object", "UID") => "ObjectId".to_string(),
            (&AccountAddress::TWO, "object", "ID") => "ObjectId".to_string(),
            _ => {
                let cache = BINDING_REGISTRY.read().unwrap();

                let package_path = cache.get(&account_addr).cloned();
                drop(cache);

                let type_ = if let Some(package_path) = package_path {
                    format!("{package_path}::{module}::{name}")
                } else {
                    format!("{module}::{name}")
                };

                if type_arguments.is_empty() {
                    type_
                } else {
                    format!(
                        "{type_}<{}>",
                        type_arguments
                            .iter()
                            .map(|ty| ty.argument.to_rust_type_with_context(type_params))
                            .join(", ")
                    )
                }
            }
        }
    } else {
        unreachable!()
    }
}

fn try_resolve_known_types(_type: &Type<move_binary_format::normalized::RcIdentifier>) -> String {
    if let Type::Datatype(datatype) = _type {
        let address = &datatype.module.address;
        let module = &datatype.module.name;
        let name = &datatype.name;
        let type_arguments = &datatype.type_arguments;

        match (address, module.as_str(), name.as_str()) {
            (&AccountAddress::ONE, "type_name", "TypeName") => "String".to_string(),
            (&AccountAddress::ONE, "string", "String") => "String".to_string(),
            (&AccountAddress::ONE, "ascii", "String") => "String".to_string(),
            (&AccountAddress::ONE, "option", "Option") => {
                format!("Option<{}>", type_arguments[0].to_rust_type())
            }

            (&AccountAddress::TWO, "object", "UID") => "ObjectId".to_string(),
            (&AccountAddress::TWO, "object", "ID") => "ObjectId".to_string(),
            _ => {
                let cache = BINDING_REGISTRY.read().unwrap();

                let package_path = cache.get(address).cloned();
                drop(cache); // Release read lock

                let type_ = if let Some(package_path) = package_path {
                    format!("{package_path}::{module}::{name}")
                } else {
                    format!("{module}::{name}")
                };

                if type_arguments.is_empty() {
                    type_
                } else {
                    format!(
                        "{type_}<{}>",
                        type_arguments.iter().map(|ty| ty.to_rust_type()).join(", ")
                    )
                }
            }
        }
    } else {
        unreachable!()
    }
}
