use anyhow::{bail, Result as AnyResult};
use schemars::JsonSchema;

use cosmwasm_std::{
    coin, coins, to_binary, Addr, Api, BankMsg, Binary, BlockInfo, Coin, CustomQuery, Decimal,
    DelegationResponse, DistributionMsg, Empty, Event, FullDelegation, Querier, StakingMsg,
    StakingQuery, Storage, Uint128,
};
use cosmwasm_storage::{prefixed, prefixed_read};
use cw_storage_plus::Map;
use cw_utils::NativeBalance;

use crate::app::CosmosRouter;
use crate::executor::AppResponse;
use crate::{BankSudo, Module};

const STAKES: Map<&Addr, NativeBalance> = Map::new("stakes");

pub const NAMESPACE_STAKING: &[u8] = b"staking";

// We need to expand on this, but we will need this to properly test out staking
#[derive(Clone, std::fmt::Debug, PartialEq, Eq, JsonSchema)]
pub enum StakingSudo {
    Slash {
        validator: String,
        percentage: Decimal,
    },
}

pub trait Staking: Module<ExecT = StakingMsg, QueryT = StakingQuery, SudoT = StakingSudo> {}

pub trait Distribution: Module<ExecT = DistributionMsg, QueryT = Empty, SudoT = Empty> {}

pub struct StakeKeeper {
    module_addr: Addr,
}

impl StakeKeeper {
    pub fn new() -> Self {
        StakeKeeper {
            // define this better?? it is an account for everything held my the staking keeper
            module_addr: Addr::unchecked("staking_module"),
        }
    }

    pub fn init_balance(
        &self,
        storage: &mut dyn Storage,
        account: &Addr,
        amount: Vec<Coin>,
    ) -> AnyResult<()> {
        let mut storage = prefixed(storage, NAMESPACE_STAKING);
        self.set_balance(&mut storage, account, amount)
    }

    fn get_stakes(&self, storage: &dyn Storage, account: &Addr) -> AnyResult<Vec<Coin>> {
        let val = STAKES.may_load(storage, account)?;
        Ok(val.unwrap_or_default().into_vec())
    }

    fn set_balance(
        &self,
        storage: &mut dyn Storage,
        account: &Addr,
        amount: Vec<Coin>,
    ) -> AnyResult<()> {
        let mut stake = NativeBalance(amount);
        stake.normalize();
        STAKES.save(storage, account, &stake).map_err(Into::into)
    }

    fn add_stake(
        &self,
        storage: &mut dyn Storage,
        to_address: Addr,
        amount: Vec<Coin>,
    ) -> AnyResult<()> {
        let amount = self.normalize_amount(amount)?;
        let b = self.get_stakes(storage, &to_address)?;
        let b = NativeBalance(b) + NativeBalance(amount);
        self.set_balance(storage, &to_address, b.into_vec())
    }

    fn remove_stake(
        &self,
        storage: &mut dyn Storage,
        from_address: Addr,
        amount: Vec<Coin>,
    ) -> AnyResult<()> {
        let amount = self.normalize_amount(amount)?;
        let a = self.get_stakes(storage, &from_address)?;
        let a = (NativeBalance(a) - amount)?;
        self.set_balance(storage, &from_address, a.into_vec())
    }

    /// Filters out all 0 value coins and returns an error if the resulting Vec is empty
    fn normalize_amount(&self, amount: Vec<Coin>) -> AnyResult<Vec<Coin>> {
        let res: Vec<_> = amount.into_iter().filter(|x| !x.amount.is_zero()).collect();
        if res.is_empty() {
            bail!("Cannot transfer empty coins amount")
        } else {
            Ok(res)
        }
    }
}

impl Staking for StakeKeeper {}

impl Module for StakeKeeper {
    type ExecT = StakingMsg;
    type QueryT = StakingQuery;
    type SudoT = StakingSudo;

    fn execute<ExecC, QueryC: CustomQuery>(
        &self,
        api: &dyn Api,
        storage: &mut dyn Storage,
        router: &dyn CosmosRouter<ExecC = ExecC, QueryC = QueryC>,
        block: &BlockInfo,
        sender: Addr,
        msg: StakingMsg,
    ) -> AnyResult<AppResponse> {
        let mut staking_storage = prefixed(storage, NAMESPACE_STAKING);
        match msg {
            StakingMsg::Delegate { validator, amount } => {
                // TODO: assert amount is the proper denom
                // see https://github.com/cosmos/cosmos-sdk/blob/v0.46.1/x/staking/keeper/msg_server.go#L251-L256
                let events = vec![Event::new("delegate")
                    .add_attribute("validator", &validator)
                    .add_attribute("amount", format!("{}{}", amount.amount, amount.denom))
                    .add_attribute("new_shares", amount.amount.to_string())]; // TODO: calculate shares?
                self.add_stake(&mut staking_storage, sender.clone(), vec![amount.clone()])?;
                // move money from sender account to this module (note we can controller sender here)
                router.execute(
                    api,
                    storage,
                    block,
                    sender,
                    BankMsg::Send {
                        to_address: self.module_addr.to_string(),
                        amount: vec![amount],
                    }
                    .into(),
                )?;
                Ok(AppResponse { events, data: None })
            }
            StakingMsg::Undelegate { validator, amount } => {
                // see https://github.com/cosmos/cosmos-sdk/blob/v0.46.1/x/staking/keeper/msg_server.go#L378-L383
                let events = vec![Event::new("unbond")
                    .add_attribute("validator", &validator)
                    .add_attribute("amount", format!("{}{}", amount.amount, amount.denom))
                    .add_attribute("completion_time", "2022-09-27T14:00:00+00:00")]; // TODO: actual date?
                self.remove_stake(&mut staking_storage, sender.clone(), vec![amount.clone()])?;
                // move token from this module to sender account
                // TODO: actually store this so it is released later after unbonding period
                // but showing how to do the payback
                router.execute(
                    api,
                    storage,
                    block,
                    self.module_addr.clone(),
                    BankMsg::Send {
                        to_address: sender.into_string(),
                        amount: vec![amount],
                    }
                    .into(),
                )?;

                // NB: when you need more tokens for staking rewards you can do something like:
                router.sudo(
                    api,
                    storage,
                    block,
                    BankSudo::Mint {
                        to_address: self.module_addr.to_string(),
                        amount: coins(123456000, "ucosm"),
                    }
                    .into(),
                )?;
                Ok(AppResponse { events, data: None })
            }
            StakingMsg::Redelegate {
                src_validator,
                dst_validator,
                amount,
            } => {
                // see https://github.com/cosmos/cosmos-sdk/blob/v0.46.1/x/staking/keeper/msg_server.go#L316-L322
                let events = vec![Event::new("redelegate")
                    .add_attribute("source_validator", &src_validator)
                    .add_attribute("destination_validator", &dst_validator)
                    .add_attribute("amount", format!("{}{}", amount.amount, amount.denom))];

                // this is not a noop, since there is validation regarding the amount
                self.remove_stake(&mut staking_storage, sender.clone(), vec![amount.clone()])?;
                self.add_stake(&mut staking_storage, sender, vec![amount])?;

                Ok(AppResponse { events, data: None })
            }
            m => bail!("Unsupported staking message: {:?}", m),
        }
    }

    fn sudo<ExecC, QueryC>(
        &self,
        _api: &dyn Api,
        _storage: &mut dyn Storage,
        _router: &dyn CosmosRouter<ExecC = ExecC, QueryC = QueryC>,
        _block: &BlockInfo,
        msg: StakingSudo,
    ) -> AnyResult<AppResponse> {
        bail!("Unsupported staking sudo message: {:?}", msg)
    }

    fn query(
        &self,
        api: &dyn Api,
        storage: &dyn Storage,
        _querier: &dyn Querier,
        _block: &BlockInfo,
        request: StakingQuery,
    ) -> AnyResult<Binary> {
        let staking_storage = prefixed_read(storage, NAMESPACE_STAKING);
        match request {
            StakingQuery::Delegation {
                delegator,
                validator,
            } => {
                // for now validator is ignored, as I want to support only one validator
                let delegator = api.addr_validate(&delegator)?;
                let stakes = match self.get_stakes(&staking_storage, &delegator) {
                    Ok(stakes) => stakes[0].clone(),
                    Err(_) => {
                        let response = DelegationResponse { delegation: None };
                        return Ok(to_binary(&response)?);
                    }
                };
                // set fixed reward ratio 1:10 per delegated amoutn
                let reward = coin(
                    (stakes.amount / Uint128::new(10)).u128(),
                    stakes.denom.clone(),
                );
                let full_delegation_response = DelegationResponse {
                    delegation: Some(FullDelegation {
                        delegator,
                        validator,
                        amount: stakes,
                        can_redelegate: coin(0, "testcoin"),
                        accumulated_rewards: vec![reward],
                    }),
                };

                let res = to_binary(&full_delegation_response)?;
                Ok(res)
            }
            q => bail!("Unsupported staking sudo message: {:?}", q),
        }
    }
}

#[derive(Default)]
pub struct DistributionKeeper {}

impl DistributionKeeper {
    pub fn new() -> Self {
        DistributionKeeper {}
    }

    fn get_stakes(&self, storage: &dyn Storage, account: &Addr) -> AnyResult<Vec<Coin>> {
        let val = STAKES.may_load(storage, account)?;
        Ok(val.unwrap_or_default().into_vec())
    }
}

impl Distribution for DistributionKeeper {}

impl Module for DistributionKeeper {
    type ExecT = DistributionMsg;
    type QueryT = Empty;
    type SudoT = Empty;

    fn execute<ExecC, QueryC>(
        &self,
        _api: &dyn Api,
        storage: &mut dyn Storage,
        _router: &dyn CosmosRouter<ExecC = ExecC, QueryC = QueryC>,
        _block: &BlockInfo,
        sender: Addr,
        msg: DistributionMsg,
    ) -> AnyResult<AppResponse> {
        let staking_storage = prefixed(storage, NAMESPACE_STAKING);
        match msg {
            // For now it ignores validator as I want to support only one
            DistributionMsg::WithdrawDelegatorReward { validator } => {
                let stakes = self.get_stakes(&staking_storage, &sender)?[0].clone();
                // set fixed reward ratio 1:10 per delegated amoutn
                let reward = coin((stakes.amount / Uint128::new(10)).u128(), stakes.denom);

                let events = vec![Event::new("withdraw_delegator_reward")
                    .add_attribute("validator", &validator)
                    .add_attribute("sender", &sender)
                    .add_attribute("amount", format!("{}{}", reward.amount, reward.denom))];
                // TODO: add balance to sender by sending BankMsg transfer
                Ok(AppResponse { events, data: None })
            }
            m => bail!("Unsupported distribution message: {:?}", m),
        }
    }

    fn sudo<ExecC, QueryC>(
        &self,
        _api: &dyn Api,
        _storage: &mut dyn Storage,
        _router: &dyn CosmosRouter<ExecC = ExecC, QueryC = QueryC>,
        _block: &BlockInfo,
        _msg: Empty,
    ) -> AnyResult<AppResponse> {
        bail!("Something went wrong - Distribution doesn't have sudo messages")
    }

    fn query(
        &self,
        _api: &dyn Api,
        _storage: &dyn Storage,
        _querier: &dyn Querier,
        _block: &BlockInfo,
        _request: Empty,
    ) -> AnyResult<Binary> {
        bail!("Something went wrong - Distribution doesn't have query messages")
    }
}
