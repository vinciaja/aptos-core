// Copyright (c) Aptos
// SPDX-License-Identifier: Apache-2.0

use crate::{Factory, GenesisConfig, Result, Swarm, Version};
use anyhow::{bail, format_err};
use rand::rngs::StdRng;
use std::{env, fs::File, io::Read, num::NonZeroUsize, path::PathBuf};
use tokio::runtime::Runtime;

mod cluster_helper;
mod node;
mod swarm;

pub use cluster_helper::*;
pub use node::K8sNode;
pub use swarm::*;

use aptos_sdk::crypto::ed25519::ED25519_PRIVATE_KEY_LENGTH;
use aptos_secure_storage::{CryptoStorage, KVStorage, VaultStorage};

pub struct K8sFactory {
    root_key: [u8; ED25519_PRIVATE_KEY_LENGTH],
    cluster_name: String,
    helm_repo: String,
    image_tag: String,
    base_image_tag: String,
}

impl K8sFactory {
    pub fn new(
        cluster_name: String,
        helm_repo: String,
        image_tag: String,
        base_image_tag: String,
    ) -> Result<K8sFactory> {
        let vault_addr = env::var("VAULT_ADDR")
            .map_err(|_| format_err!("Expected environment variable VAULT_ADDR"))?;
        let vault_cacert = env::var("VAULT_CACERT")
            .map_err(|_| format_err!("Expected environment variable VAULT_CACERT"))?;
        let vault_token = env::var("VAULT_TOKEN")
            .map_err(|_| format_err!("Expected environment variable VAULT_TOKEN"))?;

        let vault_cacert_path = PathBuf::from(vault_cacert.clone());

        let mut vault_cacert_file = File::open(vault_cacert_path)
            .map_err(|_| format_err!("Failed to open VAULT_CACERT file at {}", &vault_cacert))?;
        let mut vault_cacert_contents = String::new();
        vault_cacert_file
            .read_to_string(&mut vault_cacert_contents)
            .map_err(|_| format_err!("Failed to read VAULT_CACERT file at {}", &vault_cacert))?;

        let vault = VaultStorage::new(
            vault_addr,
            vault_token,
            Some(vault_cacert_contents),
            None,
            false,
            None,
            None,
        );
        vault.available()?;
        let root_key = vault
            .export_private_key("aptos__aptos_root")
            .unwrap()
            .to_bytes();

        Ok(Self {
            root_key,
            cluster_name,
            helm_repo,
            image_tag,
            base_image_tag,
        })
    }
}

impl Drop for K8sFactory {
    // When the K8sSwarm struct goes out of scope we need to wipe the chain state and scale down
    fn drop(&mut self) {
        uninstall_from_k8s_cluster().unwrap();
        let runtime = Runtime::new().unwrap();
        runtime
            .block_on(set_eks_nodegroup_size(self.cluster_name.clone(), 0, true))
            .unwrap();
    }
}

#[async_trait::async_trait]
impl Factory for K8sFactory {
    fn versions<'a>(&'a self) -> Box<dyn Iterator<Item = Version> + 'a> {
        let version = vec![
            Version::new(0, self.base_image_tag.clone()),
            Version::new(1, self.image_tag.clone()),
        ];
        Box::new(version.into_iter())
    }

    async fn launch_swarm(
        &self,
        _rng: &mut StdRng,
        node_num: NonZeroUsize,
        init_version: &Version,
        genesis_version: &Version,
        genesis_config: Option<&GenesisConfig>,
    ) -> Result<Box<dyn Swarm>> {
        let genesis_modules_path = match genesis_config {
            Some(config) => match config {
                GenesisConfig::Bytes(_) => {
                    bail!("k8s forge backend does not support raw bytes as genesis modules. please specify a path instead")
                }
                GenesisConfig::Path(path) => Some(path.clone()),
            },
            None => None,
        };

        set_eks_nodegroup_size(self.cluster_name.clone(), node_num.get(), true).await?;
        uninstall_from_k8s_cluster()?;
        let era = clean_k8s_cluster(
            self.helm_repo.clone(),
            node_num.get(),
            format!("{}", init_version),
            format!("{}", genesis_version),
            false,
            genesis_modules_path,
        )
        .await?;

        let swarm = K8sSwarm::new(
            &self.root_key,
            &self.cluster_name,
            &self.helm_repo,
            &self.image_tag,
            &self.base_image_tag,
            format!("{}", init_version).as_str(),
            &era,
        )
        .await
        .unwrap();
        Ok(Box::new(swarm))
    }
}
