use crate::package_id_resolver::PackageIdResolver;
use crate::SuiNetwork;
use fastcrypto::encoding::{Base64, Encoding};
use move_binary_format::normalized::Module;
use move_core_types::account_address::AccountAddress;
use reqwest::header::CONTENT_TYPE;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;

pub trait ModuleProvider {
    fn get_package(&self, package_id: &str) -> Result<Package, anyhow::Error>;
}

pub struct MoveModuleProvider {
    network: SuiNetwork,
}

impl MoveModuleProvider {
    pub fn new(network: SuiNetwork) -> Self {
        Self { network }
    }
}

impl ModuleProvider for MoveModuleProvider {
    fn get_package(&self, package: &str) -> Result<Package, anyhow::Error> {
        let package_id = PackageIdResolver::resolve_package_id(self.network, package)?;
        let client = reqwest::blocking::Client::new();
        let request = format!(
            r#"{{package(address: "{package_id}") {{moduleBcs, typeOrigins{{module, struct, definingId}}, version}}}}"#
        );
        let res = client
            .post(self.network.gql())
            .header(CONTENT_TYPE, "application/json")
            .json(&json!({
                "query": request,
                "variables": Value::Null
            }))
            .send()
            .ok()
            .expect("Error fetching package from Sui GQL.");

        let value = res.json::<Value>().unwrap();
        let module_bcs: String =
            serde_json::from_value(value["data"]["package"]["moduleBcs"].clone()).unwrap();
        let module_bytes = Base64::decode(&module_bcs).unwrap();
        let module_map: BTreeMap<String, Vec<u8>> = bcs::from_bytes(&module_bytes).unwrap();

        let pool = &mut move_binary_format::normalized::RcPool::new();
        let module_map: BTreeMap<_, _> = module_map
            .iter()
            .map(|(name, bytes)| {
                let module = move_binary_format::CompiledModule::deserialize_with_defaults(bytes)?;
                let normalized = Module::new(pool, &module, false);
                Ok::<_, anyhow::Error>((name.clone(), normalized))
            })
            .collect::<Result<_, _>>()?;

        let type_origin_table: Vec<Value> =
            serde_json::from_value(value["data"]["package"]["typeOrigins"].clone())?;

        let type_origin_table = type_origin_table.iter().fold(
            HashMap::new(),
            |mut results: HashMap<String, HashMap<String, AccountAddress>>, v| {
                let module = v["module"].as_str().unwrap();
                let struct_ = v["struct"].as_str().unwrap();
                let defining_id = v["definingId"].as_str().unwrap();
                results.entry(module.to_string()).or_default().insert(
                    struct_.to_string(),
                    AccountAddress::from_str(defining_id).unwrap(),
                );
                results
            },
        );

        let version = serde_json::from_value(value["data"]["package"]["version"].clone())?;

        Ok(Package {
            module_map,
            type_origin_table,
            version,
        })
    }
}

pub struct Package {
    pub module_map: BTreeMap<String, Module<move_binary_format::normalized::RcIdentifier>>,
    pub type_origin_table: HashMap<String, HashMap<String, AccountAddress>>,
    pub version: u64,
}
