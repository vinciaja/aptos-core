// Copyright (c) Aptos
// SPDX-License-Identifier: Apache-2.0

use crate::{
    backend::k8s::node::K8sNode, create_k8s_client, query_sequence_numbers, remove_helm_release,
    set_validator_image_tag, ChainInfo, FullNode, Node, Result, Swarm, Validator, Version,
};
use ::aptos_logger::*;
use anyhow::{anyhow, bail, format_err};
use aptos_config::config::NodeConfig;
use aptos_sdk::{
    crypto::ed25519::Ed25519PrivateKey,
    types::{
        chain_id::{ChainId, NamedChain},
        AccountKey, LocalAccount, PeerId,
    },
};
use k8s_openapi::api::core::v1::Service;
use kube::{
    api::{Api, ListParams},
    client::Client as K8sClient,
};
use std::{collections::HashMap, convert::TryFrom, env, process::Command, str, sync::Arc};
use tokio::time::Duration;

const JSON_RPC_PORT: u32 = 80;
const REST_API_PORT: u32 = 80;
const VALIDATOR_LB: &str = "validator-validator-lb";
const FULLNODES_LB: &str = "validator-fullnode-lb";

pub struct K8sSwarm {
    validators: HashMap<PeerId, K8sNode>,
    fullnodes: HashMap<PeerId, K8sNode>,
    root_account: LocalAccount,
    kube_client: K8sClient,
    cluster_name: String,
    helm_repo: String,
    versions: Arc<HashMap<Version, String>>,
    pub chain_id: ChainId,
}

impl K8sSwarm {
    pub async fn new(
        root_key: &[u8],
        cluster_name: &str,
        helm_repo: &str,
        image_tag: &str,
        base_image_tag: &str,
        init_image_tag: &str,
        era: &str,
    ) -> Result<Self> {
        let kube_client = create_k8s_client().await;
        let validators = get_validators(kube_client.clone(), init_image_tag).await?;
        let fullnodes = get_fullnodes(kube_client.clone(), init_image_tag, era).await?;

        let client = validators.values().next().unwrap().rest_client();
        let key = load_root_key(root_key);
        let account_key = AccountKey::from_private_key(key);
        let address = aptos_sdk::types::account_config::aptos_root_address();
        let sequence_number = query_sequence_numbers(&client, &[address])
            .await
            .map_err(|e| {
                format_err!(
                    "query_sequence_numbers on {:?} for dd account failed: {}",
                    client,
                    e
                )
            })?[0];
        let root_account = LocalAccount::new(address, account_key, sequence_number);

        let mut versions = HashMap::new();
        let base_version = Version::new(0, base_image_tag.to_string());
        let cur_version = Version::new(1, image_tag.to_string());
        versions.insert(cur_version, image_tag.to_string());
        versions.insert(base_version, base_image_tag.to_string());

        Ok(Self {
            validators,
            fullnodes,
            root_account,
            kube_client,
            chain_id: ChainId::new(NamedChain::DEVNET.id()),
            cluster_name: cluster_name.to_string(),
            helm_repo: helm_repo.to_string(),
            versions: Arc::new(versions),
        })
    }

    fn get_rest_api_url(&self) -> String {
        self.validators
            .values()
            .next()
            .unwrap()
            .rest_api_endpoint()
            .to_string()
    }

    #[allow(dead_code)]
    fn get_kube_client(&self) -> K8sClient {
        self.kube_client.clone()
    }
}

#[async_trait::async_trait]
impl Swarm for K8sSwarm {
    async fn health_check(&mut self) -> Result<()> {
        let nodes = self.validators.values().collect();
        let unhealthy_nodes = nodes_healthcheck(nodes).await.unwrap();
        if !unhealthy_nodes.is_empty() {
            bail!("Unhealthy nodes: {:?}", unhealthy_nodes)
        }

        Ok(())
    }

    fn validators<'a>(&'a self) -> Box<dyn Iterator<Item = &'a dyn Validator> + 'a> {
        Box::new(self.validators.values().map(|v| v as &'a dyn Validator))
    }

    fn validators_mut<'a>(&'a mut self) -> Box<dyn Iterator<Item = &'a mut dyn Validator> + 'a> {
        Box::new(
            self.validators
                .values_mut()
                .map(|v| v as &'a mut dyn Validator),
        )
    }

    fn validator(&self, id: PeerId) -> Option<&dyn Validator> {
        self.validators.get(&id).map(|v| v as &dyn Validator)
    }

    fn validator_mut(&mut self, id: PeerId) -> Option<&mut dyn Validator> {
        self.validators
            .get_mut(&id)
            .map(|v| v as &mut dyn Validator)
    }

    fn upgrade_validator(&mut self, id: PeerId, version: &Version) -> Result<()> {
        let validator = self
            .validators
            .get_mut(&id)
            .ok_or_else(|| anyhow!("Invalid id: {}", id))?;
        let version = self
            .versions
            .get(version)
            .cloned()
            .ok_or_else(|| anyhow!("Invalid version: {:?}", version))?;
        set_validator_image_tag(validator.name(), &version, &self.helm_repo)
    }

    fn full_nodes<'a>(&'a self) -> Box<dyn Iterator<Item = &'a dyn FullNode> + 'a> {
        Box::new(self.fullnodes.values().map(|v| v as &'a dyn FullNode))
    }

    fn full_nodes_mut<'a>(&'a mut self) -> Box<dyn Iterator<Item = &'a mut dyn FullNode> + 'a> {
        Box::new(
            self.fullnodes
                .values_mut()
                .map(|v| v as &'a mut dyn FullNode),
        )
    }

    fn full_node(&self, id: PeerId) -> Option<&dyn FullNode> {
        self.fullnodes.get(&id).map(|v| v as &dyn FullNode)
    }

    fn full_node_mut(&mut self, id: PeerId) -> Option<&mut dyn FullNode> {
        self.fullnodes.get_mut(&id).map(|v| v as &mut dyn FullNode)
    }

    fn add_validator(&mut self, _version: &Version, _template: NodeConfig) -> Result<PeerId> {
        todo!()
    }

    fn remove_validator(&mut self, id: PeerId) -> Result<()> {
        remove_helm_release(self.validator(id).unwrap().name())
    }

    fn add_full_node(&mut self, _version: &Version, _template: NodeConfig) -> Result<PeerId> {
        todo!()
    }

    fn remove_full_node(&mut self, _id: PeerId) -> Result<()> {
        todo!()
    }

    fn versions<'a>(&'a self) -> Box<dyn Iterator<Item = Version> + 'a> {
        Box::new(self.versions.keys().cloned())
    }

    fn chain_info(&mut self) -> ChainInfo<'_> {
        let rest_api_url = self.get_rest_api_url();
        ChainInfo::new(&mut self.root_account, rest_api_url, self.chain_id)
    }

    // Returns env CENTRAL_LOGGING_ADDRESS if present (without timestamps)
    // otherwise returns a kubectl logs command to retrieve the logs manually
    fn logs_location(&mut self) -> String {
        if let Ok(central_logging_address) = std::env::var("CENTRAL_LOGGING_ADDRESS") {
            central_logging_address
        } else {
            let hostname_output = Command::new("hostname")
                .output()
                .expect("failed to get pod hostname");
            let hostname = String::from_utf8(hostname_output.stdout).unwrap();
            format!(
                "aws eks --region us-west-2 update-kubeconfig --name {} && kubectl logs {}",
                &self.cluster_name, hostname
            )
        }
    }
}

pub(crate) fn k8s_retry_strategy() -> impl Iterator<Item = Duration> {
    aptos_retrier::exp_retry_strategy(1000, 10000, 50)
}

#[derive(Clone, Debug)]
pub struct KubeService {
    pub name: String,
    pub host_ip: String,
}

impl TryFrom<Service> for KubeService {
    type Error = anyhow::Error;

    fn try_from(service: Service) -> Result<Self, Self::Error> {
        let metadata = service.metadata;
        let name = metadata
            .name
            .ok_or_else(|| format_err!("node name not found"))?;
        let spec = service
            .spec
            .ok_or_else(|| format_err!("spec not found for node"))?;
        let host_ip = spec.cluster_ip.unwrap_or_default();
        Ok(Self { name, host_ip })
    }
}

async fn list_services(client: K8sClient) -> Result<Vec<KubeService>> {
    let node_api: Api<Service> = Api::all(client);
    let lp = ListParams::default();
    let services = node_api.list(&lp).await?.items;
    services.into_iter().map(KubeService::try_from).collect()
}

pub(crate) async fn get_validators(
    client: K8sClient,
    image_tag: &str,
) -> Result<HashMap<PeerId, K8sNode>> {
    let services = list_services(client).await?;
    let validators = services
        .into_iter()
        .filter(|s| s.name.contains(VALIDATOR_LB))
        .map(|s| {
            let node_id = parse_node_id(&s.name).expect("error to parse node id");
            let node = K8sNode {
                name: format!("val{}", node_id),
                sts_name: format!("val{}-aptos-validator-validator", node_id),
                // TODO: fetch this from running node
                peer_id: PeerId::random(),
                node_id,
                ip: s.host_ip.clone(),
                port: JSON_RPC_PORT,
                rest_api_port: REST_API_PORT,
                dns: s.name,
                version: Version::new(0, image_tag.to_string()),
            };
            (node.peer_id(), node)
        })
        .collect::<HashMap<_, _>>();
    let all_nodes = validators.values().collect();
    let unhealthy_nodes = nodes_healthcheck(all_nodes).await.unwrap();
    let mut health_nodes = HashMap::new();
    for node in validators {
        if !unhealthy_nodes.contains(&node.1.name) {
            health_nodes.insert(node.0, node.1);
        }
    }

    Ok(health_nodes)
}

pub(crate) async fn get_fullnodes(
    client: K8sClient,
    image_tag: &str,
    era: &str,
) -> Result<HashMap<PeerId, K8sNode>> {
    let services = list_services(client).await?;
    let fullnodes = services
        .into_iter()
        .filter(|s| s.name.contains(FULLNODES_LB))
        .map(|s| {
            let node_id = parse_node_id(&s.name).expect("error to parse node id");
            let node = K8sNode {
                name: format!("val{}", node_id),
                sts_name: format!("val{}-aptos-validator-fullnode-e{}", node_id, era),
                // TODO: fetch this from running node
                peer_id: PeerId::random(),
                node_id,
                ip: s.host_ip.clone(),
                port: JSON_RPC_PORT,
                rest_api_port: REST_API_PORT,
                dns: s.name,
                version: Version::new(0, image_tag.to_string()),
            };
            (node.peer_id(), node)
        })
        .collect::<HashMap<_, _>>();

    Ok(fullnodes)
}

fn parse_node_id(s: &str) -> Result<usize> {
    let v = s.split('-').collect::<Vec<&str>>();
    if v.len() < 5 {
        return Err(format_err!("Failed to parse {:?} node id format", s));
    }
    let idx: usize = v[0][3..].parse().unwrap();
    Ok(idx)
}

fn load_root_key(root_key_bytes: &[u8]) -> Ed25519PrivateKey {
    Ed25519PrivateKey::try_from(root_key_bytes).unwrap()
}

pub async fn nodes_healthcheck(nodes: Vec<&K8sNode>) -> Result<Vec<String>> {
    let mut unhealthy_nodes = vec![];
    for node in nodes {
        let node_name = node.name().to_string();
        println!("Attempting health check: {}", node_name);
        // perform healthcheck with retry, returning unhealthy
        let check = aptos_retrier::retry_async(k8s_retry_strategy(), || {
            Box::pin(async move {
                println!("Attempting health check: {}", node.name());
                match node.rest_client().get_ledger_information().await {
                    Ok(_) => {
                        println!("Node {} healthy", node.name());
                        Ok(())
                    }
                    Err(x) => {
                        debug!("K8s Node {} unhealthy: {}", node.name(), &x);
                        Err(x)
                    }
                }
            })
        })
        .await;
        if check.is_err() {
            unhealthy_nodes.push(node_name);
        }
    }
    if !unhealthy_nodes.is_empty() {
        debug!("Unhealthy validators after cleanup: {:?}", unhealthy_nodes);
    }

    Ok(unhealthy_nodes)
}
