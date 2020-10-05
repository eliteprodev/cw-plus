use schemars::JsonSchema;
use std::fmt;

use cosmwasm_std::{
    attr, to_binary, Api, Binary, CanonicalAddr, CosmosMsg, Empty, Env, Extern, HandleResponse,
    HumanAddr, InitResponse, Querier, StdResult, Storage,
};
use cw1::CanSendResponse;
use cw2::set_contract_version;

use crate::error::ContractError;
use crate::msg::{AdminListResponse, HandleMsg, InitMsg, QueryMsg};
use crate::state::{admin_list, admin_list_read, AdminList};

// version info for migration info
const CONTRACT_NAME: &str = "crates.io:cw1-whitelist";
const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn init<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    _env: Env,
    msg: InitMsg,
) -> StdResult<InitResponse> {
    set_contract_version(&mut deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;
    let cfg = AdminList {
        admins: map_canonical(&deps.api, &msg.admins)?,
        mutable: msg.mutable,
    };
    admin_list(&mut deps.storage).save(&cfg)?;
    Ok(InitResponse::default())
}

fn map_canonical<A: Api>(api: &A, admins: &[HumanAddr]) -> StdResult<Vec<CanonicalAddr>> {
    admins
        .iter()
        .map(|addr| api.canonical_address(addr))
        .collect()
}

fn map_human<A: Api>(api: &A, admins: &[CanonicalAddr]) -> StdResult<Vec<HumanAddr>> {
    admins.iter().map(|addr| api.human_address(addr)).collect()
}

pub fn handle<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    // Note: implement this function with different type to add support for custom messages
    // and then import the rest of this contract code.
    msg: HandleMsg<Empty>,
) -> Result<HandleResponse<Empty>, ContractError> {
    match msg {
        HandleMsg::Execute { msgs } => handle_execute(deps, env, msgs),
        HandleMsg::Freeze {} => handle_freeze(deps, env),
        HandleMsg::UpdateAdmins { admins } => handle_update_admins(deps, env, admins),
    }
}

pub fn handle_execute<S: Storage, A: Api, Q: Querier, T>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    msgs: Vec<CosmosMsg<T>>,
) -> Result<HandleResponse<T>, ContractError>
where
    T: Clone + fmt::Debug + PartialEq + JsonSchema,
{
    if !can_send(&deps, &env.message.sender)? {
        Err(ContractError::Unauthorized {})
    } else {
        let mut res = HandleResponse::default();
        res.messages = msgs;
        res.attributes = vec![attr("action", "execute")];
        Ok(res)
    }
}

pub fn handle_freeze<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
) -> Result<HandleResponse, ContractError> {
    let mut cfg = admin_list_read(&deps.storage).load()?;
    if !cfg.can_modify(&deps.api.canonical_address(&env.message.sender)?) {
        Err(ContractError::Unauthorized {})
    } else {
        cfg.mutable = false;
        admin_list(&mut deps.storage).save(&cfg)?;

        let mut res = HandleResponse::default();
        res.attributes = vec![attr("action", "freeze")];
        Ok(res)
    }
}

pub fn handle_update_admins<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    admins: Vec<HumanAddr>,
) -> Result<HandleResponse, ContractError> {
    let mut cfg = admin_list_read(&deps.storage).load()?;
    if !cfg.can_modify(&deps.api.canonical_address(&env.message.sender)?) {
        Err(ContractError::Unauthorized {})
    } else {
        cfg.admins = map_canonical(&deps.api, &admins)?;
        admin_list(&mut deps.storage).save(&cfg)?;

        let mut res = HandleResponse::default();
        res.attributes = vec![attr("action", "update_admins")];
        Ok(res)
    }
}

fn can_send<S: Storage, A: Api, Q: Querier>(
    deps: &Extern<S, A, Q>,
    sender: &HumanAddr,
) -> StdResult<bool> {
    let cfg = admin_list_read(&deps.storage).load()?;
    let can = cfg.is_admin(&deps.api.canonical_address(sender)?);
    Ok(can)
}

pub fn query<S: Storage, A: Api, Q: Querier>(
    deps: &Extern<S, A, Q>,
    msg: QueryMsg,
) -> StdResult<Binary> {
    match msg {
        QueryMsg::AdminList {} => to_binary(&query_admin_list(deps)?),
        QueryMsg::CanSend { sender, msg } => to_binary(&query_can_send(deps, sender, msg)?),
    }
}

pub fn query_admin_list<S: Storage, A: Api, Q: Querier>(
    deps: &Extern<S, A, Q>,
) -> StdResult<AdminListResponse> {
    let cfg = admin_list_read(&deps.storage).load()?;
    Ok(AdminListResponse {
        admins: map_human(&deps.api, &cfg.admins)?,
        mutable: cfg.mutable,
    })
}

pub fn query_can_send<S: Storage, A: Api, Q: Querier>(
    deps: &Extern<S, A, Q>,
    sender: HumanAddr,
    _msg: CosmosMsg,
) -> StdResult<CanSendResponse> {
    Ok(CanSendResponse {
        can_send: can_send(&deps, &sender)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmwasm_std::testing::{mock_dependencies, mock_env, MOCK_CONTRACT_ADDR};
    use cosmwasm_std::{coin, coins, BankMsg, StakingMsg, WasmMsg};

    const CANONICAL_LENGTH: usize = 20;

    #[test]
    fn init_and_modify_config() {
        let mut deps = mock_dependencies(CANONICAL_LENGTH, &[]);

        let alice = HumanAddr::from("alice");
        let bob = HumanAddr::from("bob");
        let carl = HumanAddr::from("carl");

        let anyone = HumanAddr::from("anyone");

        // init the contract
        let init_msg = InitMsg {
            admins: vec![alice.clone(), bob.clone(), carl.clone()],
            mutable: true,
        };
        let env = mock_env(&anyone, &[]);
        init(&mut deps, env, init_msg).unwrap();

        // ensure expected config
        let expected = AdminListResponse {
            admins: vec![alice.clone(), bob.clone(), carl.clone()],
            mutable: true,
        };
        assert_eq!(query_admin_list(&deps).unwrap(), expected);

        // anyone cannot modify the contract
        let msg = HandleMsg::UpdateAdmins {
            admins: vec![anyone.clone()],
        };
        let env = mock_env(&anyone, &[]);
        let res = handle(&mut deps, env, msg);
        match res.unwrap_err() {
            ContractError::Unauthorized { .. } => {}
            e => panic!("unexpected error: {}", e),
        }

        // but alice can kick out carl
        let msg = HandleMsg::UpdateAdmins {
            admins: vec![alice.clone(), bob.clone()],
        };
        let env = mock_env(&alice, &[]);
        handle(&mut deps, env, msg).unwrap();

        // ensure expected config
        let expected = AdminListResponse {
            admins: vec![alice.clone(), bob.clone()],
            mutable: true,
        };
        assert_eq!(query_admin_list(&deps).unwrap(), expected);

        // carl cannot freeze it
        let env = mock_env(&carl, &[]);
        let res = handle(&mut deps, env, HandleMsg::Freeze {});
        match res.unwrap_err() {
            ContractError::Unauthorized { .. } => {}
            e => panic!("unexpected error: {}", e),
        }

        // but bob can
        let env = mock_env(&bob, &[]);
        handle(&mut deps, env, HandleMsg::Freeze {}).unwrap();
        let expected = AdminListResponse {
            admins: vec![alice.clone(), bob.clone()],
            mutable: false,
        };
        assert_eq!(query_admin_list(&deps).unwrap(), expected);

        // and now alice cannot change it again
        let msg = HandleMsg::UpdateAdmins {
            admins: vec![alice.clone()],
        };
        let env = mock_env(&alice, &[]);
        let res = handle(&mut deps, env, msg);
        match res.unwrap_err() {
            ContractError::Unauthorized { .. } => {}
            e => panic!("unexpected error: {}", e),
        }
    }

    #[test]
    fn execute_messages_has_proper_permissions() {
        let mut deps = mock_dependencies(CANONICAL_LENGTH, &[]);

        let alice = HumanAddr::from("alice");
        let bob = HumanAddr::from("bob");
        let carl = HumanAddr::from("carl");

        // init the contract
        let init_msg = InitMsg {
            admins: vec![alice.clone(), carl.clone()],
            mutable: false,
        };
        let env = mock_env(&bob, &[]);
        init(&mut deps, env, init_msg).unwrap();

        let freeze: HandleMsg<Empty> = HandleMsg::Freeze {};
        let msgs = vec![
            BankMsg::Send {
                from_address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                to_address: bob.clone(),
                amount: coins(10000, "DAI"),
            }
            .into(),
            WasmMsg::Execute {
                contract_addr: HumanAddr::from("some contract"),
                msg: to_binary(&freeze).unwrap(),
                send: vec![],
            }
            .into(),
        ];

        // make some nice message
        let handle_msg = HandleMsg::Execute { msgs: msgs.clone() };

        // bob cannot execute them
        let env = mock_env(&bob, &[]);
        let res = handle(&mut deps, env, handle_msg.clone());
        match res.unwrap_err() {
            ContractError::Unauthorized { .. } => {}
            e => panic!("unexpected error: {}", e),
        }

        // but carl can
        let env = mock_env(&carl, &[]);
        let res = handle(&mut deps, env, handle_msg.clone()).unwrap();
        assert_eq!(res.messages, msgs);
        assert_eq!(res.attributes, vec![attr("action", "execute")]);
    }

    #[test]
    fn can_send_query_works() {
        let mut deps = mock_dependencies(CANONICAL_LENGTH, &[]);

        let alice = HumanAddr::from("alice");
        let bob = HumanAddr::from("bob");

        let anyone = HumanAddr::from("anyone");

        // init the contract
        let init_msg = InitMsg {
            admins: vec![alice.clone(), bob.clone()],
            mutable: false,
        };
        let env = mock_env(&anyone, &[]);
        init(&mut deps, env, init_msg).unwrap();

        // let us make some queries... different msg types by owner and by other
        let send_msg = CosmosMsg::Bank(BankMsg::Send {
            from_address: MOCK_CONTRACT_ADDR.into(),
            to_address: anyone.clone(),
            amount: coins(12345, "ushell"),
        });
        let staking_msg = CosmosMsg::Staking(StakingMsg::Delegate {
            validator: anyone.clone(),
            amount: coin(70000, "ureef"),
        });

        // owner can send
        let res = query_can_send(&deps, alice.clone(), send_msg.clone()).unwrap();
        assert_eq!(res.can_send, true);

        // owner can stake
        let res = query_can_send(&deps, bob.clone(), staking_msg.clone()).unwrap();
        assert_eq!(res.can_send, true);

        // anyone cannot send
        let res = query_can_send(&deps, anyone.clone(), send_msg.clone()).unwrap();
        assert_eq!(res.can_send, false);

        // anyone cannot stake
        let res = query_can_send(&deps, anyone.clone(), staking_msg.clone()).unwrap();
        assert_eq!(res.can_send, false);
    }
}
