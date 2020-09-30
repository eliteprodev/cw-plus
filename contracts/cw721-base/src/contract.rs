use cosmwasm_std::{
    from_binary, log, to_binary, Api, Binary, CosmosMsg, Env, Extern, HandleResponse, HumanAddr,
    InitResponse, Querier, StdError, StdResult, Storage,
};

use cw2::set_contract_version;
use cw721::{
    AllNftInfoResponse, ApprovedForAllResponse, ContractInfoResponse, Expiration, NftInfoResponse,
    NumTokensResponse, OwnerOfResponse,
};

use crate::msg::{HandleMsg, InitMsg, MinterResponse, QueryMsg};
use crate::state::{
    contract_info, contract_info_read, increment_tokens, mint, mint_read, num_tokens, operators,
    operators_read, tokens, tokens_read, Approval, TokenInfo,
};

// version info for migration info
const CONTRACT_NAME: &str = "crates.io:cw721-base";
const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn init<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    _env: Env,
    msg: InitMsg,
) -> StdResult<InitResponse> {
    set_contract_version(&mut deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;

    let info = ContractInfoResponse {
        name: msg.name,
        symbol: msg.symbol,
    };
    contract_info(&mut deps.storage).save(&info)?;
    let minter = deps.api.canonical_address(&msg.minter)?;
    mint(&mut deps.storage).save(&minter)?;
    Ok(InitResponse::default())
}

pub fn handle<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    msg: HandleMsg,
) -> StdResult<HandleResponse> {
    match msg {
        HandleMsg::Mint {
            token_id,
            owner,
            name,
            description,
            image,
        } => handle_mint(deps, env, token_id, owner, name, description, image),
        HandleMsg::Approve {
            spender,
            token_id,
            expires,
        } => handle_approve(deps, env, spender, token_id, expires),
        HandleMsg::Revoke { spender, token_id } => handle_revoke(deps, env, spender, token_id),
        HandleMsg::ApproveAll { operator, expires } => {
            handle_approve_all(deps, env, operator, expires)
        }
        HandleMsg::RevokeAll { operator } => handle_revoke_all(deps, env, operator),
        HandleMsg::TransferNft {
            recipient,
            token_id,
        } => handle_transfer_nft(deps, env, recipient, token_id),
        HandleMsg::SendNft {
            contract,
            token_id,
            msg,
        } => handle_send_nft(deps, env, contract, token_id, msg),
    }
}

pub fn handle_mint<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    token_id: String,
    owner: HumanAddr,
    name: String,
    description: Option<String>,
    image: Option<String>,
) -> StdResult<HandleResponse> {
    let minter = mint(&mut deps.storage).load()?;
    let minter_human = deps.api.human_address(&minter)?;

    if minter_human != env.message.sender {
        return Err(StdError::unauthorized());
    }

    // create the token
    let token = TokenInfo {
        owner: deps.api.canonical_address(&owner)?,
        approvals: vec![],
        name,
        description: description.unwrap_or_default(),
        image,
    };
    tokens(&mut deps.storage).update(token_id.as_bytes(), |old| match old {
        Some(_) => Err(StdError::generic_err("token_id already claimed")),
        None => Ok(token),
    })?;

    increment_tokens(&mut deps.storage)?;

    Ok(HandleResponse {
        messages: vec![],
        log: vec![
            log("action", "mint"),
            log("minter", minter_human),
            log("token_id", token_id),
        ],
        data: None,
    })
}

pub fn handle_transfer_nft<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    recipient: HumanAddr,
    token_id: String,
) -> StdResult<HandleResponse> {
    _transfer_nft(deps, &env, &recipient, &token_id)?;

    Ok(HandleResponse {
        messages: vec![],
        log: vec![
            log("action", "transfer_nft"),
            log("sender", env.message.sender),
            log("recipient", recipient),
            log("token_id", token_id),
        ],
        data: None,
    })
}

pub fn handle_send_nft<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    contract: HumanAddr,
    token_id: String,
    msg: Option<Binary>,
) -> StdResult<HandleResponse> {
    // Unwrap message first
    let msgs: Vec<CosmosMsg> = match &msg {
        None => vec![],
        Some(msg) => vec![from_binary(msg)?],
    };

    // Transfer token
    _transfer_nft(deps, &env, &contract, &token_id)?;

    // Send message
    Ok(HandleResponse {
        messages: msgs,
        log: vec![
            log("action", "send_nft"),
            log("sender", env.message.sender),
            log("recipient", contract),
            log("token_id", token_id),
        ],
        data: None,
    })
}

pub fn _transfer_nft<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: &Env,
    recipient: &HumanAddr,
    token_id: &str,
) -> StdResult<TokenInfo> {
    let mut token = tokens(&mut deps.storage).load(token_id.as_bytes())?;
    // ensure we have permissions
    check_can_send(&deps, env, &token)?;
    // set owner and remove existing approvals
    token.owner = deps.api.canonical_address(recipient)?;
    token.approvals = vec![];
    tokens(&mut deps.storage).save(token_id.as_bytes(), &token)?;
    Ok(token)
}

pub fn handle_approve<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    spender: HumanAddr,
    token_id: String,
    expires: Option<Expiration>,
) -> StdResult<HandleResponse> {
    _update_approvals(deps, &env, &spender, &token_id, true, expires)?;

    Ok(HandleResponse {
        messages: vec![],
        log: vec![
            log("action", "approve"),
            log("sender", env.message.sender),
            log("spender", spender),
            log("token_id", token_id),
        ],
        data: None,
    })
}

pub fn handle_revoke<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    spender: HumanAddr,
    token_id: String,
) -> StdResult<HandleResponse> {
    _update_approvals(deps, &env, &spender, &token_id, false, None)?;

    Ok(HandleResponse {
        messages: vec![],
        log: vec![
            log("action", "revoke"),
            log("sender", env.message.sender),
            log("spender", spender),
            log("token_id", token_id),
        ],
        data: None,
    })
}

pub fn _update_approvals<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: &Env,
    spender: &HumanAddr,
    token_id: &str,
    // if add == false, remove. if add == true, remove then set with this expiration
    add: bool,
    expires: Option<Expiration>,
) -> StdResult<TokenInfo> {
    let mut token = tokens(&mut deps.storage).load(token_id.as_bytes())?;
    // ensure we have permissions
    check_can_approve(&deps, &env, &token)?;

    // update the approval list (remove any for the same spender before adding)
    let spender_raw = deps.api.canonical_address(&spender)?;
    token.approvals = token
        .approvals
        .into_iter()
        .filter(|apr| apr.spender != spender_raw)
        .collect();

    // only difference between approve and revoke
    if add {
        // reject expired data as invalid
        let expires = expires.unwrap_or_default();
        if expires.is_expired(&env.block) {
            return Err(StdError::generic_err(
                "Cannot set approval that is already expired",
            ));
        }
        let approval = Approval {
            spender: spender_raw,
            expires,
        };
        token.approvals.push(approval);
    }

    tokens(&mut deps.storage).save(token_id.as_bytes(), &token)?;

    Ok(token)
}

pub fn handle_approve_all<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    operator: HumanAddr,
    expires: Option<Expiration>,
) -> StdResult<HandleResponse> {
    // reject expired data as invalid
    let expires = expires.unwrap_or_default();
    if expires.is_expired(&env.block) {
        return Err(StdError::generic_err(
            "Cannot set approval that is already expired",
        ));
    }

    // set the operator for us
    let sender_raw = deps.api.canonical_address(&env.message.sender)?;
    let operator_raw = deps.api.canonical_address(&operator)?;
    operators(&mut deps.storage, &sender_raw).save(operator_raw.as_slice(), &expires)?;

    Ok(HandleResponse {
        messages: vec![],
        log: vec![
            log("action", "approve_all"),
            log("sender", env.message.sender),
            log("operator", operator),
        ],
        data: None,
    })
}

pub fn handle_revoke_all<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    operator: HumanAddr,
) -> StdResult<HandleResponse> {
    let sender_raw = deps.api.canonical_address(&env.message.sender)?;
    let operator_raw = deps.api.canonical_address(&operator)?;
    operators(&mut deps.storage, &sender_raw).remove(operator_raw.as_slice());

    Ok(HandleResponse {
        messages: vec![],
        log: vec![
            log("action", "revoke_all"),
            log("sender", env.message.sender),
            log("operator", operator),
        ],
        data: None,
    })
}

/// returns true iff the sender can execute approve or reject on the contract
fn check_can_approve<S: Storage, A: Api, Q: Querier>(
    deps: &Extern<S, A, Q>,
    env: &Env,
    token: &TokenInfo,
) -> StdResult<()> {
    // owner can approve
    let sender_raw = deps.api.canonical_address(&env.message.sender)?;
    if token.owner == sender_raw {
        return Ok(());
    }
    // operator can approve
    let op = operators_read(&deps.storage, &token.owner).may_load(sender_raw.as_slice())?;
    match op {
        Some(ex) => {
            if ex.is_expired(&env.block) {
                Err(StdError::unauthorized())
            } else {
                Ok(())
            }
        }
        None => Err(StdError::unauthorized()),
    }
}

/// returns true iff the sender can transfer ownership of the token
fn check_can_send<S: Storage, A: Api, Q: Querier>(
    deps: &Extern<S, A, Q>,
    env: &Env,
    token: &TokenInfo,
) -> StdResult<()> {
    // owner can send
    let sender_raw = deps.api.canonical_address(&env.message.sender)?;
    if token.owner == sender_raw {
        return Ok(());
    }

    // any non-expired token approval can send
    if token
        .approvals
        .iter()
        .any(|apr| apr.spender == sender_raw && !apr.expires.is_expired(&env.block))
    {
        return Ok(());
    }

    // operator can send
    let op = operators_read(&deps.storage, &token.owner).may_load(sender_raw.as_slice())?;
    match op {
        Some(ex) => {
            if ex.is_expired(&env.block) {
                Err(StdError::unauthorized())
            } else {
                Ok(())
            }
        }
        None => Err(StdError::unauthorized()),
    }
}

pub fn query<S: Storage, A: Api, Q: Querier>(
    deps: &Extern<S, A, Q>,
    msg: QueryMsg,
) -> StdResult<Binary> {
    match msg {
        QueryMsg::Minter {} => to_binary(&query_minter(deps)?),
        QueryMsg::ContractInfo {} => to_binary(&query_contract_info(deps)?),
        QueryMsg::NftInfo { token_id } => to_binary(&query_nft_info(deps, token_id)?),
        QueryMsg::OwnerOf { token_id } => to_binary(&query_owner_of(deps, token_id)?),
        QueryMsg::AllNftInfo { token_id } => to_binary(&query_all_nft_info(deps, token_id)?),
        QueryMsg::ApprovedForAll { owner } => to_binary(&query_all_approvals(deps, owner)?),
        QueryMsg::NumTokens {} => to_binary(&query_num_tokens(deps)?),
    }
}

fn query_minter<S: Storage, A: Api, Q: Querier>(
    deps: &Extern<S, A, Q>,
) -> StdResult<MinterResponse> {
    let minter_raw = mint_read(&deps.storage).load()?;
    let minter = deps.api.human_address(&minter_raw)?;
    Ok(MinterResponse { minter })
}

fn query_contract_info<S: Storage, A: Api, Q: Querier>(
    deps: &Extern<S, A, Q>,
) -> StdResult<ContractInfoResponse> {
    contract_info_read(&deps.storage).load()
}

fn query_num_tokens<S: Storage, A: Api, Q: Querier>(
    deps: &Extern<S, A, Q>,
) -> StdResult<NumTokensResponse> {
    let count = num_tokens(&deps.storage)?;
    Ok(NumTokensResponse { count })
}

fn query_nft_info<S: Storage, A: Api, Q: Querier>(
    deps: &Extern<S, A, Q>,
    token_id: String,
) -> StdResult<NftInfoResponse> {
    let info = tokens_read(&deps.storage).load(token_id.as_bytes())?;
    Ok(NftInfoResponse {
        name: info.name,
        description: info.description,
        image: info.image,
    })
}

fn query_owner_of<S: Storage, A: Api, Q: Querier>(
    deps: &Extern<S, A, Q>,
    token_id: String,
) -> StdResult<OwnerOfResponse> {
    let info = tokens_read(&deps.storage).load(token_id.as_bytes())?;
    Ok(OwnerOfResponse {
        owner: deps.api.human_address(&info.owner)?,
        approvals: humanize_approvals(deps.api, &info)?,
    })
}

fn query_all_approvals<S: Storage, A: Api, Q: Querier>(
    _deps: &Extern<S, A, Q>,
    _owner: HumanAddr,
) -> StdResult<ApprovedForAllResponse> {
    // FIXME!
    /*
    let owner_raw = deps.api.canonical_address(&owner)?;
    let res: StdResult<Vec<_>> = operators_read(&deps.storage, &owner_raw)
        .range(None, None, Order::Ascending)
        .map(|item| {
            item.and_then(|(k, _)| {
                let human_addr = deps.api.human_address(&CanonicalAddr::from(k))?;
                Ok(human_addr)
            })
        })
        .collect();
    Ok(ApprovedForAllResponse { operators: res? })
    */
    Ok(ApprovedForAllResponse { operators: vec![] })
}

fn query_all_nft_info<S: Storage, A: Api, Q: Querier>(
    deps: &Extern<S, A, Q>,
    token_id: String,
) -> StdResult<AllNftInfoResponse> {
    let info = tokens_read(&deps.storage).load(token_id.as_bytes())?;
    Ok(AllNftInfoResponse {
        access: OwnerOfResponse {
            owner: deps.api.human_address(&info.owner)?,
            approvals: humanize_approvals(deps.api, &info)?,
        },
        info: NftInfoResponse {
            name: info.name,
            description: info.description,
            image: info.image,
        },
    })
}

fn humanize_approvals<A: Api>(api: A, info: &TokenInfo) -> StdResult<Vec<cw721::Approval>> {
    info.approvals
        .iter()
        .map(|apr| humanize_approval(api, apr))
        .collect()
}

fn humanize_approval<A: Api>(api: A, approval: &Approval) -> StdResult<cw721::Approval> {
    Ok(cw721::Approval {
        spender: api.human_address(&approval.spender)?,
        expires: approval.expires,
    })
}

#[cfg(test)]
mod tests {
    use cosmwasm_std::testing::{mock_dependencies, mock_env};
    use cosmwasm_std::{StdError, WasmMsg};

    use super::*;
    use cw721::ApprovedForAllResponse;

    const MINTER: &str = "merlin";
    const CONTRACT_NAME: &str = "Magic Power";
    const SYMBOL: &str = "MGK";

    fn setup_contract<S: Storage, A: Api, Q: Querier>(deps: &mut Extern<S, A, Q>) {
        let msg = InitMsg {
            name: CONTRACT_NAME.to_string(),
            symbol: SYMBOL.to_string(),
            minter: MINTER.into(),
        };
        let env = mock_env("creator", &[]);
        let res = init(deps, env, msg).unwrap();
        assert_eq!(0, res.messages.len());
    }

    #[test]
    fn proper_initialization() {
        let mut deps = mock_dependencies(20, &[]);

        let msg = InitMsg {
            name: CONTRACT_NAME.to_string(),
            symbol: SYMBOL.to_string(),
            minter: MINTER.into(),
        };
        let env = mock_env("creator", &[]);

        // we can just call .unwrap() to assert this was a success
        let res = init(&mut deps, env, msg).unwrap();
        assert_eq!(0, res.messages.len());

        // it worked, let's query the state
        let res = query_minter(&deps).unwrap();
        assert_eq!(MINTER, res.minter.as_str());
        let info = query_contract_info(&deps).unwrap();
        assert_eq!(
            info,
            ContractInfoResponse {
                name: CONTRACT_NAME.to_string(),
                symbol: SYMBOL.to_string(),
            }
        );

        let count = query_num_tokens(&deps).unwrap();
        assert_eq!(0, count.count);
    }

    #[test]
    fn minting() {
        let mut deps = mock_dependencies(20, &[]);
        setup_contract(&mut deps);

        let token_id = "petrify".to_string();
        let name = "Petrify with Gaze".to_string();
        let description = "Allows the owner to petrify anyone looking at him or her".to_string();

        let mint_msg = HandleMsg::Mint {
            token_id: token_id.clone(),
            owner: "medusa".into(),
            name: name.clone(),
            description: Some(description.clone()),
            image: None,
        };

        // random cannot mint
        let random = mock_env("random", &[]);
        let err = handle(&mut deps, random, mint_msg.clone()).unwrap_err();
        match err {
            StdError::Unauthorized { .. } => {}
            e => panic!("unexpected error: {}", e),
        }

        // minter can mint
        let allowed = mock_env(MINTER, &[]);
        let _ = handle(&mut deps, allowed, mint_msg.clone()).unwrap();

        // ensure num tokens increases
        let count = query_num_tokens(&deps).unwrap();
        assert_eq!(1, count.count);

        // unknown nft returns error
        let _ = query_nft_info(&deps, "unknown".to_string()).unwrap_err();

        // this nft info is correct
        let info = query_nft_info(&deps, token_id.clone()).unwrap();
        assert_eq!(
            info,
            NftInfoResponse {
                name: name.clone(),
                description: description.clone(),
                image: None,
            }
        );

        // owner info is correct
        let owner = query_owner_of(&deps, token_id.clone()).unwrap();
        assert_eq!(
            owner,
            OwnerOfResponse {
                owner: "medusa".into(),
                approvals: vec![],
            }
        );

        // TODO: Cannot mint same token again
    }

    #[test]
    fn transferring_nft() {
        let mut deps = mock_dependencies(20, &[]);
        setup_contract(&mut deps);

        // Mint a token
        let token_id = "melt".to_string();
        let name = "Melting power".to_string();
        let description = "Allows the owner to melt anyone looking at him or her".to_string();

        let mint_msg = HandleMsg::Mint {
            token_id: token_id.clone(),
            owner: "venus".into(),
            name: name.clone(),
            description: Some(description.clone()),
            image: None,
        };

        let minter = mock_env(MINTER, &[]);
        handle(&mut deps, minter, mint_msg).unwrap();

        // random cannot transfer
        let random = mock_env("random", &[]);
        let transfer_msg = HandleMsg::TransferNft {
            recipient: "random".into(),
            token_id: token_id.clone(),
        };

        let err = handle(&mut deps, random, transfer_msg.clone()).unwrap_err();

        match err {
            StdError::Unauthorized { .. } => {}
            e => panic!("unexpected error: {}", e),
        }

        // owner can
        let random = mock_env("venus", &[]);
        let transfer_msg = HandleMsg::TransferNft {
            recipient: "random".into(),
            token_id: token_id.clone(),
        };

        let res = handle(&mut deps, random, transfer_msg.clone()).unwrap();

        assert_eq!(
            res,
            HandleResponse {
                messages: vec![],
                log: vec![
                    log("action", "transfer_nft"),
                    log("sender", "venus"),
                    log("recipient", "random"),
                    log("token_id", token_id),
                ],
                data: None,
            }
        );
    }

    #[test]
    fn sending_nft() {
        let mut deps = mock_dependencies(20, &[]);
        setup_contract(&mut deps);

        // Mint a token
        let token_id = "melt".to_string();
        let name = "Melting power".to_string();
        let description = "Allows the owner to melt anyone looking at him or her".to_string();

        let mint_msg = HandleMsg::Mint {
            token_id: token_id.clone(),
            owner: "venus".into(),
            name: name.clone(),
            description: Some(description.clone()),
            image: None,
        };

        let minter = mock_env(MINTER, &[]);
        handle(&mut deps, minter, mint_msg).unwrap();

        // random cannot send
        let inner_msg = WasmMsg::Execute {
            contract_addr: "another_contract".into(),
            msg: to_binary("You now have the melting power").unwrap(),
            send: vec![],
        };
        let msg: CosmosMsg = CosmosMsg::Wasm(inner_msg);

        let send_msg = HandleMsg::SendNft {
            contract: "another_contract".into(),
            token_id: token_id.clone(),
            msg: Some(to_binary(&msg).unwrap()),
        };

        let random = mock_env("random", &[]);
        let err = handle(&mut deps, random, send_msg.clone()).unwrap_err();
        match err {
            StdError::Unauthorized { .. } => {}
            e => panic!("unexpected error: {}", e),
        }

        // but owner can
        let random = mock_env("venus", &[]);
        let res = handle(&mut deps, random, send_msg).unwrap();
        assert_eq!(
            res,
            HandleResponse {
                messages: vec![msg],
                log: vec![
                    log("action", "send_nft"),
                    log("sender", "venus"),
                    log("recipient", "another_contract"),
                    log("token_id", token_id),
                ],
                data: None,
            }
        );
    }

    #[test]
    fn approving_revoking() {
        let mut deps = mock_dependencies(20, &[]);
        setup_contract(&mut deps);

        // Mint a token
        let token_id = "grow".to_string();
        let name = "Growing power".to_string();
        let description = "Allows the owner to grow anything".to_string();

        let mint_msg = HandleMsg::Mint {
            token_id: token_id.clone(),
            owner: "demeter".into(),
            name: name.clone(),
            description: Some(description.clone()),
            image: None,
        };

        let minter = mock_env(MINTER, &[]);
        handle(&mut deps, minter, mint_msg).unwrap();

        // Give random transferring power
        let approve_msg = HandleMsg::Approve {
            spender: "random".into(),
            token_id: token_id.clone(),
            expires: None,
        };
        let owner = mock_env("demeter", &[]);
        let res = handle(&mut deps, owner, approve_msg).unwrap();
        assert_eq!(
            res,
            HandleResponse {
                messages: vec![],
                log: vec![
                    log("action", "approve"),
                    log("sender", "demeter"),
                    log("spender", "random"),
                    log("token_id", token_id.clone()),
                ],
                data: None,
            }
        );

        // random can now transfer
        let random = mock_env("random", &[]);
        let transfer_msg = HandleMsg::TransferNft {
            recipient: "person".into(),
            token_id: token_id.clone(),
        };
        handle(&mut deps, random, transfer_msg).unwrap();

        // Approvals are removed / cleared
        let query_msg = QueryMsg::OwnerOf {
            token_id: token_id.clone(),
        };
        let res: OwnerOfResponse = from_binary(&query(&deps, query_msg.clone()).unwrap()).unwrap();
        assert_eq!(
            res,
            OwnerOfResponse {
                owner: "person".into(),
                approvals: vec![],
            }
        );

        // Approve, revoke, and check for empty, to test revoke
        let approve_msg = HandleMsg::Approve {
            spender: "random".into(),
            token_id: token_id.clone(),
            expires: None,
        };
        let owner = mock_env("person", &[]);
        handle(&mut deps, owner.clone(), approve_msg).unwrap();

        let revoke_msg = HandleMsg::Revoke {
            spender: "random".into(),
            token_id: token_id.clone(),
        };
        handle(&mut deps, owner, revoke_msg).unwrap();

        // Approvals are now removed / cleared
        let res: OwnerOfResponse = from_binary(&query(&deps, query_msg).unwrap()).unwrap();
        assert_eq!(
            res,
            OwnerOfResponse {
                owner: "person".into(),
                approvals: vec![],
            }
        );
    }

    #[test]
    fn approving_all_revoking_all() {
        let mut deps = mock_dependencies(20, &[]);
        setup_contract(&mut deps);

        // Mint a couple tokens (from the same owner)
        let token_id1 = "grow1".to_string();
        let name1 = "Growing power".to_string();
        let description1 = "Allows the owner the power to grow anything".to_string();
        let token_id2 = "grow2".to_string();
        let name2 = "Growing power".to_string();
        let description2 = "Allows the owner the power to grow anything".to_string();

        let mint_msg1 = HandleMsg::Mint {
            token_id: token_id1.clone(),
            owner: "demeter".into(),
            name: name1.clone(),
            description: Some(description1.clone()),
            image: None,
        };

        let minter = mock_env(MINTER, &[]);
        handle(&mut deps, minter.clone(), mint_msg1).unwrap();

        let mint_msg2 = HandleMsg::Mint {
            token_id: token_id2.clone(),
            owner: "demeter".into(),
            name: name2.clone(),
            description: Some(description2.clone()),
            image: None,
        };

        handle(&mut deps, minter, mint_msg2).unwrap();

        // demeter gives random full (operator) power over her tokens
        let approve_all_msg = HandleMsg::ApproveAll {
            operator: "random".into(),
            expires: None,
        };
        let owner = mock_env("demeter", &[]);
        let res = handle(&mut deps, owner, approve_all_msg).unwrap();
        assert_eq!(
            res,
            HandleResponse {
                messages: vec![],
                log: vec![
                    log("action", "approve_all"),
                    log("sender", "demeter"),
                    log("operator", "random"),
                ],
                data: None,
            }
        );

        // random can now transfer
        let random = mock_env("random", &[]);
        let transfer_msg = HandleMsg::TransferNft {
            recipient: "person".into(),
            token_id: token_id1.clone(),
        };
        handle(&mut deps, random.clone(), transfer_msg).unwrap();

        // random can now send
        let inner_msg = WasmMsg::Execute {
            contract_addr: "another_contract".into(),
            msg: to_binary("You now also have the growing power").unwrap(),
            send: vec![],
        };
        let msg: CosmosMsg = CosmosMsg::Wasm(inner_msg);

        let send_msg = HandleMsg::SendNft {
            contract: "another_contract".into(),
            token_id: token_id2.clone(),
            msg: Some(to_binary(&msg).unwrap()),
        };
        handle(&mut deps, random, send_msg).unwrap();

        // Approve_all, revoke_all, and check for empty, to test revoke_all
        let approve_all_msg = HandleMsg::ApproveAll {
            operator: "operator".into(),
            expires: None,
        };
        // person is now the owner of the tokens
        let owner = mock_env("person", &[]);
        handle(&mut deps, owner.clone(), approve_all_msg).unwrap();

        let revoke_all_msg = HandleMsg::RevokeAll {
            operator: "operator".into(),
        };
        handle(&mut deps, owner, revoke_all_msg).unwrap();

        // Approvals are removed / cleared
        let query_msg = QueryMsg::ApprovedForAll {
            owner: "person".into(),
        };
        let res: ApprovedForAllResponse = from_binary(&query(&deps, query_msg).unwrap()).unwrap();
        assert_eq!(res, ApprovedForAllResponse { operators: vec![] });
    }
}
