use anyhow::{Context, Result};
use clap::Parser;
use move_binary_format::CompiledModule;
use move_binding::move_codegen::LocalCodegen;
use move_core_types::account_address::AccountAddress;
use move_model_2::compiled_model::Model;
use move_symbol_pool::Symbol;
use std::collections::BTreeMap;
use std::{io::Write, path::PathBuf};
use sui_rpc::proto::sui::rpc::v2::{GetObjectRequest, GetPackageRequest, Package};
use sui_sdk_types::Address;
use tokio::runtime::Runtime;

#[derive(Parser)]
#[clap(
    name = "move-binding-package-summaries",
    about = "Generate unified Rust bindings from one or more package_summaries folders"
)]
struct Cli {
    /// The Rust import prefix for the generated module.
    #[clap(short, long, default_value = "crate")]
    base_path: String,

    /// Network for on-chain packages (mainnet|testnet|devnet).
    #[clap(short, long, default_value = "mainnet")]
    network: Network,

    /// One or more inputs as `alias=path` (local summaries dir) or
    /// `alias=0xADDRESS` (on-chain package). Repeat for each main package.
    #[clap(short, long = "package", required = true)]
    packages: Vec<PackageRef>,

    /// Output file. Defaults to `./bindings.rs`.
    #[clap(short, long, default_value = "./bindings.rs")]
    output: PathBuf,
}

#[derive(Clone, Copy)]
enum Network {
    Mainnet,
    Testnet,
    Devnet,
}

impl Network {
    fn rpc_url(self) -> &'static str {
        match self {
            Network::Mainnet => sui_rpc::Client::MAINNET_FULLNODE,
            Network::Testnet => sui_rpc::Client::TESTNET_FULLNODE,
            Network::Devnet => sui_rpc::Client::DEVNET_FULLNODE,
        }
    }
}

impl std::str::FromStr for Network {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "main" | "mainnet" => Ok(Network::Mainnet),
            "test" | "testnet" => Ok(Network::Testnet),
            "dev" | "devnet" => Ok(Network::Devnet),
            other => Err(format!(
                "unknown network '{other}' (expected mainnet, testnet, or devnet)"
            )),
        }
    }
}

impl std::fmt::Display for Network {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Network::Mainnet => "mainnet",
            Network::Testnet => "testnet",
            Network::Devnet => "devnet",
        })
    }
}

#[derive(Clone)]
enum PackageRef {
    Onchain {
        address: Address,
        alias: String,
    },
    Local {
        path: std::path::PathBuf,
        alias: String,
    },
}

impl std::str::FromStr for PackageRef {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (alias, value) = s
            .split_once('=')
            .ok_or_else(|| format!("expected alias=path-or-address, got '{s}'"))?;
        if alias.is_empty() || value.is_empty() {
            return Err(format!("alias and path/address must be non-empty in '{s}'"));
        }
        let alias = alias.to_string();
        if value.starts_with("0x") {
            let address = value
                .parse()
                .map_err(|e| format!("invalid on-chain address '{value}': {e}"))?;
            Ok(PackageRef::Onchain { address, alias })
        } else {
            Ok(PackageRef::Local {
                path: value.into(),
                alias,
            })
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let markup = handle_packages(&cli.base_path, cli.network, cli.packages)?;

    std::fs::File::create(&cli.output)?.write_all(markup.as_bytes())?;
    println!("Wrote bindings to {}", cli.output.display());

    Ok(())
}

fn handle_packages(
    base_path: &str,
    network: Network,
    packages: Vec<PackageRef>,
) -> anyhow::Result<String> {
    let rt = Runtime::new().context("failed to create tokio runtime")?;
    // A single gRPC client for the whole run, created only when an on-chain
    // package is actually requested so all-local runs stay offline.
    let mut client = packages
        .iter()
        .any(|p| matches!(p, PackageRef::Onchain { .. }))
        .then(|| sui_rpc::Client::new(network.rpc_url()))
        .transpose()
        .with_context(|| format!("failed to connect to {network} gRPC"))?;

    let summaries = packages
        .iter()
        .map(|package| match package {
            PackageRef::Local { path, alias } => {
                let summary = summary_parser::parse_summaries(path.as_path())
                    .with_context(|| format!("failed to parse summaries at {}", path.display()))?;
                Ok::<_, anyhow::Error>((summary, alias.clone()))
            }
            PackageRef::Onchain { address, alias } => {
                let client = client
                    .as_mut()
                    .expect("client is created whenever an on-chain package is present");
                let summary = rt
                    .block_on(fetch_summary_from_chain(client, address))
                    .with_context(|| format!("failed to fetch on-chain package {address}"))?;
                Ok((summary, alias.clone()))
            }
        })
        .collect::<Result<Vec<_>, _>>()?;

    let inputs: Vec<(&_, &str)> = summaries.iter().map(|(s, a)| (s, a.as_str())).collect();

    let stream = LocalCodegen::expand_multi(&inputs, base_path, Some(2))?;
    let markup = prettyplease::unparse(&syn::parse2(stream)?);

    Ok(markup)
}

/// On-chain analogue of `parse_summaries`: fetch a package (and every package
/// in its linkage table) over gRPC, run the bytecode through move-model-2, and
/// return the SAME `Summary` shape as the on-disk path.
///
/// Both sources converge on `move_model_2::summary::Packages`, and the
/// type-origin table is derived with the shared
/// `build_type_origin_table_from_summaries` so the two paths can't drift.
///
/// `client` is a fullnode gRPC client; `package_id` is the on-chain (storage)
/// object id of the package.
async fn fetch_summary_from_chain(
    client: &mut sui_rpc::Client,
    package_id: &Address,
) -> Result<summary_parser::Summary> {
    // Root package + every package in its linkage table.
    let root = fetch_package(client, package_id)
        .await
        .with_context(|| format!("Failed to fetch root package {}", package_id))?;

    let mut rpc_packages = vec![root.clone()];
    for link in &root.linkage {
        if let Some(dep_id) = &link.upgraded_id {
            let dep_addr: Address = dep_id
                .parse()
                .with_context(|| format!("Invalid linkage address '{}'", dep_id))?;
            let dep = fetch_package(client, &dep_addr)
                .await
                .with_context(|| format!("Failed to fetch dependency package {}", dep_addr))?;
            rpc_packages.push(dep);
        }
    }

    // Bytecode -> move-model-2 -> the exact `Packages` type `parse_packages`
    // produces from disk.
    let mut modules = Vec::new();
    for package in &rpc_packages {
        for module in &package.modules {
            let bytes = module
                .contents
                .as_deref()
                .with_context(|| format!("Module '{}' is missing bytecode", module.name()))?;
            let compiled = CompiledModule::deserialize_with_defaults(bytes)
                .with_context(|| format!("Failed to deserialize module '{}'", module.name()))?;
            modules.push(compiled);
        }
    }

    let model = Model::from_compiled(&BTreeMap::new(), modules);

    let packages: move_model_2::summary::Packages =
        serde_json::from_value(serde_json::to_value(model.summary())?)
            .context("Failed to materialize owned Packages from model summary")?;

    // address_mapping: same convention as the CLI's `--bytecode` branch, where
    // there are no named addresses so the canonical address string is the key.
    let address_mapping: BTreeMap<Symbol, AccountAddress> = packages
        .packages
        .keys()
        .map(|addr| (Symbol::from(addr.to_canonical_string(true)), *addr))
        .collect();

    // additional_metadata: same JSON shape `parse_metadata` reads off disk
    // (the `PackageSummaryMetadata` fields).
    let additional_metadata = Some(serde_json::json!({
        "root_package_id": root.storage_id,
        "root_package_original_id": root.original_id,
        "root_package_version": root.version,
        "linkage": root.linkage,
        "type_origins": root.type_origins,
    }));

    Ok(summary_parser::Summary {
        type_origin_table: summary_parser::build_type_origin_table(
            &packages,
            additional_metadata.as_ref(),
        ),
        packages,
        address_mapping,
        additional_metadata,
    })
}

/// Fetch a single package's full data.
///
/// Neither RPC alone is sufficient: `GetPackage` resolves the id fields
/// (`storage_id`/`original_id`/`version`) but the fullnode does not include raw
/// module bytecode in its response, while `GetObject` reads the on-chain
/// `MovePackage` (module bytecode + `type_origins` + the `linkage` table used to
/// walk deps) but omits those id fields. So we take the bytecode-bearing package
/// from `GetObject` and graft the ids on from `GetPackage`.
async fn fetch_package(client: &mut sui_rpc::Client, package_id: &Address) -> Result<Package> {
    let meta = client
        .package_client()
        .get_package(GetPackageRequest::new(package_id))
        .await
        .with_context(|| format!("GetPackage RPC failed for {}", package_id))?
        .into_inner()
        .package
        .with_context(|| format!("No package returned for {}", package_id))?;

    let mut package = client
        .ledger_client()
        .get_object(
            GetObjectRequest::default()
                .with_object_id(package_id.to_string())
                .with_read_mask(sui_rpc::field::FieldMask {
                    paths: vec!["package".to_string()],
                }),
        )
        .await
        .with_context(|| format!("GetObject RPC failed for {}", package_id))?
        .into_inner()
        .object
        .with_context(|| format!("No object returned for {}", package_id))?
        .package
        .with_context(|| format!("Object {} is not a Move package", package_id))?;

    package.storage_id = meta.storage_id;
    package.original_id = meta.original_id;
    package.version = meta.version;

    Ok(package)
}
