// Copyright (c) Aptos
// SPDX-License-Identifier: Apache-2.0
use crate::{
    CryptoStorage, Error, GetResponse, GitHubStorage, InMemoryStorage, KVStorage, Namespaced,
    OnDiskStorage, PublicKeyResponse, VaultStorage,
};
use aptos_crypto::ed25519::{Ed25519PrivateKey, Ed25519PublicKey, Ed25519Signature};
use enum_dispatch::enum_dispatch;
use serde::{de::DeserializeOwned, Serialize};

/// This is the interface into secure storage. Any storage engine implementing this trait
/// should support both key/value operations (e.g., get, set and create) and cryptographic key
/// operations (e.g., generate_key, sign and rotate_key).

/// This is a hack that allows us to convert from SecureBackend into a useable
/// T: Storage. This boilerplate can be 100% generated by a proc macro.
#[enum_dispatch(KVStorage, CryptoStorage)]
pub enum Storage {
    GitHubStorage(GitHubStorage),
    VaultStorage(VaultStorage),
    InMemoryStorage(InMemoryStorage),
    NamespacedStorage(Namespaced<Box<Storage>>),
    OnDiskStorage(OnDiskStorage),
}

impl KVStorage for Box<Storage> {
    fn available(&self) -> Result<(), Error> {
        Storage::available(self)
    }

    fn get<T: DeserializeOwned>(&self, key: &str) -> Result<GetResponse<T>, Error> {
        Storage::get(self, key)
    }

    fn set<T: Serialize>(&mut self, key: &str, value: T) -> Result<(), Error> {
        Storage::set(self, key, value)
    }

    #[cfg(any(test, feature = "testing"))]
    fn reset_and_clear(&mut self) -> Result<(), Error> {
        Storage::reset_and_clear(self)
    }
}

impl CryptoStorage for Box<Storage> {
    fn create_key(&mut self, name: &str) -> Result<Ed25519PublicKey, Error> {
        Storage::create_key(self, name)
    }

    fn export_private_key(&self, name: &str) -> Result<Ed25519PrivateKey, Error> {
        Storage::export_private_key(self, name)
    }

    fn import_private_key(&mut self, name: &str, key: Ed25519PrivateKey) -> Result<(), Error> {
        Storage::import_private_key(self, name, key)
    }

    fn export_private_key_for_version(
        &self,
        name: &str,
        version: Ed25519PublicKey,
    ) -> Result<Ed25519PrivateKey, Error> {
        Storage::export_private_key_for_version(self, name, version)
    }

    fn get_public_key(&self, name: &str) -> Result<PublicKeyResponse, Error> {
        Storage::get_public_key(self, name)
    }

    fn get_public_key_previous_version(&self, name: &str) -> Result<Ed25519PublicKey, Error> {
        Storage::get_public_key_previous_version(self, name)
    }

    fn rotate_key(&mut self, name: &str) -> Result<Ed25519PublicKey, Error> {
        Storage::rotate_key(self, name)
    }

    fn sign<T: aptos_crypto::hash::CryptoHash + Serialize>(
        &self,
        name: &str,
        message: &T,
    ) -> Result<Ed25519Signature, Error> {
        Storage::sign(self, name, message)
    }

    fn sign_using_version<T: aptos_crypto::hash::CryptoHash + Serialize>(
        &self,
        name: &str,
        version: Ed25519PublicKey,
        message: &T,
    ) -> Result<Ed25519Signature, Error> {
        Storage::sign_using_version(self, name, version, message)
    }
}
