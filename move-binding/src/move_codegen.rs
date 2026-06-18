use crate::SuiNetwork;
use crate::package_provider::{ModuleProvider, MoveModuleProvider};
use crate::types::{ToRustType, ToRustTypeWithContext};
use anyhow::{Context, anyhow};
use itertools::Itertools;
use move_binary_format::normalized::{Enum, Function, RcIdentifier, Struct, Type};
use move_core_types::account_address::AccountAddress;
use proc_macro2::{Ident, TokenStream};
use quote::quote;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

/// Per-`expand` codegen context. Replaces the former process-wide `RwLock`
/// statics so concurrent or repeated expansions don't clobber each other.
#[derive(Default)]
pub struct CodegenCtx {
    /// Maps a (defining) package address to the Rust path its bindings live at.
    pub bindings: HashMap<AccountAddress, String>,
    /// Maps an address-alias symbol to the actual account address.
    pub tags: HashMap<move_symbol_pool::Symbol, AccountAddress>,
    /// The main package addresses being generated, used to distinguish external refs.
    pub main_addrs: HashSet<AccountAddress>,
    /// External (dependency) types referenced during codegen, accumulated as
    /// type resolution walks field types: (package_addr, module, type_name).
    pub external_refs: RefCell<HashSet<(AccountAddress, String, String)>>,
}

pub struct MoveCodegen;

impl MoveCodegen {
    pub fn expand(
        network: SuiNetwork,
        package: &str,
        package_alias: &str,
        base_path: &str,
    ) -> Result<TokenStream, anyhow::Error> {
        let module_provider = MoveModuleProvider::new(network);
        let package = module_provider.get_package(package)?;

        let main_pkg_addr = *package
            .module_map
            .values()
            .next()
            .context("package has no modules")?
            .address();

        // register package path
        let mut bindings: HashMap<AccountAddress, String> = HashMap::new();
        package
            .type_origin_table
            .iter()
            .flat_map(|(_, m)| m.values())
            .dedup()
            .for_each(|addr| {
                bindings.insert(addr.clone(), format!("{base_path}::{package_alias}"));
            });

        // register dependency packages so type references resolve to qualified paths
        let dep_addrs: std::collections::BTreeSet<AccountAddress> = package
            .module_map
            .values()
            .flat_map(|m| m.immediate_dependencies.iter().map(|d| d.address))
            .filter(|addr| *addr != main_pkg_addr)
            .collect();
        for addr in &dep_addrs {
            if bindings.contains_key(addr) {
                continue;
            }
            let alias = match *addr {
                AccountAddress::ONE => dep_alias("std"),
                AccountAddress::TWO => dep_alias("sui"),
                _ => dep_alias(&format!("{:#x}", addr)),
            };
            bindings.insert(*addr, format!("{base_path}::{package_alias}::{alias}"));
        }

        let ctx = CodegenCtx {
            bindings,
            ..Default::default()
        };

        let module_tokens = package
            .module_map
            .iter()
            .map(|(module_name, module)| {
                let module_ident = Ident::new(module_name, proc_macro2::Span::call_site());
                let type_origin_table = package
                    .type_origin_table
                    .get(module_name)
                    .cloned()
                    .unwrap_or_default();

                let mut struct_fun_tokens =
                    Self::create_structs(&module.structs, &type_origin_table, &ctx)?;
                struct_fun_tokens.extend(Self::create_enums(
                    &module.enums,
                    &type_origin_table,
                    &ctx,
                ));
                struct_fun_tokens.extend(Self::create_funs(&module.functions, &ctx));

                Ok::<_, anyhow::Error>(if struct_fun_tokens.is_empty() {
                    quote! {}
                } else {
                    let addr_hex = addr_hex_literal(module.address());
                    quote! {
                        pub mod #module_ident{
                            use move_binding_derive::{MoveStruct, Key};
                            use move_types::{MoveType, Address, Identifier};
                            use move_types::functions::{Arg, Ref, MutRef};
                            pub const PACKAGE_ID: Address = Address::from_static(#addr_hex);
                            pub const MODULE_NAME: &str = #module_name;
                            #(#struct_fun_tokens)*
                        }
                    }
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        // fetch and generate stub modules for dependency packages
        let mut dep_tokens = vec![];
        for addr in &dep_addrs {
            let alias = match *addr {
                AccountAddress::ONE => dep_alias("std"),
                AccountAddress::TWO => dep_alias("sui"),
                _ => dep_alias(&format!("{:#x}", addr)),
            };
            let dep_pkg = match module_provider.get_package(&format!("{:#x}", addr)) {
                Ok(p) => p,
                Err(_) => continue,
            };
            let dep_alias_ident = Ident::new(&alias, proc_macro2::Span::call_site());
            let dep_module_tokens: Vec<_> = dep_pkg
                .module_map
                .iter()
                .filter_map(|(module_name, module)| {
                    let type_origin_table = dep_pkg
                        .type_origin_table
                        .get(module_name)
                        .cloned()
                        .unwrap_or_default();
                    let struct_tokens =
                        Self::create_structs(&module.structs, &type_origin_table, &ctx).ok()?;
                    let enum_tokens = Self::create_enums(&module.enums, &type_origin_table, &ctx);
                    let all_tokens: Vec<_> = struct_tokens.into_iter().chain(enum_tokens).collect();
                    if all_tokens.is_empty() {
                        return None;
                    }
                    let module_ident = Ident::new(module_name, proc_macro2::Span::call_site());
                    let addr_hex = addr_hex_literal(module.address());
                    Some(quote! {
                        pub mod #module_ident {
                            use move_binding_derive::{MoveStruct, Key};
                            use move_types::{MoveType, Address, Identifier, StructTag};
                            pub const PACKAGE_ID: Address = Address::from_static(#addr_hex);
                            pub const MODULE_NAME: &str = #module_name;
                            #(#all_tokens)*
                        }
                    })
                })
                .collect();
            if !dep_module_tokens.is_empty() {
                dep_tokens.push(quote! {
                    pub mod #dep_alias_ident {
                        #(#dep_module_tokens)*
                    }
                });
            }
        }

        let package_ident = Ident::new(&package_alias, proc_macro2::Span::call_site());
        let version = package.version;

        Ok(quote! {
            #[allow(unused_imports, non_camel_case_types, non_snake_case, non_upper_case_globals)]
            pub mod #package_ident{
                pub const PACKAGE_VERSION:u64 = #version;
                #(#module_tokens)*
                #(#dep_tokens)*
            }
        })
    }

    fn create_structs(
        structs: &indexmap::IndexMap<RcIdentifier, Rc<Struct<RcIdentifier>>>,
        type_origin_ids: &HashMap<String, AccountAddress>,
        ctx: &CodegenCtx,
    ) -> Result<Vec<TokenStream>, anyhow::Error> {
        structs
            .iter()
            .map(|(name, move_struct)| {
                Self::create_struct(name.as_str(), move_struct, type_origin_ids, ctx)
            })
            .collect()
    }

    fn create_struct(
        struct_name: &str,
        move_struct: &Rc<Struct<RcIdentifier>>,
        type_origin_id: &HashMap<String, AccountAddress>,
        ctx: &CodegenCtx,
    ) -> Result<TokenStream, anyhow::Error> {
        let (type_parameters, phantoms) = move_struct.type_parameters.iter().enumerate().fold(
            (vec![], vec![]),
            |(mut type_parameters, mut phantoms), (i, v)| {
                let ident = Ident::new(&format!("T{i}"), proc_macro2::Span::call_site());
                type_parameters.push(quote! {#ident});

                if v.is_phantom {
                    let name =
                        Ident::new(&format!("phantom_data_{i}"), proc_macro2::Span::call_site());
                    let type_: syn::Type =
                        syn::parse_str(&format!("std::marker::PhantomData<T{i}>")).unwrap();
                    phantoms.push(quote! {#name: #type_,})
                }
                (type_parameters, phantoms)
            },
        );

        let struct_ident = Ident::new(&struct_name.to_string(), proc_macro2::Span::call_site());
        let field_tokens = move_struct
            .fields
            .0
            .iter()
            .map(|(_, field)| {
                let field_ident = Ident::new(
                    &escape_keyword(field.name.as_str()),
                    proc_macro2::Span::call_site(),
                );
                let field_type: syn::Type = syn::parse_str(&field.type_.to_rust_type(ctx))?;
                Ok(quote! {pub #field_ident: #field_type,})
            })
            .collect::<Result<Vec<_>, anyhow::Error>>()?;

        let mut derives = vec![
            quote! {serde::Deserialize},
            quote! {serde::Serialize},
            quote! {Debug},
            quote! {MoveStruct},
        ];

        if move_struct.abilities.has_key() {
            derives.push(quote! {Key});
        }

        let addr_hex = addr_hex_literal(&type_origin_id[struct_name]);
        Ok(if type_parameters.is_empty() {
            quote! {
                #[derive(#(#derives),*)]
                #[move_struct(origin = #addr_hex)]
                pub struct #struct_ident {
                    #(#field_tokens)*
                }
            }
        } else {
            quote! {
                #[derive(#(#derives),*)]
                #[move_struct(origin = #addr_hex)]
                pub struct #struct_ident<#(#type_parameters),*> {
                    #(#field_tokens)*
                    #(#phantoms)*
                }
            }
        })
    }

    fn create_enums(
        enums: &indexmap::IndexMap<RcIdentifier, Rc<Enum<RcIdentifier>>>,
        type_origin_ids: &HashMap<String, AccountAddress>,
        ctx: &CodegenCtx,
    ) -> Vec<TokenStream> {
        enums
            .iter()
            .map(|(name, move_enum)| {
                Self::create_enum(name.as_str(), move_enum, type_origin_ids, ctx)
            })
            .collect()
    }

    fn create_enum(
        enum_name: &str,
        move_enum: &Rc<Enum<RcIdentifier>>,
        type_origin_id: &HashMap<String, AccountAddress>,
        ctx: &CodegenCtx,
    ) -> TokenStream {
        let type_parameters: Vec<_> = move_enum
            .type_parameters
            .iter()
            .enumerate()
            .map(|(i, _)| {
                let ident = Ident::new(&format!("T{i}"), proc_macro2::Span::call_site());
                quote! {#ident}
            })
            .collect();

        let enum_ident = Ident::new(&enum_name.to_string(), proc_macro2::Span::call_site());
        let variant_tokens = move_enum.variants.iter().map(|(_, variant)| {
            let variant_ident = Ident::new(
                &escape_keyword(&variant.name.as_str()),
                proc_macro2::Span::call_site(),
            );

            if variant.fields.0.is_empty() {
                return quote! {#variant_ident,};
            }

            if variant
                .fields
                .0
                .iter()
                .enumerate()
                .all(|(i, (_, field))| field.name.to_string() == format!("pos{}", i))
            {
                let field_types = variant.fields.0.iter().map(|(_, field)| {
                    let field_type: syn::Type =
                        syn::parse_str(&field.type_.to_rust_type(ctx)).unwrap();
                    quote! {#field_type,}
                });

                return quote! {
                    #variant_ident(#(#field_types)*),
                };
            }

            let field_tokens = variant.fields.0.iter().map(|(_, field)| {
                let field_ident = Ident::new(
                    &escape_keyword(field.name.as_str()),
                    proc_macro2::Span::call_site(),
                );
                let field_type: syn::Type = syn::parse_str(&field.type_.to_rust_type(ctx)).unwrap();
                quote! {#field_ident: #field_type,}
            });
            quote! { #variant_ident {#(#field_tokens)*},}
        });

        let derives = vec![
            quote! {serde::Deserialize},
            quote! {serde::Serialize},
            quote! {Debug},
            quote! {MoveStruct},
        ];

        let addr_hex = addr_hex_literal(&type_origin_id[enum_name]);

        if type_parameters.is_empty() {
            quote! {
                #[derive(#(#derives),*)]
                #[move_struct(origin = #addr_hex)]
                pub enum #enum_ident {
                    #(#variant_tokens)*
                }
            }
        } else {
            quote! {
                #[derive(#(#derives),*)]
                #[move_struct(origin = #addr_hex)]
                pub enum #enum_ident<#(#type_parameters),*> {
                    #(#variant_tokens)*
                }
            }
        }
    }

    fn create_funs(
        funs: &indexmap::IndexMap<RcIdentifier, Rc<Function<RcIdentifier>>>,
        ctx: &CodegenCtx,
    ) -> Vec<TokenStream> {
        funs.iter()
            .flat_map(|(name, fun)| Self::create_fun(name.as_str(), fun, ctx))
            .collect()
    }

    fn create_fun(
        fun_name: &str,
        fun: &Rc<Function<RcIdentifier>>,
        ctx: &CodegenCtx,
    ) -> Option<TokenStream> {
        let (param_names, mut params, need_lifetime) = fun.parameters.iter().enumerate().fold(
            (vec![], vec![], false),
            |(mut param_names, mut params, mut lifetime), (i, move_type)| {
                let field_ident = Ident::new(&format!("p{i}"), proc_macro2::Span::call_site());
                lifetime = lifetime || move_type.is_ref();
                match &**move_type {
                    Type::Reference(_mutable, r) => {
                        match &**r {
                            Type::Datatype(obj) => {
                                // filter out TxContext
                                if &obj.module.address == &AccountAddress::TWO
                                    && obj.name.as_str() == "TxContext"
                                {
                                    return (param_names, params, lifetime);
                                }
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
                param_names.push(quote! {#field_ident});
                let field_type: syn::Type = syn::parse_str(&move_type.to_arg_type(ctx)).unwrap();
                params.push(quote! {#field_ident: #field_type});
                (param_names, params, lifetime)
            },
        );
        params.insert(
            0,
            quote! {builder: &mut sui_transaction_builder::TransactionBuilder},
        );

        let returns = fun
            .return_
            .iter()
            .flat_map(|move_type| syn::parse_str::<syn::Type>(&move_type.to_arg_type(ctx)).ok())
            .collect::<Vec<_>>();

        let (types, mut types_with_ability) = fun.type_parameters.iter().enumerate().fold(
            (vec![], vec![]),
            |(mut types, mut types_with_ability), (i, v)| {
                let ident = Ident::new(&format!("T{i}"), proc_macro2::Span::call_site());
                types.push(ident.clone());
                let mut abilities = vec![];
                if v.has_key() {
                    abilities.push(quote! {move_types::Key});
                } else {
                    abilities.push(quote! {MoveType});
                }
                types_with_ability.push(quote! {#ident: #(#abilities)+*});
                (types, types_with_ability)
            },
        );

        if need_lifetime || fun.return_.iter().any(|t| t.is_ref()) {
            types_with_ability.insert(0, quote! {'a})
        }

        let fun_ident = Ident::new(&escape_keyword(fun_name), proc_macro2::Span::call_site());

        let type_args_call = if types.is_empty() {
            quote! {}
        } else {
            quote! { .with_type_args(vec![#(#types::type_()),*]) }
        };
        let call_expr = quote! {
            builder.move_call(
                sui_transaction_builder::Function::new(
                    PACKAGE_ID,
                    Identifier::from_static(MODULE_NAME),
                    Identifier::from_static(#fun_name),
                )#type_args_call,
                vec![#(#param_names.into()),*],
            )
        };

        let (maybe_returns, body) = match returns.len() {
            0 => (quote! {}, quote! { #call_expr; }),
            1 => {
                let t = &returns[0];
                (quote! { -> #t }, quote! { #call_expr.into() })
            }
            n => {
                let ret_tuple = quote! { -> (#(#returns),*) };
                let nested_idents: Vec<_> = (0..n)
                    .map(|i| Ident::new(&format!("__r{i}"), proc_macro2::Span::call_site()))
                    .collect();
                let indices: Vec<_> = (0..n).collect();
                let body = quote! {
                    let __ret = #call_expr.to_nested(#n);
                    #(let #nested_idents = __ret[#indices].into();)*
                    (#(#nested_idents),*)
                };
                (ret_tuple, body)
            }
        };

        let sig = if types_with_ability.is_empty() {
            quote! {
                pub fn #fun_ident(#(#params),*) #maybe_returns
            }
        } else {
            quote! {
                pub fn #fun_ident <#(#types_with_ability),*>(#(#params),*) #maybe_returns
            }
        };

        let fun_impl = quote! {
            #sig {
                #body
            }
        };
        Some(fun_impl)
    }
}

/// Formats an `AccountAddress` as a 0x-prefixed full 64-char hex literal,
/// suitable for `Address::from_static`.
fn addr_hex_literal(addr: &AccountAddress) -> String {
    let mut s = String::with_capacity(66);
    s.push_str("0x");
    for b in addr.to_vec() {
        use std::fmt::Write;
        let _ = write!(s, "{:02x}", b);
    }
    s
}

fn escape_keyword(name: &str) -> String {
    const RUST_KEYWORDS: &[&str] = &[
        "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn",
        "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref",
        "return", "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe",
        "use", "where", "while", "async", "await", "dyn", "abstract", "become", "box", "do",
        "final", "macro", "override", "priv", "typeof", "unsized", "virtual", "yield", "try",
    ];
    if RUST_KEYWORDS.contains(&name) {
        format!("{name}_")
    } else {
        name.to_string()
    }
}

/// Map a Move package name to a Rust-safe module alias.
pub fn dep_alias(name: &str) -> String {
    // `std` collides with the Rust standard library; other names that collide with
    // Rust strict keywords get a `move_` prefix.
    const RESERVED: &[&str] = &[
        "std", "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false",
        "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
        "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
        "unsafe", "use", "where", "while", "async", "await", "dyn",
    ];
    if name.starts_with("0x") {
        format!("pkg_{}", &name[2..10])
    } else if RESERVED.contains(&name) {
        format!("move_{name}")
    } else {
        name.to_string()
    }
}

/// Info needed to generate a stub type.
struct StubDef {
    /// One entry per type parameter; `true` if phantom.
    type_params: Vec<bool>,
    /// Named fields: (field_name, rust_type_string)
    fields: Vec<(String, String)>,
}

pub struct LocalCodegen;

impl LocalCodegen {
    pub fn expand(
        summary: &summary_parser::Summary,
        package_alias: &str,
        base_path: &str,
        package_version: Option<u64>,
    ) -> Result<TokenStream, anyhow::Error> {
        Self::expand_multi(&[(summary, package_alias)], base_path, package_version)
    }

    pub fn expand_multi(
        inputs: &[(&summary_parser::Summary, &str)],
        base_path: &str,
        package_version: Option<u64>,
    ) -> Result<TokenStream, anyhow::Error> {
        if inputs.is_empty() {
            return Err(anyhow!("at least one input is required"));
        }

        // Resolve main package address for each input. Fall back to
        // root_package_original_id in the metadata if the alias isn't a key
        // in address_mapping (the case for summaries generated from chain).
        let mains: Vec<(AccountAddress, &summary_parser::Summary, &str)> = inputs
            .iter()
            .map(|(summary, alias)| {
                let addr = if let Some(addr) = summary.address_mapping.get(&(*alias).into()) {
                    *addr
                } else {
                    let meta = summary
                        .additional_metadata
                        .as_ref()
                        .with_context(|| format!(
                            "alias '{alias}' not in address_mapping and no root_package_metadata.json present"
                        ))?;
                    let hex = meta
                        .get("root_package_original_id")
                        .and_then(|v| v.as_str())
                        .with_context(|| format!(
                            "alias '{alias}' not in address_mapping and root_package_original_id missing from metadata"
                        ))?;
                    AccountAddress::from_hex_literal(hex)
                        .with_context(|| format!("invalid root_package_original_id '{hex}'"))?
                };
                Ok::<_, anyhow::Error>((addr, *summary, *alias))
            })
            .collect::<Result<_, _>>()?;

        let main_addr_set: HashSet<AccountAddress> = mains.iter().map(|(a, _, _)| *a).collect();

        // Map original → latest published address (used for PACKAGE_ID, not type identity).
        let summaries_refs: Vec<_> = inputs.iter().map(|(s, _)| *s).collect();
        let latest_addr_map = summary_parser::build_latest_address_map(&summaries_refs);

        // Build the per-call codegen context (replaces the former process-wide statics).

        // Tags: union of all inputs' address mappings (alias symbol → address).
        let mut tags: HashMap<move_symbol_pool::Symbol, AccountAddress> = HashMap::new();
        for (summary, _) in inputs {
            for (k, v) in &summary.address_mapping {
                tags.insert(*k, *v);
            }
        }

        // Bindings:
        // - Mains first: every address in a main's type_origin_table → its alias.
        // - Then deps overwrite: each non-main address from any address_mapping
        //   gets remapped to its own dep alias. First-seen alias wins across
        //   summaries.
        let mut bindings: HashMap<AccountAddress, String> = HashMap::new();
        for (main_addr, summary, alias) in &mains {
            summary
                .type_origin_table
                .iter()
                .flat_map(|(_, m)| m.values())
                .for_each(|addr| {
                    bindings.insert(*addr, format!("{base_path}::{alias}"));
                });
            bindings.insert(*main_addr, format!("{base_path}::{alias}"));
        }
        let mut seen_deps: HashSet<AccountAddress> = HashSet::new();
        for (summary, _) in inputs {
            for (name, addr) in &summary.address_mapping {
                if main_addr_set.contains(addr) {
                    continue;
                }
                if !seen_deps.insert(*addr) {
                    continue;
                }
                let alias = match *addr {
                    AccountAddress::ONE => dep_alias("std"),
                    AccountAddress::TWO => dep_alias("sui"),
                    _ => dep_alias(name.as_str()),
                };
                bindings.insert(*addr, format!("{base_path}::{alias}"));
            }
        }

        let ctx = CodegenCtx {
            bindings,
            tags,
            main_addrs: main_addr_set,
            ..Default::default()
        };

        // Generate main package modules per input.
        let version = package_version.unwrap_or(1);
        let mut main_pkg_tokens = vec![];
        for (main_addr, summary, alias) in &mains {
            let main_pkg = summary
                .packages
                .packages
                .get(main_addr)
                .with_context(|| format!("package address {:#x} not in summary", main_addr))?;
            let module_tokens: Vec<_> = main_pkg
                .modules
                .iter()
                .map(|(module_name, module)| {
                    let module_ident =
                        Ident::new(module_name.as_str(), proc_macro2::Span::call_site());
                    let type_origin_table = summary
                        .type_origin_table
                        .get(module_name.as_str())
                        .cloned()
                        .unwrap_or_default();

                    let mut struct_fun_tokens =
                        Self::create_structs(&module.structs, &type_origin_table, &ctx)?;
                    struct_fun_tokens.extend(Self::create_enums(
                        &module.enums,
                        &type_origin_table,
                        &ctx,
                    ));
                    struct_fun_tokens.extend(Self::create_funs(&module.functions, &ctx));

                    Ok::<_, anyhow::Error>(if struct_fun_tokens.is_empty() {
                        quote! {}
                    } else {
                        let pkg_id_addr = latest_addr_map
                            .get(main_addr)
                            .copied()
                            .unwrap_or(*main_addr);
                        let addr_hex = addr_hex_literal(&pkg_id_addr);
                        let name_ident = module_name.to_string();
                        quote! {
                            pub mod #module_ident{
                                use move_binding_derive::{MoveStruct, Key};
                                use move_types::{MoveType, Address, Identifier};
                                use move_types::functions::{Arg, Ref, MutRef};
                                pub const PACKAGE_ID: Address = Address::from_static(#addr_hex);
                                pub const MODULE_NAME: &str = #name_ident;
                                #(#struct_fun_tokens)*
                            }
                        }
                    })
                })
                .collect::<Result<_, _>>()?;

            let package_ident = Ident::new(alias, proc_macro2::Span::call_site());
            main_pkg_tokens.push(quote! {
                #[allow(unused_imports, non_camel_case_types, non_snake_case, non_upper_case_globals)]
                pub mod #package_ident{
                    pub const PACKAGE_VERSION:u64 = #version;
                    #(#module_tokens)*
                }
            });
        }

        // Generate stubs for external dependency types, looking up defs across all summaries.
        let stub_tokens = Self::generate_stubs_multi(&summaries_refs, &latest_addr_map, &ctx)?;

        Ok(quote! {
            #(#main_pkg_tokens)*
            #(#stub_tokens)*
        })
    }

    fn generate_stubs_multi(
        summaries: &[&summary_parser::Summary],
        latest_addr_map: &HashMap<AccountAddress, AccountAddress>,
        ctx: &CodegenCtx,
    ) -> Result<Vec<TokenStream>, anyhow::Error> {
        // Resolving a stub's field types pushes more entries into ctx.external_refs
        // (transitive dep references), so walk to fixpoint, caching defs as we go.
        let mut stub_defs: HashMap<(AccountAddress, String, String), StubDef> = HashMap::new();
        loop {
            let current: Vec<_> = {
                let guard = ctx.external_refs.borrow();
                guard.iter().cloned().collect()
            };
            let new_refs: Vec<_> = current
                .into_iter()
                .filter(|r| !stub_defs.contains_key(r))
                .collect();
            if new_refs.is_empty() {
                break;
            }
            for r in new_refs {
                // Try each summary until one has the type definition.
                let def = summaries
                    .iter()
                    .map(|s| Self::lookup_stub_def(s, &r.0, &r.1, &r.2, ctx))
                    .find(|d| !d.fields.is_empty() || !d.type_params.is_empty())
                    .unwrap_or(StubDef {
                        type_params: vec![],
                        fields: vec![],
                    });
                stub_defs.insert(r, def);
            }
        }

        if stub_defs.is_empty() {
            return Ok(vec![]);
        }

        // Build reverse map: address -> package alias (first-seen wins, skip main pkgs).
        let mut addr_to_alias: HashMap<AccountAddress, String> = HashMap::new();
        for summary in summaries {
            for (name, addr) in &summary.address_mapping {
                if ctx.main_addrs.contains(addr) {
                    continue;
                }
                addr_to_alias.entry(*addr).or_insert_with(|| match *addr {
                    AccountAddress::ONE => dep_alias("std"),
                    AccountAddress::TWO => dep_alias("sui"),
                    _ => dep_alias(name.as_str()),
                });
            }
        }

        // Group external refs by (package_alias, module). BTreeMap keeps iteration
        // order stable across runs so generated code (and snapshots) are deterministic.
        let mut by_package: BTreeMap<String, BTreeMap<String, Vec<(String, AccountAddress)>>> =
            BTreeMap::new();
        for (addr, module, type_name) in stub_defs.keys() {
            let alias = match addr_to_alias.get(addr) {
                Some(a) => a.clone(),
                None => continue,
            };
            by_package
                .entry(alias)
                .or_default()
                .entry(module.clone())
                .or_default()
                .push((type_name.clone(), *addr));
        }
        // Within each module, sort types so emission order is stable.
        for modules in by_package.values_mut() {
            for types in modules.values_mut() {
                types.sort_by(|a, b| a.0.cmp(&b.0));
            }
        }

        let mut package_tokens = vec![];
        for (pkg_alias, modules) in &by_package {
            let pkg_ident = Ident::new(pkg_alias, proc_macro2::Span::call_site());

            let mut module_tokens = vec![];
            for (module_name, types) in modules {
                let module_ident = Ident::new(module_name, proc_macro2::Span::call_site());
                let module_name_str = module_name.as_str();

                let mut type_tokens = vec![];
                for (type_name, pkg_addr) in types {
                    let key = (*pkg_addr, module_name.clone(), type_name.clone());
                    let stub_def = stub_defs
                        .get(&key)
                        .context("stub def missing from cache (internal invariant)")?;
                    let origin_hex = addr_hex_literal(pkg_addr);
                    type_tokens.push(Self::generate_stub_type(type_name, stub_def, &origin_hex)?);
                }

                let orig_pkg_addr = types[0].1;
                let pkg_id_addr = latest_addr_map
                    .get(&orig_pkg_addr)
                    .copied()
                    .unwrap_or(orig_pkg_addr);
                let pkg_addr_hex = addr_hex_literal(&pkg_id_addr);

                module_tokens.push(quote! {
                    pub mod #module_ident {
                        use move_types::{MoveType, Address, Identifier, StructTag};
                        pub const PACKAGE_ID: Address = Address::from_static(#pkg_addr_hex);
                        pub const MODULE_NAME: &str = #module_name_str;
                        #(#type_tokens)*
                    }
                });
            }

            package_tokens.push(quote! {
                #[allow(unused_imports, non_camel_case_types, non_snake_case, non_upper_case_globals)]
                pub mod #pkg_ident {
                    #(#module_tokens)*
                }
            });
        }

        Ok(package_tokens)
    }

    fn lookup_stub_def(
        summary: &summary_parser::Summary,
        pkg_addr: &AccountAddress,
        module_name: &str,
        type_name: &str,
        ctx: &CodegenCtx,
    ) -> StubDef {
        let empty = StubDef {
            type_params: vec![],
            fields: vec![],
        };
        let Some(pkg) = summary.packages.packages.get(pkg_addr) else {
            return empty;
        };
        let module_sym = move_symbol_pool::Symbol::from(module_name);
        let Some(module) = pkg.modules.get(&module_sym) else {
            return empty;
        };
        let type_sym = move_symbol_pool::Symbol::from(type_name);
        if let Some(s) = module.structs.get(&type_sym) {
            // Keep None placeholders so a slice index lines up with the original
            // type-parameter index.
            let type_param_names: Vec<_> =
                s.type_parameters.iter().map(|tp| tp.tparam.name).collect();
            let type_params: Vec<_> = s.type_parameters.iter().map(|tp| tp.phantom).collect();
            let fields: Vec<_> = s
                .fields
                .fields
                .iter()
                .map(|(name, field)| {
                    (
                        escape_keyword(name),
                        field
                            .type_
                            .to_rust_type_with_context(ctx, &type_param_names),
                    )
                })
                .collect();
            return StubDef {
                type_params,
                fields,
            };
        }
        // Enums: fall back to phantom-only stub (no fields). This stub is incorrect for
        // any consumer that pattern-matches or constructs the enum.
        if let Some(e) = module.enums.get(&type_sym) {
            let type_params: Vec<_> = e.type_parameters.iter().map(|_| false).collect();
            return StubDef {
                type_params,
                fields: vec![],
            };
        }
        empty
    }

    fn generate_stub_type(
        type_name: &str,
        def: &StubDef,
        origin_addr_hex: &str,
    ) -> Result<TokenStream, anyhow::Error> {
        let type_ident = Ident::new(type_name, proc_macro2::Span::call_site());
        let name_str = type_name.to_string();
        let type_param_count = def.type_params.len();

        let field_tokens: Vec<_> = def
            .fields
            .iter()
            .map(|(name, rust_type)| {
                let field_ident = Ident::new(name, proc_macro2::Span::call_site());
                let field_type: syn::Type = syn::parse_str(rust_type).with_context(|| {
                    format!(
                        "failed to parse field type `{}` for stub `{}::{}`",
                        rust_type, type_name, name
                    )
                })?;
                Ok::<_, anyhow::Error>(quote! { pub #field_ident: #field_type, })
            })
            .collect::<Result<_, _>>()?;

        Ok(if type_param_count == 0 {
            if field_tokens.is_empty() {
                quote! {
                    #[derive(serde::Deserialize, serde::Serialize, Debug, move_binding_derive::MoveStruct)]
                    #[move_struct(origin = #origin_addr_hex)]
                    pub struct #type_ident;
                }
            } else {
                quote! {
                    #[derive(serde::Deserialize, serde::Serialize, Debug, move_binding_derive::MoveStruct)]
                    #[move_struct(origin = #origin_addr_hex)]
                    pub struct #type_ident {
                        #(#field_tokens)*
                    }
                }
            }
        } else {
            let type_params: Vec<_> = (0..type_param_count)
                .map(|i| Ident::new(&format!("T{i}"), proc_macro2::Span::call_site()))
                .collect();

            let phantom_fields: Vec<_> = def
                .type_params
                .iter()
                .enumerate()
                .filter(|(_, is_phantom)| **is_phantom)
                .map(|(i, _)| {
                    let name = Ident::new(&format!("_phantom_{i}"), proc_macro2::Span::call_site());
                    let tp = &type_params[i];
                    quote! {
                        #[serde(skip)]
                        #name: std::marker::PhantomData<#tp>,
                    }
                })
                .collect();

            let type_calls: Vec<_> = type_params.iter().map(|t| quote! { #t::type_() }).collect();

            let _ = type_calls;
            let _ = name_str;
            let all_phantom = def.type_params.iter().all(|p| *p);
            let serde_bound = if all_phantom {
                quote! { #[serde(bound(serialize = "", deserialize = ""))] }
            } else {
                quote! {}
            };
            quote! {
                #[derive(serde::Deserialize, serde::Serialize, Debug, move_binding_derive::MoveStruct)]
                #[move_struct(origin = #origin_addr_hex)]
                #serde_bound
                pub struct #type_ident<#(#type_params),*> {
                    #(#field_tokens)*
                    #(#phantom_fields)*
                }
            }
        })
    }

    fn create_structs(
        structs: &indexmap::IndexMap<move_symbol_pool::Symbol, move_model_2::summary::Struct>,
        type_origin_ids: &HashMap<String, AccountAddress>,
        ctx: &CodegenCtx,
    ) -> Result<Vec<TokenStream>, anyhow::Error> {
        structs
            .iter()
            .map(|(name, move_struct)| {
                Self::create_struct(name.as_str(), move_struct, type_origin_ids, ctx)
            })
            .collect()
    }

    fn create_struct(
        struct_name: &str,
        move_struct: &move_model_2::summary::Struct,
        type_origin_id: &HashMap<String, AccountAddress>,
        ctx: &CodegenCtx,
    ) -> Result<TokenStream, anyhow::Error> {
        let (type_parameters, phantoms) = move_struct.type_parameters.iter().enumerate().fold(
            (vec![], vec![]),
            |(mut type_parameters, mut phantoms), (i, v)| {
                let ident = Ident::new(&format!("T{i}"), proc_macro2::Span::call_site());
                type_parameters.push(quote! {#ident});

                if v.phantom {
                    let name =
                        Ident::new(&format!("phantom_data_{i}"), proc_macro2::Span::call_site());
                    let type_: syn::Type =
                        syn::parse_str(&format!("std::marker::PhantomData<T{i}>")).unwrap();
                    phantoms.push(quote! {#name: #type_,})
                }
                (type_parameters, phantoms)
            },
        );

        let struct_ident = Ident::new(&struct_name.to_string(), proc_macro2::Span::call_site());
        let type_param_names: Vec<_> = move_struct
            .type_parameters
            .iter()
            .map(|tp| tp.tparam.name)
            .collect();
        let field_tokens = move_struct
            .fields
            .fields
            .iter()
            .map(|(name, field)| {
                let field_ident = Ident::new(&escape_keyword(name), proc_macro2::Span::call_site());
                let field_type: syn::Type = syn::parse_str(
                    &field
                        .type_
                        .to_rust_type_with_context(ctx, &type_param_names),
                )?;
                Ok(quote! {pub #field_ident: #field_type,})
            })
            .collect::<Result<Vec<_>, anyhow::Error>>()?;

        let mut derives = vec![
            quote! {serde::Deserialize},
            quote! {serde::Serialize},
            quote! {Debug},
            quote! {MoveStruct},
        ];

        // hack: because move_model_2::summary::AbilitySet contents are private
        let has_key = serde_json::to_value(&move_struct.abilities)?
            .as_array()
            .context("incorrect abilities structure")?
            .iter()
            .filter_map(|v| v.as_str())
            .map(|v| v.to_string())
            .collect::<Vec<String>>()
            .contains(&"Key".to_string());

        if has_key {
            derives.push(quote! {Key});
        }

        let addr_hex = addr_hex_literal(&type_origin_id[struct_name]);
        Ok(if type_parameters.is_empty() {
            quote! {
                #[derive(#(#derives),*)]
                #[move_struct(origin = #addr_hex)]
                pub struct #struct_ident {
                    #(#field_tokens)*
                }
            }
        } else {
            quote! {
                #[derive(#(#derives),*)]
                #[move_struct(origin = #addr_hex)]
                pub struct #struct_ident<#(#type_parameters),*> {
                    #(#field_tokens)*
                    #(#phantoms)*
                }
            }
        })
    }

    fn create_enums(
        enums: &indexmap::IndexMap<move_symbol_pool::Symbol, move_model_2::summary::Enum>,
        type_origin_ids: &HashMap<String, AccountAddress>,
        ctx: &CodegenCtx,
    ) -> Vec<TokenStream> {
        enums
            .iter()
            .map(|(name, move_enum)| {
                Self::create_enum(name.as_str(), move_enum, type_origin_ids, ctx)
            })
            .collect()
    }

    fn create_enum(
        enum_name: &str,
        move_enum: &move_model_2::summary::Enum,
        type_origin_id: &HashMap<String, AccountAddress>,
        ctx: &CodegenCtx,
    ) -> TokenStream {
        let enum_ident = Ident::new(&enum_name.to_string(), proc_macro2::Span::call_site());
        let variant_tokens = move_enum.variants.iter().map(|(k, variant)| {
            let variant_ident = Ident::new(&escape_keyword(&k), proc_macro2::Span::call_site());

            if variant.fields.fields.is_empty() {
                return quote! {#variant_ident,};
            }

            if variant
                .fields
                .fields
                .iter()
                .enumerate()
                .all(|(i, (name, _))| &name.to_string() == &format!("pos{}", i))
            {
                let field_types = variant.fields.fields.iter().map(|(_, field)| {
                    let field_type: syn::Type =
                        syn::parse_str(&field.type_.to_rust_type(ctx)).unwrap();
                    quote! {#field_type,}
                });

                return quote! {
                    #variant_ident(#(#field_types)*),
                };
            }

            let field_tokens = variant.fields.fields.iter().map(|(field_name, field)| {
                let field_ident =
                    Ident::new(&escape_keyword(field_name), proc_macro2::Span::call_site());
                let field_type: syn::Type = syn::parse_str(&field.type_.to_rust_type(ctx)).unwrap();
                quote! {#field_ident: #field_type,}
            });
            quote! { #variant_ident {#(#field_tokens)*},}
        });

        let derives = vec![
            quote! {serde::Deserialize},
            quote! {serde::Serialize},
            quote! {Debug},
            quote! {MoveStruct},
        ];

        let addr_hex = addr_hex_literal(&type_origin_id[enum_name]);

        quote! {
            #[derive(#(#derives),*)]
            #[move_struct(origin = #addr_hex)]
            pub enum #enum_ident{
                #(#variant_tokens)*
            }
        }
    }

    fn create_funs(
        funs: &indexmap::IndexMap<move_symbol_pool::Symbol, move_model_2::summary::Function>,
        ctx: &CodegenCtx,
    ) -> Vec<TokenStream> {
        funs.iter()
            .flat_map(|(name, fun)| Self::create_fun(name.as_str(), fun, ctx))
            .collect()
    }

    fn create_fun(
        fun_name: &str,
        fun: &move_model_2::summary::Function,
        ctx: &CodegenCtx,
    ) -> Option<TokenStream> {
        let type_param_names: Vec<_> = fun.type_parameters.iter().map(|tp| tp.name).collect();

        let (param_names, mut params, need_lifetime) = fun.parameters.iter().enumerate().fold(
            (vec![], vec![], false),
            |(mut param_names, mut params, mut lifetime), (i, param)| {
                // TODO - hack: necessary because param fields are private
                // move_model_2::summary::Parameter
                let hack = serde_json::to_value(param).unwrap();
                let field_ident = hack
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(|name| Ident::new(&escape_keyword(name), proc_macro2::Span::call_site()))
                    .unwrap_or_else(|| {
                        Ident::new(&format!("p{i}"), proc_macro2::Span::call_site())
                    });
                let type_ = {
                    let type_attr = hack.get("type_").unwrap();
                    serde_json::from_value::<move_model_2::summary::Type>(type_attr.clone())
                        .unwrap()
                };

                lifetime = lifetime || type_.is_ref();
                if let move_model_2::summary::Type::Reference(_, to) = &type_ {
                    if let move_model_2::summary::Type::Datatype(obj) = to.as_ref() {
                        // Filter out TxContext. Resolve the address-alias Symbol to the
                        // actual AccountAddress via the tag map so this doesn't break
                        // when a summary uses a non-"sui" alias for 0x2.
                        let resolved = ctx.tags.get(&obj.module.address).copied();
                        if resolved == Some(AccountAddress::TWO)
                            && obj.module.name.as_str() == "tx_context"
                        {
                            return (param_names, params, lifetime);
                        }
                    }
                }
                param_names.push(quote! {#field_ident});
                let field_type: syn::Type =
                    syn::parse_str(&type_.to_arg_type_with_context(ctx, &type_param_names))
                        .unwrap();
                params.push(quote! {#field_ident: #field_type});
                (param_names, params, lifetime)
            },
        );

        params.insert(
            0,
            quote! {builder: &mut sui_transaction_builder::TransactionBuilder},
        );

        let returns = fun
            .return_
            .iter()
            .flat_map(|move_type| {
                syn::parse_str::<syn::Type>(
                    &move_type.to_arg_type_with_context(ctx, &type_param_names),
                )
                .ok()
            })
            .collect::<Vec<_>>();

        let (types, mut types_with_ability) = fun.type_parameters.iter().enumerate().fold(
            (vec![], vec![]),
            |(mut types, mut types_with_ability), (i, _v)| {
                let ident = Ident::new(&format!("T{i}"), proc_macro2::Span::call_site());
                types.push(ident.clone());
                let mut abilities = vec![];
                // Note: summary::TParam doesn't have the same ability methods as normalized
                // For now, just use MoveType as default
                abilities.push(quote! {MoveType});
                types_with_ability.push(quote! {#ident: #(#abilities)+*});
                (types, types_with_ability)
            },
        );

        if need_lifetime || fun.return_.iter().any(|t| t.is_ref()) {
            types_with_ability.insert(0, quote! {'a})
        }

        let fun_ident = Ident::new(&escape_keyword(fun_name), proc_macro2::Span::call_site());

        let type_args_call = if types.is_empty() {
            quote! {}
        } else {
            quote! { .with_type_args(vec![#(#types::type_()),*]) }
        };
        let call_expr = quote! {
            builder.move_call(
                sui_transaction_builder::Function::new(
                    PACKAGE_ID,
                    Identifier::from_static(MODULE_NAME),
                    Identifier::from_static(#fun_name),
                )#type_args_call,
                vec![#(#param_names.into()),*],
            )
        };

        let (maybe_returns, body) = match returns.len() {
            0 => (quote! {}, quote! { #call_expr; }),
            1 => {
                let t = &returns[0];
                (quote! { -> #t }, quote! { #call_expr.into() })
            }
            n => {
                let ret_tuple = quote! { -> (#(#returns),*) };
                let nested_idents: Vec<_> = (0..n)
                    .map(|i| Ident::new(&format!("__r{i}"), proc_macro2::Span::call_site()))
                    .collect();
                let indices: Vec<_> = (0..n).collect();
                let body = quote! {
                    let __ret = #call_expr.to_nested(#n);
                    #(let #nested_idents = __ret[#indices].into();)*
                    (#(#nested_idents),*)
                };
                (ret_tuple, body)
            }
        };

        let sig = if types_with_ability.is_empty() {
            quote! {
                pub fn #fun_ident(#(#params),*) #maybe_returns
            }
        } else {
            quote! {
                pub fn #fun_ident <#(#types_with_ability),*>(#(#params),*) #maybe_returns
            }
        };

        let fun_impl = quote! {
            #sig {
                #body
            }
        };
        Some(fun_impl)
    }
}

#[cfg(test)]
mod tests {
    use super::dep_alias;

    #[test]
    fn dep_alias_escapes_std_and_keywords() {
        assert_eq!(dep_alias("std"), "move_std");
        assert_eq!(dep_alias("move"), "move_move");
        assert_eq!(dep_alias("type"), "move_type");
        assert_eq!(dep_alias("self"), "move_self");
        assert_eq!(dep_alias("crate"), "move_crate");
        assert_eq!(dep_alias("sui"), "sui");
        assert_eq!(dep_alias("bridge"), "bridge");
    }
}
