use crate::move_codegen::CodegenCtx;
use itertools::Itertools;
use move_binary_format::normalized::Type;
use move_core_types::account_address::AccountAddress;

pub trait ToRustType {
    fn to_rust_type(&self, ctx: &CodegenCtx) -> String;
    fn is_ref(&self) -> bool;
    fn to_arg_type(&self, ctx: &CodegenCtx) -> String;
}

pub trait ToRustTypeWithContext {
    fn to_rust_type_with_context(
        &self,
        ctx: &CodegenCtx,
        type_params: &[Option<move_symbol_pool::Symbol>],
    ) -> String;
    fn to_arg_type_with_context(
        &self,
        ctx: &CodegenCtx,
        type_params: &[Option<move_symbol_pool::Symbol>],
    ) -> String;
}

impl ToRustType for Type<move_binary_format::normalized::RcIdentifier> {
    fn to_rust_type(&self, ctx: &CodegenCtx) -> String {
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
            t @ Self::Datatype { .. } => try_resolve_known_types(t, ctx),
            Self::Vector(t) => {
                format!("Vec<{}>", t.to_rust_type(ctx))
            }
            Self::Reference(false, t) => {
                format!("&'static {}", t.to_rust_type(ctx))
            }
            Self::Reference(true, t) => {
                format!("&'static mut {}", t.to_rust_type(ctx))
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

    fn to_arg_type(&self, ctx: &CodegenCtx) -> String {
        match self {
            Self::Reference(false, t) => {
                format!("Ref<'a, {}>", t.to_rust_type(ctx))
            }
            Self::Reference(true, t) => {
                format!("MutRef<'a, {}>", t.to_rust_type(ctx))
            }
            _ => format!("Arg<{}>", self.to_rust_type(ctx)),
        }
    }
}

impl ToRustTypeWithContext for move_model_2::summary::Type {
    fn to_rust_type_with_context(
        &self,
        ctx: &CodegenCtx,
        type_params: &[Option<move_symbol_pool::Symbol>],
    ) -> String {
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
                try_resolve_known_types_summary_with_context(t, ctx, type_params)
            }
            Self::Vector(t) => {
                format!("Vec<{}>", t.to_rust_type_with_context(ctx, type_params))
            }
            Self::Reference(mutable, to) => {
                let mut_ = if *mutable { "mut " } else { "" };
                format!(
                    "&'static {mut_}{}",
                    to.to_rust_type_with_context(ctx, type_params)
                )
            }
            Self::TypeParameter(index) => format!("T{index}"),
            Self::NamedTypeParameter(name) => {
                // Find the position of this name in the type parameter list — the index
                // into the slice IS the original type-parameter index because None entries
                // are preserved as placeholders.
                if let Some(index) = type_params
                    .iter()
                    .position(|param| param.as_ref() == Some(name))
                {
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

    fn to_arg_type_with_context(
        &self,
        ctx: &CodegenCtx,
        type_params: &[Option<move_symbol_pool::Symbol>],
    ) -> String {
        match self {
            Self::Reference(mutable, to) => {
                let wrapper = if *mutable { "MutRef" } else { "Ref" };
                format!(
                    "{wrapper}<'a, {}>",
                    to.to_rust_type_with_context(ctx, type_params)
                )
            }
            _ => format!("Arg<{}>", self.to_rust_type_with_context(ctx, type_params)),
        }
    }
}

impl ToRustType for move_model_2::summary::Type {
    fn to_rust_type(&self, ctx: &CodegenCtx) -> String {
        self.to_rust_type_with_context(ctx, &[])
    }

    fn is_ref(&self) -> bool {
        matches!(self, Self::Reference { .. })
    }

    fn to_arg_type(&self, ctx: &CodegenCtx) -> String {
        self.to_arg_type_with_context(ctx, &[])
    }
}

fn try_resolve_known_types_summary_with_context(
    _type: &move_model_2::summary::Type,
    ctx: &CodegenCtx,
    type_params: &[Option<move_symbol_pool::Symbol>],
) -> String {
    if let move_model_2::summary::Type::Datatype(datatype) = _type {
        let address = &datatype.module.address;
        let module = &datatype.module.name;
        let name = &datatype.name;
        let type_arguments = &datatype.type_arguments;

        let account_addr = match ctx.tags.get(&address).cloned() {
            Some(addr) => addr,
            None => {
                let fallback = format!("{module}::{name}");
                return if type_arguments.is_empty() {
                    fallback
                } else {
                    format!(
                        "{fallback}<{}>",
                        type_arguments
                            .iter()
                            .map(|ty| ty.argument.to_rust_type_with_context(ctx, type_params))
                            .join(", ")
                    )
                };
            }
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
                        .to_rust_type_with_context(ctx, type_params)
                )
            }

            (&AccountAddress::TWO, "object", "UID") => "Address".to_string(),
            (&AccountAddress::TWO, "object", "ID") => "Address".to_string(),
            _ => {
                let package_path = ctx.bindings.get(&account_addr).cloned();

                let type_ = if let Some(package_path) = package_path {
                    // Track external type references (non-main-package types)
                    if !ctx.main_addrs.contains(&account_addr) {
                        ctx.external_refs.borrow_mut().insert((
                            account_addr,
                            module.as_str().to_string(),
                            name.as_str().to_string(),
                        ));
                    }
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
                            .map(|ty| ty.argument.to_rust_type_with_context(ctx, type_params))
                            .join(", ")
                    )
                }
            }
        }
    } else {
        unreachable!()
    }
}

fn try_resolve_known_types(
    _type: &Type<move_binary_format::normalized::RcIdentifier>,
    ctx: &CodegenCtx,
) -> String {
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
                format!("Option<{}>", type_arguments[0].to_rust_type(ctx))
            }

            (&AccountAddress::TWO, "object", "UID") => "Address".to_string(),
            (&AccountAddress::TWO, "object", "ID") => "Address".to_string(),
            _ => {
                let package_path = ctx.bindings.get(address).cloned();

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
                            .map(|ty| ty.to_rust_type(ctx))
                            .join(", ")
                    )
                }
            }
        }
    } else {
        unreachable!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use move_model_2::summary::Type as SType;

    fn u64_ref(mutable: bool) -> SType {
        SType::Reference(mutable, Box::new(SType::U64))
    }

    #[test]
    fn summary_reference_mutability_no_context() {
        // Regression guard: these were previously swapped.
        let ctx = CodegenCtx::default();
        assert_eq!(u64_ref(false).to_rust_type(&ctx), "&'static u64");
        assert_eq!(u64_ref(true).to_rust_type(&ctx), "&'static mut u64");
        assert_eq!(u64_ref(false).to_arg_type(&ctx), "Ref<'a, u64>");
        assert_eq!(u64_ref(true).to_arg_type(&ctx), "MutRef<'a, u64>");
    }

    #[test]
    fn summary_reference_mutability_with_context() {
        let ctx = CodegenCtx::default();
        assert_eq!(
            u64_ref(false).to_rust_type_with_context(&ctx, &[]),
            "&'static u64"
        );
        assert_eq!(
            u64_ref(true).to_rust_type_with_context(&ctx, &[]),
            "&'static mut u64"
        );
        assert_eq!(
            u64_ref(false).to_arg_type_with_context(&ctx, &[]),
            "Ref<'a, u64>"
        );
        assert_eq!(
            u64_ref(true).to_arg_type_with_context(&ctx, &[]),
            "MutRef<'a, u64>"
        );
    }

    #[test]
    fn named_type_parameter_resolves_by_original_index() {
        // The slice stores None for unnamed type params so the slice index lines up
        // with the original type-parameter position. Here `B` is the 3rd type param
        // and must resolve to `T2`, not `T1`.
        let ctx = CodegenCtx::default();
        let a = move_symbol_pool::Symbol::from("A");
        let b = move_symbol_pool::Symbol::from("B");
        let type_params = vec![Some(a), None, Some(b)];

        let ty = SType::NamedTypeParameter(b);
        assert_eq!(ty.to_rust_type_with_context(&ctx, &type_params), "T2");

        let ty = SType::NamedTypeParameter(a);
        assert_eq!(ty.to_rust_type_with_context(&ctx, &type_params), "T0");
    }
}
