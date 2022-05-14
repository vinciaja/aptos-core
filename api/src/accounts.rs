// Copyright (c) Aptos
// SPDX-License-Identifier: Apache-2.0

use crate::{
    context::Context,
    failpoint::fail_point,
    metrics::metrics,
    param::{AddressParam, LedgerVersionParam, MoveIdentifierParam, MoveStructTagParam},
    version::Version,
};

use aptos_api_types::{
    AccountData, Address, AsConverter, Error, LedgerInfo, MoveModuleBytecode, Response,
    TransactionId,
};
use aptos_types::{
    account_config::AccountResource,
    account_state::AccountState,
    event::{EventHandle, EventKey},
};

use anyhow::Result;
use aptos_types::{access_path::AccessPath, state_store::state_key::StateKey};
use move_deps::move_core_types::{
    identifier::Identifier,
    language_storage::{ResourceKey, StructTag},
    move_resource::MoveStructType,
    value::MoveValue,
};
use std::convert::TryInto;
use warp::{filters::BoxedFilter, Filter, Rejection, Reply};

// GET /accounts/<address>
pub fn get_account(context: Context) -> BoxedFilter<(impl Reply,)> {
    warp::path!("accounts" / AddressParam)
        .and(warp::get())
        .and(context.filter())
        .and_then(handle_get_account)
        .with(metrics("get_account"))
        .boxed()
}

// GET /accounts/<address>/resources
pub fn get_account_resources(context: Context) -> BoxedFilter<(impl Reply,)> {
    warp::path!("accounts" / AddressParam / "resources")
        .and(warp::get())
        .and(context.filter())
        .and(warp::query::<Version>())
        .map(|address, ctx, version: Version| (version.version, address, ctx))
        .untuple_one()
        .and_then(handle_get_account_resources)
        .with(metrics("get_account_resources"))
        .boxed()
}

// GET /accounts/<address>/modules
pub fn get_account_modules(context: Context) -> BoxedFilter<(impl Reply,)> {
    warp::path!("accounts" / AddressParam / "modules")
        .and(warp::get())
        .and(context.filter())
        .and(warp::query::<Version>())
        .map(|address, ctx, version: Version| (version.version, address, ctx))
        .untuple_one()
        .and_then(handle_get_account_modules)
        .with(metrics("get_account_modules"))
        .boxed()
}

async fn handle_get_account(
    address: AddressParam,
    context: Context,
) -> Result<impl Reply, Rejection> {
    fail_point("endpoint_get_account")?;
    Ok(Account::new(None, address, context)?.account()?)
}

async fn handle_get_account_resources(
    ledger_version: Option<LedgerVersionParam>,
    address: AddressParam,
    context: Context,
) -> Result<impl Reply, Rejection> {
    fail_point("endpoint_get_account_resources")?;
    Ok(Account::new(ledger_version, address, context)?.resources()?)
}

async fn handle_get_account_modules(
    ledger_version: Option<LedgerVersionParam>,
    address: AddressParam,
    context: Context,
) -> Result<impl Reply, Rejection> {
    fail_point("endpoint_get_account_modules")?;
    Ok(Account::new(ledger_version, address, context)?.modules()?)
}

pub(crate) struct Account {
    ledger_version: u64,
    address: Address,
    latest_ledger_info: LedgerInfo,
    context: Context,
}

impl Account {
    pub fn new(
        ledger_version: Option<LedgerVersionParam>,
        address: AddressParam,
        context: Context,
    ) -> Result<Self, Error> {
        let latest_ledger_info = context.get_latest_ledger_info()?;
        let ledger_version = ledger_version
            .map(|v| v.parse("ledger version"))
            .unwrap_or_else(|| Ok(latest_ledger_info.version()))?;

        if ledger_version > latest_ledger_info.version() {
            return Err(Error::not_found(
                "ledger",
                TransactionId::Version(ledger_version),
                latest_ledger_info.version(),
            ));
        }

        Ok(Self {
            ledger_version,
            address: address.parse("account address")?,
            latest_ledger_info,
            context,
        })
    }

    pub fn account(self) -> Result<impl Reply, Error> {
        let state_key = StateKey::AccessPath(AccessPath::resource_access_path(ResourceKey::new(
            self.address.into(),
            AccountResource::struct_tag(),
        )));

        let state_value = self
            .context
            .get_state_value(&state_key, self.ledger_version)?;

        let account_resource: AccountResource = state_value
            .map(|bytes| bcs::from_bytes(&bytes))
            .transpose()
            .map_err(anyhow::Error::from)?
            .ok_or_else(|| self.resource_not_found(&AccountResource::struct_tag()))?;

        let account: AccountData = account_resource.into();

        Response::new(self.latest_ledger_info, &account)
    }

    pub fn resources(self) -> Result<impl Reply, Error> {
        let resources = self
            .context
            .move_resolver()?
            .as_converter()
            .try_into_resources(self.account_state()?.get_resources())?;
        Response::new(self.latest_ledger_info, &resources)
    }

    pub fn modules(self) -> Result<impl Reply, Error> {
        let modules = self
            .account_state()?
            .into_modules()
            .map(MoveModuleBytecode::new)
            .map(|m| m.try_parse_abi())
            .collect::<Result<Vec<MoveModuleBytecode>>>()?;
        Response::new(self.latest_ledger_info, &modules)
    }

    pub fn find_event_key(
        &self,
        struct_tag_param: MoveStructTagParam,
        field_name_param: MoveIdentifierParam,
    ) -> Result<EventKey, Error> {
        let struct_tag: StructTag = struct_tag_param.parse("event handle struct")?.try_into()?;
        let field_name = field_name_param.parse("event handle field name")?;

        let resource = self.find_resource(&struct_tag)?;

        let (_id, value) = resource
            .into_iter()
            .find(|(id, _)| id == &field_name)
            .ok_or_else(|| self.field_not_found(&struct_tag, &field_name))?;

        // serialization should not fail, otherwise it's internal bug
        let event_handle_bytes = bcs::to_bytes(&value).map_err(anyhow::Error::from)?;
        // deserialization may fail because the bytes are not EventHandle struct type.
        let event_handle: EventHandle = bcs::from_bytes(&event_handle_bytes).map_err(|e| {
            Error::bad_request(format!(
                "field({}) type is not EventHandle struct, deserialize error: {}",
                field_name, e
            ))
        })?;
        Ok(*event_handle.key())
    }

    pub fn find_resource(
        &self,
        struct_tag: &StructTag,
    ) -> Result<Vec<(Identifier, MoveValue)>, Error> {
        let account_state = self.account_state()?;
        let (typ, data) = account_state
            .get_resources()
            .find(|(tag, _data)| tag == struct_tag)
            .ok_or_else(|| self.resource_not_found(struct_tag))?;
        Ok(self
            .context
            .move_resolver()?
            .as_converter()
            .move_struct_fields(&typ, data)?)
    }

    fn account_state(&self) -> Result<AccountState, Error> {
        let state = self
            .context
            .get_account_state(self.address.into(), self.ledger_version)?
            .ok_or_else(|| self.account_not_found())?;
        Ok(state)
    }

    fn account_not_found(&self) -> Error {
        Error::not_found(
            "account",
            format!(
                "address({}) and ledger version({})",
                self.address, self.ledger_version,
            ),
            self.latest_ledger_info.version(),
        )
    }

    fn resource_not_found(&self, struct_tag: &StructTag) -> Error {
        Error::not_found(
            "resource",
            format!(
                "address({}), struct tag({}) and ledger version({})",
                self.address, struct_tag, self.ledger_version,
            ),
            self.latest_ledger_info.version(),
        )
    }

    fn field_not_found(&self, struct_tag: &StructTag, field_name: &Identifier) -> Error {
        Error::not_found(
            "resource",
            format!(
                "address({}), struct tag({}), field name({}) and ledger version({})",
                self.address, struct_tag, field_name, self.ledger_version,
            ),
            self.latest_ledger_info.version(),
        )
    }
}
