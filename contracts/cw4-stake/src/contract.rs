use cosmwasm_std::{
    attr, coin, coins, to_binary, BankMsg, Binary, CanonicalAddr, Coin, CosmosMsg, Deps, DepsMut,
    Env, HandleResponse, HumanAddr, InitResponse, MessageInfo, Order, StdResult, Storage, Uint128,
};
use cw0::maybe_canonical;
use cw2::set_contract_version;
use cw4::{
    Member, MemberChangedHookMsg, MemberDiff, MemberListResponse, MemberResponse,
    TotalWeightResponse,
};
use cw_storage_plus::Bound;

use crate::error::ContractError;
use crate::msg::{HandleMsg, InitMsg, QueryMsg, StakedResponse};
use crate::state::{Config, ADMIN, CLAIMS, CONFIG, HOOKS, MEMBERS, STAKE, TOTAL};

// version info for migration info
const CONTRACT_NAME: &str = "crates.io:cw4-stake";
const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

// Note, you can use StdResult in some functions where you do not
// make use of the custom errors
pub fn init(
    mut deps: DepsMut,
    _env: Env,
    _info: MessageInfo,
    msg: InitMsg,
) -> Result<InitResponse, ContractError> {
    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;
    ADMIN.set(deps.branch(), msg.admin)?;

    // min_bond is at least 1, so 0 stake -> non-membership
    let min_bond = match msg.min_bond {
        Uint128(0) => Uint128(1),
        v => v,
    };

    let config = Config {
        denom: msg.denom,
        tokens_per_weight: msg.tokens_per_weight,
        min_bond,
        unbonding_period: msg.unbonding_period,
    };
    CONFIG.save(deps.storage, &config)?;
    TOTAL.save(deps.storage, &0)?;

    Ok(InitResponse::default())
}

// And declare a custom Error variant for the ones where you will want to make use of it
pub fn handle(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: HandleMsg,
) -> Result<HandleResponse, ContractError> {
    match msg {
        HandleMsg::UpdateAdmin { admin } => Ok(ADMIN.handle_update_admin(deps, info, admin)?),
        HandleMsg::AddHook { addr } => Ok(HOOKS.handle_add_hook(&ADMIN, deps, info, addr)?),
        HandleMsg::RemoveHook { addr } => Ok(HOOKS.handle_remove_hook(&ADMIN, deps, info, addr)?),
        HandleMsg::Bond {} => handle_bond(deps, env, info),
        HandleMsg::Unbond { tokens: amount } => handle_unbond(deps, env, info, amount),
        HandleMsg::Claim {} => handle_claim(deps, env, info),
    }
}

pub fn handle_bond(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
) -> Result<HandleResponse, ContractError> {
    let cfg = CONFIG.load(deps.storage)?;

    // ensure the sent denom was proper
    // NOTE: those clones are not needed (if we move denom, we return early),
    // but the compiler cannot see that (yet...)
    let sent = match info.sent_funds.len() {
        0 => Err(ContractError::NoFunds {}),
        1 => {
            if info.sent_funds[0].denom == cfg.denom {
                Ok(info.sent_funds[0].amount)
            } else {
                Err(ContractError::MissingDenom(cfg.denom.clone()))
            }
        }
        _ => Err(ContractError::ExtraDenoms(cfg.denom.clone())),
    }?;
    if sent.is_zero() {
        return Err(ContractError::NoFunds {});
    }

    // update the sender's stake
    let sender_raw = deps.api.canonical_address(&info.sender)?;
    let new_stake = STAKE.update(deps.storage, &sender_raw, |stake| -> StdResult<_> {
        Ok(stake.unwrap_or_default() + sent)
    })?;

    let messages = update_membership(
        deps.storage,
        info.sender.clone(),
        &sender_raw,
        new_stake,
        &cfg,
        env.block.height,
    )?;

    let attributes = vec![
        attr("action", "bond"),
        attr("amount", sent),
        attr("sender", info.sender),
    ];
    Ok(HandleResponse {
        messages,
        attributes,
        data: None,
    })
}

pub fn handle_unbond(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    amount: Uint128,
) -> Result<HandleResponse, ContractError> {
    // reduce the sender's stake - aborting if insufficient
    let sender_raw = deps.api.canonical_address(&info.sender)?;
    let new_stake = STAKE.update(deps.storage, &sender_raw, |stake| -> StdResult<_> {
        stake.unwrap_or_default() - amount
    })?;

    // provide them a claim
    let cfg = CONFIG.load(deps.storage)?;
    CLAIMS.create_claim(
        deps.storage,
        &sender_raw,
        amount,
        cfg.unbonding_period.after(&env.block),
    )?;

    let messages = update_membership(
        deps.storage,
        info.sender.clone(),
        &sender_raw,
        new_stake,
        &cfg,
        env.block.height,
    )?;

    let attributes = vec![
        attr("action", "unbond"),
        attr("amount", amount),
        attr("sender", info.sender),
    ];
    Ok(HandleResponse {
        messages,
        attributes,
        data: None,
    })
}

fn update_membership(
    storage: &mut dyn Storage,
    sender: HumanAddr,
    sender_raw: &CanonicalAddr,
    new_stake: Uint128,
    cfg: &Config,
    height: u64,
) -> StdResult<Vec<CosmosMsg>> {
    // update their membership weight
    let new = calc_weight(new_stake, cfg);
    let old = MEMBERS.may_load(storage, sender_raw)?;

    // short-circuit if no change
    if new == old {
        return Ok(vec![]);
    }
    // otherwise, record change of weight
    match new.as_ref() {
        Some(w) => MEMBERS.save(storage, sender_raw, w, height),
        None => MEMBERS.remove(storage, sender_raw, height),
    }?;

    // update total
    TOTAL.update(storage, |total| -> StdResult<_> {
        Ok(total + new.unwrap_or_default() - old.unwrap_or_default())
    })?;

    // alert the hooks
    let diff = MemberDiff::new(sender, old, new);
    HOOKS.prepare_hooks(storage, |h| {
        MemberChangedHookMsg::one(diff.clone()).into_cosmos_msg(h)
    })
}

fn calc_weight(stake: Uint128, cfg: &Config) -> Option<u64> {
    if stake < cfg.min_bond {
        None
    } else {
        let w = stake.u128() / (cfg.tokens_per_weight.u128());
        Some(w as u64)
    }
}

pub fn handle_claim(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
) -> Result<HandleResponse, ContractError> {
    let sender_raw = deps.api.canonical_address(&info.sender)?;
    let release = CLAIMS.claim_tokens(deps.storage, &sender_raw, &env.block, None)?;
    if release.is_zero() {
        return Err(ContractError::NothingToClaim {});
    }

    let config = CONFIG.load(deps.storage)?;
    let amount = coins(release.u128(), config.denom);
    let amount_str = coins_to_string(&amount);

    let messages = vec![BankMsg::Send {
        from_address: env.contract.address,
        to_address: info.sender.clone(),
        amount,
    }
    .into()];

    let attributes = vec![
        attr("action", "claim"),
        attr("tokens", amount_str),
        attr("sender", info.sender),
    ];
    Ok(HandleResponse {
        messages,
        attributes,
        data: None,
    })
}

// TODO: put in cosmwasm-std
fn coins_to_string(coins: &[Coin]) -> String {
    let strings: Vec<_> = coins
        .iter()
        .map(|c| format!("{}{}", c.amount, c.denom))
        .collect();
    strings.join(",")
}

pub fn query(deps: Deps, _env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::Member {
            addr,
            at_height: height,
        } => to_binary(&query_member(deps, addr, height)?),
        QueryMsg::ListMembers { start_after, limit } => {
            to_binary(&list_members(deps, start_after, limit)?)
        }
        QueryMsg::TotalWeight {} => to_binary(&query_total_weight(deps)?),
        QueryMsg::Claims { address } => to_binary(&CLAIMS.query_claims(deps, address)?),
        QueryMsg::Staked { address } => to_binary(&query_staked(deps, address)?),
        QueryMsg::Admin {} => to_binary(&ADMIN.query_admin(deps)?),
        QueryMsg::Hooks {} => to_binary(&HOOKS.query_hooks(deps)?),
    }
}

fn query_total_weight(deps: Deps) -> StdResult<TotalWeightResponse> {
    let weight = TOTAL.load(deps.storage)?;
    Ok(TotalWeightResponse { weight })
}

pub fn query_staked(deps: Deps, address: HumanAddr) -> StdResult<StakedResponse> {
    let address_raw = deps.api.canonical_address(&address)?;
    let stake = STAKE
        .may_load(deps.storage, &address_raw)?
        .unwrap_or_default();
    let denom = CONFIG.load(deps.storage)?.denom;
    Ok(StakedResponse {
        stake: coin(stake.u128(), denom),
    })
}

fn query_member(deps: Deps, addr: HumanAddr, height: Option<u64>) -> StdResult<MemberResponse> {
    let raw = deps.api.canonical_address(&addr)?;
    let weight = match height {
        Some(h) => MEMBERS.may_load_at_height(deps.storage, &raw, h),
        None => MEMBERS.may_load(deps.storage, &raw),
    }?;
    Ok(MemberResponse { weight })
}

// settings for pagination
const MAX_LIMIT: u32 = 30;
const DEFAULT_LIMIT: u32 = 10;

fn list_members(
    deps: Deps,
    start_after: Option<HumanAddr>,
    limit: Option<u32>,
) -> StdResult<MemberListResponse> {
    let limit = limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT) as usize;
    let canon = maybe_canonical(deps.api, start_after)?;
    let start = canon.map(Bound::exclusive);

    let api = &deps.api;
    let members: StdResult<Vec<_>> = MEMBERS
        .range(deps.storage, start, None, Order::Ascending)
        .take(limit)
        .map(|item| {
            let (key, weight) = item?;
            Ok(Member {
                addr: api.human_address(&CanonicalAddr::from(key))?,
                weight,
            })
        })
        .collect();

    Ok(MemberListResponse { members: members? })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmwasm_std::testing::{mock_dependencies, mock_env, mock_info};
    use cosmwasm_std::{from_slice, Api, StdError, Storage};
    use cw0::Duration;
    use cw4::{member_key, TOTAL_KEY};
    use cw_controllers::{AdminError, Claim, HookError};

    const INIT_ADMIN: &str = "juan";
    const USER1: &str = "somebody";
    const USER2: &str = "else";
    const USER3: &str = "funny";

    const DENOM: &str = "stake";
    const TOKENS_PER_WEIGHT: Uint128 = Uint128(1_000);
    const MIN_BOND: Uint128 = Uint128(5_000);
    const UNBONDING_BLOCKS: u64 = 100;

    fn default_init(deps: DepsMut) {
        do_init(
            deps,
            TOKENS_PER_WEIGHT,
            MIN_BOND,
            Duration::Height(UNBONDING_BLOCKS),
        )
    }

    fn do_init(
        deps: DepsMut,
        tokens_per_weight: Uint128,
        min_bond: Uint128,
        unbonding_period: Duration,
    ) {
        let msg = InitMsg {
            denom: DENOM.to_string(),
            tokens_per_weight,
            min_bond,
            unbonding_period,
            admin: Some(INIT_ADMIN.into()),
        };
        let info = mock_info("creator", &[]);
        init(deps, mock_env(), info, msg).unwrap();
    }

    fn bond(mut deps: DepsMut, user1: u128, user2: u128, user3: u128, height_delta: u64) {
        let mut env = mock_env();
        env.block.height += height_delta;

        for (addr, stake) in &[(USER1, user1), (USER2, user2), (USER3, user3)] {
            if *stake != 0 {
                let msg = HandleMsg::Bond {};
                let info = mock_info(HumanAddr::from(*addr), &coins(*stake, DENOM));
                handle(deps.branch(), env.clone(), info, msg).unwrap();
            }
        }
    }

    fn unbond(mut deps: DepsMut, user1: u128, user2: u128, user3: u128, height_delta: u64) {
        let mut env = mock_env();
        env.block.height += height_delta;

        for (addr, stake) in &[(USER1, user1), (USER2, user2), (USER3, user3)] {
            if *stake != 0 {
                let msg = HandleMsg::Unbond {
                    tokens: Uint128(*stake),
                };
                let info = mock_info(HumanAddr::from(*addr), &[]);
                handle(deps.branch(), env.clone(), info, msg).unwrap();
            }
        }
    }

    #[test]
    fn proper_initialization() {
        let mut deps = mock_dependencies(&[]);
        default_init(deps.as_mut());

        // it worked, let's query the state
        let res = ADMIN.query_admin(deps.as_ref()).unwrap();
        assert_eq!(Some(HumanAddr::from(INIT_ADMIN)), res.admin);

        let res = query_total_weight(deps.as_ref()).unwrap();
        assert_eq!(0, res.weight);
    }

    fn get_member(deps: Deps, addr: HumanAddr, at_height: Option<u64>) -> Option<u64> {
        let raw = query(deps, mock_env(), QueryMsg::Member { addr, at_height }).unwrap();
        let res: MemberResponse = from_slice(&raw).unwrap();
        res.weight
    }

    // this tests the member queries
    fn assert_users(
        deps: Deps,
        user1_weight: Option<u64>,
        user2_weight: Option<u64>,
        user3_weight: Option<u64>,
        height: Option<u64>,
    ) {
        let member1 = get_member(deps, USER1.into(), height);
        assert_eq!(member1, user1_weight);

        let member2 = get_member(deps, USER2.into(), height);
        assert_eq!(member2, user2_weight);

        let member3 = get_member(deps, USER3.into(), height);
        assert_eq!(member3, user3_weight);

        // this is only valid if we are not doing a historical query
        if height.is_none() {
            // compute expected metrics
            let weights = vec![user1_weight, user2_weight, user3_weight];
            let sum: u64 = weights.iter().map(|x| x.unwrap_or_default()).sum();
            let count = weights.iter().filter(|x| x.is_some()).count();

            // TODO: more detailed compare?
            let msg = QueryMsg::ListMembers {
                start_after: None,
                limit: None,
            };
            let raw = query(deps, mock_env(), msg).unwrap();
            let members: MemberListResponse = from_slice(&raw).unwrap();
            assert_eq!(count, members.members.len());

            let raw = query(deps, mock_env(), QueryMsg::TotalWeight {}).unwrap();
            let total: TotalWeightResponse = from_slice(&raw).unwrap();
            assert_eq!(sum, total.weight); // 17 - 11 + 15 = 21
        }
    }

    // this tests the member queries
    fn assert_stake(deps: Deps, user1_stake: u128, user2_stake: u128, user3_stake: u128) {
        let stake1 = query_staked(deps, USER1.into()).unwrap();
        assert_eq!(stake1.stake, coin(user1_stake, DENOM));

        let stake2 = query_staked(deps, USER2.into()).unwrap();
        assert_eq!(stake2.stake, coin(user2_stake, DENOM));

        let stake3 = query_staked(deps, USER3.into()).unwrap();
        assert_eq!(stake3.stake, coin(user3_stake, DENOM));
    }

    #[test]
    fn bond_stake_adds_membership() {
        let mut deps = mock_dependencies(&[]);
        default_init(deps.as_mut());
        let height = mock_env().block.height;

        // Assert original weights
        assert_users(deps.as_ref(), None, None, None, None);

        // ensure it rounds down, and respects cut-off
        bond(deps.as_mut(), 12_000, 7_500, 4_000, 1);

        // Assert updated weights
        assert_stake(deps.as_ref(), 12_000, 7_500, 4_000);
        assert_users(deps.as_ref(), Some(12), Some(7), None, None);

        // add some more, ensure the sum is properly respected (7.5 + 7.6 = 15 not 14)
        bond(deps.as_mut(), 0, 7_600, 1_200, 2);

        // Assert updated weights
        assert_stake(deps.as_ref(), 12_000, 15_100, 5_200);
        assert_users(deps.as_ref(), Some(12), Some(15), Some(5), None);

        // check historical queries all work
        assert_users(deps.as_ref(), None, None, None, Some(height + 1)); // before first stake
        assert_users(deps.as_ref(), Some(12), Some(7), None, Some(height + 2)); // after first stake
        assert_users(deps.as_ref(), Some(12), Some(15), Some(5), Some(height + 3));
        // after second stake
    }

    #[test]
    fn unbond_stake_update_membership() {
        let mut deps = mock_dependencies(&[]);
        default_init(deps.as_mut());
        let height = mock_env().block.height;

        // ensure it rounds down, and respects cut-off
        bond(deps.as_mut(), 12_000, 7_500, 4_000, 1);
        unbond(deps.as_mut(), 4_500, 2_600, 1_111, 2);

        // Assert updated weights
        assert_stake(deps.as_ref(), 7_500, 4_900, 2_889);
        assert_users(deps.as_ref(), Some(7), None, None, None);

        // Adding a little more returns weight
        bond(deps.as_mut(), 600, 100, 2_222, 3);

        // Assert updated weights
        assert_users(deps.as_ref(), Some(8), Some(5), Some(5), None);

        // check historical queries all work
        assert_users(deps.as_ref(), None, None, None, Some(height + 1)); // before first stake
        assert_users(deps.as_ref(), Some(12), Some(7), None, Some(height + 2)); // after first bond
        assert_users(deps.as_ref(), Some(7), None, None, Some(height + 3)); // after first unbond
        assert_users(deps.as_ref(), Some(8), Some(5), Some(5), Some(height + 4)); // after second bond

        // error if try to unbond more than stake (USER2 has 5000 staked)
        let msg = HandleMsg::Unbond {
            tokens: Uint128(5100),
        };
        let mut env = mock_env();
        env.block.height += 5;
        let info = mock_info(USER2, &[]);
        let err = handle(deps.as_mut(), env, info, msg).unwrap_err();
        match err {
            ContractError::Std(StdError::Underflow {
                minuend,
                subtrahend,
            }) => {
                assert_eq!(minuend.as_str(), "5000");
                assert_eq!(subtrahend.as_str(), "5100");
            }
            e => panic!("Unexpected error: {}", e),
        }
    }

    #[test]
    fn raw_queries_work() {
        // add will over-write and remove have no effect
        let mut deps = mock_dependencies(&[]);
        default_init(deps.as_mut());
        // Set values as (11, 6, None)
        bond(deps.as_mut(), 11_000, 6_000, 0, 1);

        // get total from raw key
        let total_raw = deps.storage.get(TOTAL_KEY.as_bytes()).unwrap();
        let total: u64 = from_slice(&total_raw).unwrap();
        assert_eq!(17, total);

        // get member votes from raw key
        let member2_canon = deps.api.canonical_address(&USER2.into()).unwrap();
        let member2_raw = deps.storage.get(&member_key(&member2_canon)).unwrap();
        let member2: u64 = from_slice(&member2_raw).unwrap();
        assert_eq!(6, member2);

        // and handle misses
        let member3_canon = deps.api.canonical_address(&USER3.into()).unwrap();
        let member3_raw = deps.storage.get(&member_key(&member3_canon));
        assert_eq!(None, member3_raw);
    }

    fn get_claims<U: Into<HumanAddr>>(deps: Deps, addr: U) -> Vec<Claim> {
        CLAIMS.query_claims(deps, addr.into()).unwrap().claims
    }

    #[test]
    fn unbond_claim_workflow() {
        let mut deps = mock_dependencies(&[]);
        default_init(deps.as_mut());

        // create some data
        bond(deps.as_mut(), 12_000, 7_500, 4_000, 1);
        unbond(deps.as_mut(), 4_500, 2_600, 0, 2);
        let mut env = mock_env();
        env.block.height += 2;

        // check the claims for each user
        let expires = Duration::Height(UNBONDING_BLOCKS).after(&env.block);
        assert_eq!(
            get_claims(deps.as_ref(), USER1),
            vec![Claim::new(4_500, expires)]
        );
        assert_eq!(
            get_claims(deps.as_ref(), USER2),
            vec![Claim::new(2_600, expires)]
        );
        assert_eq!(get_claims(deps.as_ref(), USER3), vec![]);

        // do another unbond later on
        let mut env2 = mock_env();
        env2.block.height += 22;
        unbond(deps.as_mut(), 0, 1_345, 1_500, 22);

        // with updated claims
        let expires2 = Duration::Height(UNBONDING_BLOCKS).after(&env2.block);
        assert_eq!(
            get_claims(deps.as_ref(), USER1),
            vec![Claim::new(4_500, expires)]
        );
        assert_eq!(
            get_claims(deps.as_ref(), USER2),
            vec![Claim::new(2_600, expires), Claim::new(1_345, expires2)]
        );
        assert_eq!(
            get_claims(deps.as_ref(), USER3),
            vec![Claim::new(1_500, expires2)]
        );

        // nothing can be withdrawn yet
        let err = handle(
            deps.as_mut(),
            env2.clone(),
            mock_info(USER1, &[]),
            HandleMsg::Claim {},
        )
        .unwrap_err();
        assert_eq!(err, ContractError::NothingToClaim {});

        // now mature first section, withdraw that
        let mut env3 = mock_env();
        env3.block.height += 2 + UNBONDING_BLOCKS;
        // first one can now release
        let res = handle(
            deps.as_mut(),
            env3.clone(),
            mock_info(USER1, &[]),
            HandleMsg::Claim {},
        )
        .unwrap();
        assert_eq!(
            res.messages,
            vec![BankMsg::Send {
                from_address: env3.contract.address.clone(),
                to_address: USER1.into(),
                amount: coins(4_500, DENOM),
            }
            .into()]
        );

        // second releases partially
        let res = handle(
            deps.as_mut(),
            env3.clone(),
            mock_info(USER2, &[]),
            HandleMsg::Claim {},
        )
        .unwrap();
        assert_eq!(
            res.messages,
            vec![BankMsg::Send {
                from_address: env3.contract.address.clone(),
                to_address: USER2.into(),
                amount: coins(2_600, DENOM),
            }
            .into()]
        );

        // but the third one cannot release
        let err = handle(
            deps.as_mut(),
            env3.clone(),
            mock_info(USER3, &[]),
            HandleMsg::Claim {},
        )
        .unwrap_err();
        assert_eq!(err, ContractError::NothingToClaim {});

        // claims updated properly
        assert_eq!(get_claims(deps.as_ref(), USER1), vec![]);
        assert_eq!(
            get_claims(deps.as_ref(), USER2),
            vec![Claim::new(1_345, expires2)]
        );
        assert_eq!(
            get_claims(deps.as_ref(), USER3),
            vec![Claim::new(1_500, expires2)]
        );

        // add another few claims for 2
        unbond(deps.as_mut(), 0, 600, 0, 30 + UNBONDING_BLOCKS);
        unbond(deps.as_mut(), 0, 1_005, 0, 50 + UNBONDING_BLOCKS);

        // ensure second can claim all tokens at once
        let mut env4 = mock_env();
        env4.block.height += 55 + UNBONDING_BLOCKS + UNBONDING_BLOCKS;
        let res = handle(
            deps.as_mut(),
            env4.clone(),
            mock_info(USER2, &[]),
            HandleMsg::Claim {},
        )
        .unwrap();
        assert_eq!(
            res.messages,
            vec![BankMsg::Send {
                from_address: env4.contract.address.clone(),
                to_address: USER2.into(),
                // 1_345 + 600 + 1_005
                amount: coins(2_950, DENOM),
            }
            .into()]
        );
        assert_eq!(get_claims(deps.as_ref(), USER2), vec![]);
    }

    #[test]
    fn add_remove_hooks() {
        // add will over-write and remove have no effect
        let mut deps = mock_dependencies(&[]);
        default_init(deps.as_mut());

        let hooks = HOOKS.query_hooks(deps.as_ref()).unwrap();
        assert!(hooks.hooks.is_empty());

        let contract1 = HumanAddr::from("hook1");
        let contract2 = HumanAddr::from("hook2");

        let add_msg = HandleMsg::AddHook {
            addr: contract1.clone(),
        };

        // non-admin cannot add hook
        let user_info = mock_info(USER1, &[]);
        let err = handle(
            deps.as_mut(),
            mock_env(),
            user_info.clone(),
            add_msg.clone(),
        )
        .unwrap_err();
        assert_eq!(err, HookError::Admin(AdminError::NotAdmin {}).into());

        // admin can add it, and it appears in the query
        let admin_info = mock_info(INIT_ADMIN, &[]);
        let _ = handle(
            deps.as_mut(),
            mock_env(),
            admin_info.clone(),
            add_msg.clone(),
        )
        .unwrap();
        let hooks = HOOKS.query_hooks(deps.as_ref()).unwrap();
        assert_eq!(hooks.hooks, vec![contract1.clone()]);

        // cannot remove a non-registered contract
        let remove_msg = HandleMsg::RemoveHook {
            addr: contract2.clone(),
        };
        let err = handle(
            deps.as_mut(),
            mock_env(),
            admin_info.clone(),
            remove_msg.clone(),
        )
        .unwrap_err();
        assert_eq!(err, HookError::HookNotRegistered {}.into());

        // add second contract
        let add_msg2 = HandleMsg::AddHook {
            addr: contract2.clone(),
        };
        let _ = handle(deps.as_mut(), mock_env(), admin_info.clone(), add_msg2).unwrap();
        let hooks = HOOKS.query_hooks(deps.as_ref()).unwrap();
        assert_eq!(hooks.hooks, vec![contract1.clone(), contract2.clone()]);

        // cannot re-add an existing contract
        let err = handle(
            deps.as_mut(),
            mock_env(),
            admin_info.clone(),
            add_msg.clone(),
        )
        .unwrap_err();
        assert_eq!(err, HookError::HookAlreadyRegistered {}.into());

        // non-admin cannot remove
        let remove_msg = HandleMsg::RemoveHook {
            addr: contract1.clone(),
        };
        let err = handle(
            deps.as_mut(),
            mock_env(),
            user_info.clone(),
            remove_msg.clone(),
        )
        .unwrap_err();
        assert_eq!(err, HookError::Admin(AdminError::NotAdmin {}).into());

        // remove the original
        let _ = handle(
            deps.as_mut(),
            mock_env(),
            admin_info.clone(),
            remove_msg.clone(),
        )
        .unwrap();
        let hooks = HOOKS.query_hooks(deps.as_ref()).unwrap();
        assert_eq!(hooks.hooks, vec![contract2.clone()]);
    }

    #[test]
    fn hooks_fire() {
        let mut deps = mock_dependencies(&[]);
        default_init(deps.as_mut());

        let hooks = HOOKS.query_hooks(deps.as_ref()).unwrap();
        assert!(hooks.hooks.is_empty());

        let contract1 = HumanAddr::from("hook1");
        let contract2 = HumanAddr::from("hook2");

        // register 2 hooks
        let admin_info = mock_info(INIT_ADMIN, &[]);
        let add_msg = HandleMsg::AddHook {
            addr: contract1.clone(),
        };
        let add_msg2 = HandleMsg::AddHook {
            addr: contract2.clone(),
        };
        for msg in vec![add_msg, add_msg2] {
            let _ = handle(deps.as_mut(), mock_env(), admin_info.clone(), msg).unwrap();
        }

        // check firing on bond
        assert_users(deps.as_ref(), None, None, None, None);
        let info = mock_info(USER1, &coins(13_800, DENOM));
        let res = handle(deps.as_mut(), mock_env(), info, HandleMsg::Bond {}).unwrap();
        assert_users(deps.as_ref(), Some(13), None, None, None);

        // ensure messages for each of the 2 hooks
        assert_eq!(res.messages.len(), 2);
        let diff = MemberDiff::new(USER1, None, Some(13));
        let hook_msg = MemberChangedHookMsg::one(diff);
        let msg1 = hook_msg.clone().into_cosmos_msg(contract1.clone()).unwrap();
        let msg2 = hook_msg.into_cosmos_msg(contract2.clone()).unwrap();
        assert_eq!(res.messages, vec![msg1, msg2]);

        // check firing on unbond
        let msg = HandleMsg::Unbond {
            tokens: Uint128(7_300),
        };
        let info = mock_info(USER1, &[]);
        let res = handle(deps.as_mut(), mock_env(), info, msg).unwrap();
        assert_users(deps.as_ref(), Some(6), None, None, None);

        // ensure messages for each of the 2 hooks
        assert_eq!(res.messages.len(), 2);
        let diff = MemberDiff::new(USER1, Some(13), Some(6));
        let hook_msg = MemberChangedHookMsg::one(diff);
        let msg1 = hook_msg.clone().into_cosmos_msg(contract1).unwrap();
        let msg2 = hook_msg.into_cosmos_msg(contract2).unwrap();
        assert_eq!(res.messages, vec![msg1, msg2]);
    }

    #[test]
    fn only_bond_valid_coins() {
        let mut deps = mock_dependencies(&[]);
        default_init(deps.as_mut());

        // cannot bond with 0 coins
        let info = mock_info(HumanAddr::from(USER1), &[]);
        let err = handle(deps.as_mut(), mock_env(), info, HandleMsg::Bond {}).unwrap_err();
        assert_eq!(err, ContractError::NoFunds {});

        // cannot bond with incorrect denom
        let info = mock_info(HumanAddr::from(USER1), &[coin(500, "FOO")]);
        let err = handle(deps.as_mut(), mock_env(), info, HandleMsg::Bond {}).unwrap_err();
        assert_eq!(err, ContractError::MissingDenom(DENOM.to_string()));

        // cannot bond with 2 coins (even if one is correct)
        let info = mock_info(
            HumanAddr::from(USER1),
            &[coin(1234, DENOM), coin(5000, "BAR")],
        );
        let err = handle(deps.as_mut(), mock_env(), info, HandleMsg::Bond {}).unwrap_err();
        assert_eq!(err, ContractError::ExtraDenoms(DENOM.to_string()));

        // can bond with just the proper denom
        // cannot bond with incorrect denom
        let info = mock_info(HumanAddr::from(USER1), &[coin(500, DENOM)]);
        handle(deps.as_mut(), mock_env(), info, HandleMsg::Bond {}).unwrap();
    }

    #[test]
    fn ensure_bonding_edge_cases() {
        // use min_bond 0, tokens_per_weight 500
        let mut deps = mock_dependencies(&[]);
        do_init(deps.as_mut(), Uint128(100), Uint128(0), Duration::Height(5));

        // setting 50 tokens, gives us Some(0) weight
        // even setting to 1 token
        bond(deps.as_mut(), 50, 1, 102, 1);
        assert_users(deps.as_ref(), Some(0), Some(0), Some(1), None);

        // reducing to 0 token makes us None even with min_bond 0
        unbond(deps.as_mut(), 49, 1, 102, 2);
        assert_users(deps.as_ref(), Some(0), None, None, None);
    }
}
