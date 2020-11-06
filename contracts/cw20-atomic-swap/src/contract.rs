use cosmwasm_std::{
    attr, from_binary, to_binary, Api, BankMsg, Binary, CosmosMsg, Deps, DepsMut, Env,
    HandleResponse, HumanAddr, InitResponse, MessageInfo, StdResult, WasmMsg,
};
use sha2::{Digest, Sha256};

use cw0::calc_range_start_string;
use cw2::set_contract_version;
use cw20::{Balance, Cw20Coin, Cw20CoinHuman, Cw20HandleMsg, Cw20ReceiveMsg};

use crate::error::ContractError;
use crate::msg::{
    is_valid_name, BalanceHuman, CreateMsg, DetailsResponse, HandleMsg, InitMsg, ListResponse,
    QueryMsg, ReceiveMsg,
};
use crate::state::{all_swap_ids, atomic_swaps, atomic_swaps_read, AtomicSwap};

// Version info, for migration info
const CONTRACT_NAME: &str = "crates.io:cw20-atomic-swap";
const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn init(
    deps: DepsMut,
    _env: Env,
    _info: MessageInfo,
    _msg: InitMsg,
) -> StdResult<InitResponse> {
    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;
    // No setup
    Ok(InitResponse::default())
}

pub fn handle(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: HandleMsg,
) -> Result<HandleResponse, ContractError> {
    match msg {
        HandleMsg::Create(msg) => {
            let sent_funds = info.sent_funds.clone();
            try_create(deps, env, info, msg, Balance::from(sent_funds))
        }
        HandleMsg::Release { id, preimage } => try_release(deps, env, id, preimage),
        HandleMsg::Refund { id } => try_refund(deps, env, id),
        HandleMsg::Receive(msg) => try_receive(deps, env, info, msg),
    }
}

pub fn try_receive(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    wrapper: Cw20ReceiveMsg,
) -> Result<HandleResponse, ContractError> {
    let msg: ReceiveMsg = match wrapper.msg {
        Some(bin) => Ok(from_binary(&bin)?),
        None => Err(ContractError::NoData {}),
    }?;
    let token = Cw20Coin {
        address: deps.api.canonical_address(&info.sender)?,
        amount: wrapper.amount,
    };
    // we need to update the info... so the original sender is the one authorizing with these tokens
    let orig_info = MessageInfo {
        sender: wrapper.sender,
        sent_funds: info.sent_funds,
    };
    match msg {
        ReceiveMsg::Create(create) => {
            try_create(deps, env, orig_info, create, Balance::Cw20(token))
        }
    }
}

pub fn try_create(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: CreateMsg,
    balance: Balance,
) -> Result<HandleResponse, ContractError> {
    if !is_valid_name(&msg.id) {
        return Err(ContractError::InvalidId {});
    }

    // this ignores 0 value coins, must have one or more with positive balance
    if balance.is_empty() {
        return Err(ContractError::EmptyBalance {});
    }

    // Ensure this is 32 bytes hex-encoded, and decode
    let hash = parse_hex_32(&msg.hash)?;

    if msg.expires.is_expired(&env.block) {
        return Err(ContractError::Expired {});
    }

    let recipient_raw = deps.api.canonical_address(&msg.recipient)?;

    let swap = AtomicSwap {
        hash: Binary(hash),
        recipient: recipient_raw,
        source: deps.api.canonical_address(&info.sender)?,
        expires: msg.expires,
        balance,
    };

    // Try to store it, fail if the id already exists (unmodifiable swaps)
    atomic_swaps(deps.storage).update(msg.id.as_bytes(), |existing| match existing {
        None => Ok(swap),
        Some(_) => Err(ContractError::AlreadyExists {}),
    })?;

    let mut res = HandleResponse::default();
    res.attributes = vec![
        attr("action", "create"),
        attr("id", msg.id),
        attr("hash", msg.hash),
        attr("recipient", msg.recipient),
    ];
    Ok(res)
}

pub fn try_release(
    deps: DepsMut,
    env: Env,
    id: String,
    preimage: String,
) -> Result<HandleResponse, ContractError> {
    let swap = atomic_swaps_read(deps.storage).load(id.as_bytes())?;
    if swap.is_expired(&env.block) {
        return Err(ContractError::Expired {});
    }

    let hash = Sha256::digest(&parse_hex_32(&preimage)?);
    if hash.as_slice() != swap.hash.as_slice() {
        return Err(ContractError::InvalidPreimage {});
    }

    let rcpt = deps.api.human_address(&swap.recipient)?;

    // Delete the swap
    atomic_swaps(deps.storage).remove(id.as_bytes());

    // Send all tokens out
    let msgs = send_tokens(deps.api, &env.contract.address, &rcpt, swap.balance)?;
    Ok(HandleResponse {
        messages: msgs,
        attributes: vec![
            attr("action", "release"),
            attr("id", id),
            attr("preimage", preimage),
            attr("to", rcpt),
        ],
        data: None,
    })
}

pub fn try_refund(deps: DepsMut, env: Env, id: String) -> Result<HandleResponse, ContractError> {
    let swap = atomic_swaps_read(deps.storage).load(id.as_bytes())?;
    // Anyone can try to refund, as long as the contract is expired
    if !swap.is_expired(&env.block) {
        return Err(ContractError::NotExpired {});
    }

    let rcpt = deps.api.human_address(&swap.source)?;

    // We delete the swap
    atomic_swaps(deps.storage).remove(id.as_bytes());

    let msgs = send_tokens(deps.api, &env.contract.address, &rcpt, swap.balance)?;
    Ok(HandleResponse {
        messages: msgs,
        attributes: vec![attr("action", "refund"), attr("id", id), attr("to", rcpt)],
        data: None,
    })
}

fn parse_hex_32(data: &str) -> Result<Vec<u8>, ContractError> {
    match hex::decode(data) {
        Ok(bin) => {
            if bin.len() == 32 {
                Ok(bin)
            } else {
                Err(ContractError::InvalidHash(bin.len() * 2))
            }
        }
        Err(e) => Err(ContractError::ParseError(e.to_string())),
    }
}

fn send_tokens(
    api: &dyn Api,
    from: &HumanAddr,
    to: &HumanAddr,
    amount: Balance,
) -> StdResult<Vec<CosmosMsg>> {
    if amount.is_empty() {
        Ok(vec![])
    } else {
        match amount {
            Balance::Native(coins) => {
                let msg = BankMsg::Send {
                    from_address: from.into(),
                    to_address: to.into(),
                    amount: coins.into_vec(),
                };
                Ok(vec![msg.into()])
            }
            Balance::Cw20(coin) => {
                let msg = Cw20HandleMsg::Transfer {
                    recipient: to.into(),
                    amount: coin.amount,
                };
                let exec = WasmMsg::Execute {
                    contract_addr: api.human_address(&coin.address)?,
                    msg: to_binary(&msg)?,
                    send: vec![],
                };
                Ok(vec![exec.into()])
            }
        }
    }
}

pub fn query(deps: Deps, _env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::List { start_after, limit } => to_binary(&query_list(deps, start_after, limit)?),
        QueryMsg::Details { id } => to_binary(&query_details(deps, id)?),
    }
}

fn query_details(deps: Deps, id: String) -> StdResult<DetailsResponse> {
    let swap = atomic_swaps_read(deps.storage).load(id.as_bytes())?;

    // Convert balance to human balance
    let balance_human = match swap.balance {
        Balance::Native(coins) => BalanceHuman::Native(coins.into_vec()),
        Balance::Cw20(coin) => BalanceHuman::Cw20(Cw20CoinHuman {
            address: deps.api.human_address(&coin.address)?,
            amount: coin.amount,
        }),
    };

    let details = DetailsResponse {
        id,
        hash: hex::encode(swap.hash.as_slice()),
        recipient: deps.api.human_address(&swap.recipient)?,
        source: deps.api.human_address(&swap.source)?,
        expires: swap.expires,
        balance: balance_human,
    };
    Ok(details)
}

// Settings for pagination
const MAX_LIMIT: u32 = 30;
const DEFAULT_LIMIT: u32 = 10;

fn query_list(
    deps: Deps,
    start_after: Option<String>,
    limit: Option<u32>,
) -> StdResult<ListResponse> {
    let start = calc_range_start_string(start_after);
    let limit = limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT) as usize;
    Ok(ListResponse {
        swaps: all_swap_ids(deps.storage, start, limit)?,
    })
}

#[cfg(test)]
mod tests {
    use cosmwasm_std::testing::{mock_dependencies, mock_env, mock_info, MOCK_CONTRACT_ADDR};
    use cosmwasm_std::{coins, from_binary, CosmosMsg, StdError, Uint128};

    use cw20::Expiration;

    use super::*;

    fn preimage() -> String {
        hex::encode(b"This is a string, 32 bytes long.")
    }

    fn custom_preimage(int: u16) -> String {
        hex::encode(format!("This is a custom string: {:>7}", int))
    }

    fn real_hash() -> String {
        hex::encode(&Sha256::digest(&hex::decode(preimage()).unwrap()))
    }

    fn custom_hash(int: u16) -> String {
        hex::encode(&Sha256::digest(&hex::decode(custom_preimage(int)).unwrap()))
    }

    fn mock_env_height(height: u64) -> Env {
        let mut env = mock_env();
        env.block.height = height;
        env
    }

    #[test]
    fn test_init() {
        let mut deps = mock_dependencies(&[]);

        // Init an empty contract
        let init_msg = InitMsg {};
        let info = mock_info("anyone", &[]);
        let res = init(deps.as_mut(), mock_env(), info, init_msg).unwrap();
        assert_eq!(0, res.messages.len());
    }

    #[test]
    fn test_create() {
        let mut deps = mock_dependencies(&[]);

        let info = mock_info("anyone", &[]);
        init(deps.as_mut(), mock_env(), info, InitMsg {}).unwrap();

        let sender = HumanAddr::from("sender0001");
        let balance = coins(100, "tokens");

        // Cannot create, invalid ids
        let info = mock_info(&sender, &balance);
        for id in vec!["sh", "atomic_swap_id_too_long"] {
            let create = CreateMsg {
                id: id.to_string(),
                hash: real_hash(),
                recipient: HumanAddr::from("rcpt0001"),
                expires: Expiration::AtHeight(123456),
            };
            let res = handle(
                deps.as_mut(),
                mock_env(),
                info.clone(),
                HandleMsg::Create(create.clone()),
            );
            match res {
                Ok(_) => panic!("expected error"),
                Err(ContractError::InvalidId {}) => {}
                Err(e) => panic!("unexpected error: {:?}", e),
            }
        }

        // Cannot create, no funds
        let info = mock_info(&sender, &vec![]);
        let create = CreateMsg {
            id: "swap0001".to_string(),
            hash: real_hash(),
            recipient: "rcpt0001".into(),
            expires: Expiration::AtHeight(123456),
        };
        let res = handle(
            deps.as_mut(),
            mock_env(),
            info,
            HandleMsg::Create(create.clone()),
        );
        match res {
            Ok(_) => panic!("expected error"),
            Err(ContractError::EmptyBalance {}) => {}
            Err(e) => panic!("unexpected error: {:?}", e),
        }

        // Cannot create, expired
        let info = mock_info(&sender, &balance);
        let create = CreateMsg {
            id: "swap0001".to_string(),
            hash: real_hash(),
            recipient: "rcpt0001".into(),
            expires: Expiration::AtTime(1),
        };
        let res = handle(
            deps.as_mut(),
            mock_env(),
            info,
            HandleMsg::Create(create.clone()),
        );
        match res {
            Ok(_) => panic!("expected error"),
            Err(ContractError::Expired) => {}
            Err(e) => panic!("unexpected error: {:?}", e),
        }

        // Cannot create, invalid hash
        let info = mock_info(&sender, &balance);
        let create = CreateMsg {
            id: "swap0001".to_string(),
            hash: "bu115h17".to_string(),
            recipient: "rcpt0001".into(),
            expires: Expiration::AtHeight(123456),
        };
        let res = handle(
            deps.as_mut(),
            mock_env(),
            info,
            HandleMsg::Create(create.clone()),
        );
        match res {
            Ok(_) => panic!("expected error"),
            Err(ContractError::ParseError(msg)) => {
                assert_eq!(msg, "Invalid character \'u\' at position 1".to_string())
            }
            Err(e) => panic!("unexpected error: {:?}", e),
        }

        // Can create, all valid
        let info = mock_info(&sender, &balance);
        let create = CreateMsg {
            id: "swap0001".to_string(),
            hash: real_hash(),
            recipient: "rcpt0001".into(),
            expires: Expiration::AtHeight(123456),
        };
        let res = handle(
            deps.as_mut(),
            mock_env(),
            info,
            HandleMsg::Create(create.clone()),
        )
        .unwrap();
        assert_eq!(0, res.messages.len());
        assert_eq!(attr("action", "create"), res.attributes[0]);

        // Cannot re-create (modify), already existing
        let new_balance = coins(1, "tokens");
        let info = mock_info(&sender, &new_balance);
        let create = CreateMsg {
            id: "swap0001".to_string(),
            hash: real_hash(),
            recipient: "rcpt0001".into(),
            expires: Expiration::AtHeight(123456),
        };
        let res = handle(
            deps.as_mut(),
            mock_env(),
            info,
            HandleMsg::Create(create.clone()),
        );
        match res {
            Ok(_) => panic!("expected error"),
            Err(ContractError::AlreadyExists {}) => {}
            Err(e) => panic!("unexpected error: {:?}", e),
        }
    }

    #[test]
    fn test_release() {
        let mut deps = mock_dependencies(&[]);

        let info = mock_info("anyone", &[]);
        init(deps.as_mut(), mock_env(), info, InitMsg {}).unwrap();

        let sender = HumanAddr::from("sender0001");
        let balance = coins(1000, "tokens");

        let info = mock_info(&sender, &balance);
        let create = CreateMsg {
            id: "swap0001".to_string(),
            hash: real_hash(),
            recipient: "rcpt0001".into(),
            expires: Expiration::AtHeight(123456),
        };
        handle(
            deps.as_mut(),
            mock_env(),
            info.clone(),
            HandleMsg::Create(create.clone()),
        )
        .unwrap();

        // Anyone can attempt release
        let info = mock_info("somebody", &[]);

        // Cannot release, wrong id
        let release = HandleMsg::Release {
            id: "swap0002".to_string(),
            preimage: preimage(),
        };
        let res = handle(deps.as_mut(), mock_env(), info.clone(), release);
        match res {
            Ok(_) => panic!("expected error"),
            Err(ContractError::Std(StdError::NotFound { .. })) => {}
            Err(e) => panic!("unexpected error: {:?}", e),
        }

        // Cannot release, invalid hash
        let release = HandleMsg::Release {
            id: "swap0001".to_string(),
            preimage: "bu115h17".to_string(),
        };
        let res = handle(deps.as_mut(), mock_env(), info.clone(), release);
        match res {
            Ok(_) => panic!("expected error"),
            Err(ContractError::ParseError(msg)) => {
                assert_eq!(msg, "Invalid character \'u\' at position 1".to_string())
            }
            Err(e) => panic!("unexpected error: {:?}", e),
        }

        // Cannot release, wrong hash
        let release = HandleMsg::Release {
            id: "swap0001".to_string(),
            preimage: hex::encode(b"This is 32 bytes, but incorrect."),
        };
        let res = handle(deps.as_mut(), mock_env(), info.clone(), release);
        match res {
            Ok(_) => panic!("expected error"),
            Err(ContractError::InvalidPreimage {}) => {}
            Err(e) => panic!("unexpected error: {:?}", e),
        }

        // Cannot release, expired
        let env = mock_env_height(123457);
        let info = mock_info("somebody", &[]);
        let release = HandleMsg::Release {
            id: "swap0001".to_string(),
            preimage: preimage(),
        };
        let res = handle(deps.as_mut(), env, info, release);
        match res {
            Ok(_) => panic!("expected error"),
            Err(ContractError::Expired) => {}
            Err(e) => panic!("unexpected error: {:?}", e),
        }

        // Can release, valid id, valid hash, and not expired
        let info = mock_info("somebody", &[]);
        let release = HandleMsg::Release {
            id: "swap0001".to_string(),
            preimage: preimage(),
        };
        let res = handle(deps.as_mut(), mock_env(), info.clone(), release.clone()).unwrap();
        assert_eq!(attr("action", "release"), res.attributes[0]);
        assert_eq!(1, res.messages.len());
        assert_eq!(
            res.messages[0],
            CosmosMsg::Bank(BankMsg::Send {
                from_address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                to_address: create.recipient,
                amount: balance,
            })
        );

        // Cannot release again
        let res = handle(deps.as_mut(), mock_env(), info.clone(), release);
        match res.unwrap_err() {
            ContractError::Std(StdError::NotFound { .. }) => {}
            e => panic!("Expected NotFound, got {}", e),
        }
    }

    #[test]
    fn test_refund() {
        let mut deps = mock_dependencies(&[]);

        let info = mock_info("anyone", &[]);
        init(deps.as_mut(), mock_env(), info, InitMsg {}).unwrap();

        let sender = HumanAddr::from("sender0001");
        let balance = coins(1000, "tokens");

        let info = mock_info(&sender, &balance);
        let create = CreateMsg {
            id: "swap0001".to_string(),
            hash: real_hash(),
            recipient: "rcpt0001".into(),
            expires: Expiration::AtHeight(123456),
        };
        handle(
            deps.as_mut(),
            mock_env(),
            info.clone(),
            HandleMsg::Create(create.clone()),
        )
        .unwrap();

        // Anyone can attempt refund
        let info = mock_info("somebody", &[]);

        // Cannot refund, wrong id
        let refund = HandleMsg::Refund {
            id: "swap0002".to_string(),
        };
        let res = handle(deps.as_mut(), mock_env(), info.clone(), refund);
        match res {
            Ok(_) => panic!("expected error"),
            Err(ContractError::Std(StdError::NotFound { .. })) => {}
            Err(e) => panic!("unexpected error: {:?}", e),
        }

        // Cannot refund, not expired yet
        let refund = HandleMsg::Refund {
            id: "swap0001".to_string(),
        };
        let res = handle(deps.as_mut(), mock_env(), info.clone(), refund);
        match res {
            Ok(_) => panic!("expected error"),
            Err(ContractError::NotExpired {}) => {}
            Err(e) => panic!("unexpected error: {:?}", e),
        }

        // Anyone can refund, if already expired
        let env = mock_env_height(123457);
        let info = mock_info("somebody", &[]);
        let refund = HandleMsg::Refund {
            id: "swap0001".to_string(),
        };
        let res = handle(deps.as_mut(), env.clone(), info.clone(), refund.clone()).unwrap();
        assert_eq!(attr("action", "refund"), res.attributes[0]);
        assert_eq!(1, res.messages.len());
        assert_eq!(
            res.messages[0],
            CosmosMsg::Bank(BankMsg::Send {
                from_address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                to_address: sender,
                amount: balance,
            })
        );

        // Cannot refund again
        let res = handle(deps.as_mut(), env, info, refund);
        match res.unwrap_err() {
            ContractError::Std(StdError::NotFound { .. }) => {}
            e => panic!("Expected NotFound, got {}", e),
        }
    }

    #[test]
    fn test_query() {
        let mut deps = mock_dependencies(&[]);

        let info = mock_info("anyone", &[]);
        init(deps.as_mut(), mock_env(), info, InitMsg {}).unwrap();

        let sender1 = HumanAddr::from("sender0001");
        let sender2 = HumanAddr::from("sender0002");
        // Same balance for simplicity
        let balance = coins(1000, "tokens");

        // Create a couple swaps (same hash for simplicity)
        let info = mock_info(&sender1, &balance);
        let create1 = CreateMsg {
            id: "swap0001".to_string(),
            hash: custom_hash(1),
            recipient: "rcpt0001".into(),
            expires: Expiration::AtHeight(123456),
        };
        handle(
            deps.as_mut(),
            mock_env(),
            info.clone(),
            HandleMsg::Create(create1.clone()),
        )
        .unwrap();

        let info = mock_info(&sender2, &balance);
        let create2 = CreateMsg {
            id: "swap0002".to_string(),
            hash: custom_hash(2),
            recipient: "rcpt0002".into(),
            expires: Expiration::AtTime(2_000_000_000),
        };
        handle(
            deps.as_mut(),
            mock_env(),
            info.clone(),
            HandleMsg::Create(create2.clone()),
        )
        .unwrap();

        // Get the list of ids
        let query_msg = QueryMsg::List {
            start_after: None,
            limit: None,
        };
        let ids: ListResponse =
            from_binary(&query(deps.as_ref(), mock_env(), query_msg).unwrap()).unwrap();
        assert_eq!(2, ids.swaps.len());
        assert_eq!(vec!["swap0001", "swap0002"], ids.swaps);

        // Get the details for the first swap id
        let query_msg = QueryMsg::Details {
            id: ids.swaps[0].clone(),
        };
        let res: DetailsResponse =
            from_binary(&query(deps.as_ref(), mock_env(), query_msg).unwrap()).unwrap();
        assert_eq!(
            res,
            DetailsResponse {
                id: create1.id,
                hash: create1.hash,
                recipient: create1.recipient,
                source: sender1,
                expires: create1.expires,
                balance: BalanceHuman::Native(balance.clone()),
            }
        );

        // Get the details for the second swap id
        let query_msg = QueryMsg::Details {
            id: ids.swaps[1].clone(),
        };
        let res: DetailsResponse =
            from_binary(&query(deps.as_ref(), mock_env(), query_msg).unwrap()).unwrap();
        assert_eq!(
            res,
            DetailsResponse {
                id: create2.id,
                hash: create2.hash,
                recipient: create2.recipient,
                source: sender2,
                expires: create2.expires,
                balance: BalanceHuman::Native(balance),
            }
        );
    }

    #[test]
    fn test_native_cw20_swap() {
        let mut deps = mock_dependencies(&[]);

        // Create the contract
        let info = mock_info("anyone", &[]);
        let res = init(deps.as_mut(), mock_env(), info, InitMsg {}).unwrap();
        assert_eq!(0, res.messages.len());

        // Native side (offer)
        let native_sender = HumanAddr::from("A_on_X");
        let native_rcpt = HumanAddr::from("B_on_X");
        let native_coins = coins(1000, "tokens_native");

        // Create the Native swap offer
        let native_swap_id = "native_swap".to_string();
        let create = CreateMsg {
            id: native_swap_id.clone(),
            hash: real_hash(),
            recipient: native_rcpt.clone(),
            expires: Expiration::AtHeight(123456),
        };
        let info = mock_info(&native_sender, &native_coins);
        let res = handle(deps.as_mut(), mock_env(), info, HandleMsg::Create(create)).unwrap();
        assert_eq!(0, res.messages.len());
        assert_eq!(attr("action", "create"), res.attributes[0]);

        // Cw20 side (counter offer (1:1000))
        let cw20_sender = HumanAddr::from("B_on_Y");
        let cw20_rcpt = HumanAddr::from("A_on_Y");
        let cw20_coin = Cw20CoinHuman {
            address: HumanAddr::from("my_cw20_token"),
            amount: Uint128(1),
        };

        // Create the Cw20 side swap counter offer
        let cw20_swap_id = "cw20_swap".to_string();
        let create = CreateMsg {
            id: cw20_swap_id.clone(),
            hash: real_hash(),
            recipient: cw20_rcpt.clone(),
            expires: Expiration::AtHeight(123000),
        };
        let receive = Cw20ReceiveMsg {
            sender: cw20_sender,
            amount: cw20_coin.amount,
            msg: Some(to_binary(&HandleMsg::Create(create)).unwrap()),
        };
        let token_contract = cw20_coin.address;
        let info = mock_info(&token_contract, &[]);
        let res = handle(
            deps.as_mut(),
            mock_env(),
            info,
            HandleMsg::Receive(receive.clone()),
        )
        .unwrap();
        assert_eq!(0, res.messages.len());
        assert_eq!(attr("action", "create"), res.attributes[0]);

        // Somebody (typically, A) releases the swap side on the Cw20 (Y) blockchain,
        // using her knowledge of the preimage
        let info = mock_info("somebody", &[]);
        let res = handle(
            deps.as_mut(),
            mock_env(),
            info,
            HandleMsg::Release {
                id: cw20_swap_id.clone(),
                preimage: preimage(),
            },
        )
        .unwrap();
        assert_eq!(1, res.messages.len());
        assert_eq!(attr("action", "release"), res.attributes[0]);
        assert_eq!(attr("id", cw20_swap_id), res.attributes[1]);

        // Verify the resulting Cw20 transfer message
        let send_msg = Cw20HandleMsg::Transfer {
            recipient: cw20_rcpt,
            amount: cw20_coin.amount,
        };
        assert_eq!(
            res.messages[0],
            CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: token_contract,
                msg: to_binary(&send_msg).unwrap(),
                send: vec![],
            })
        );

        // Now somebody (typically, B) releases the original offer on the Native (X) blockchain,
        // using the (now public) preimage
        let info = mock_info("other_somebody", &[]);

        // First, let's obtain the preimage from the logs of the release() transaction on Y
        let preimage_attr = &res.attributes[2];
        assert_eq!("preimage", preimage_attr.key);
        let preimage = preimage_attr.value.clone();

        let release = HandleMsg::Release {
            id: native_swap_id.clone(),
            preimage,
        };
        let res = handle(deps.as_mut(), mock_env(), info.clone(), release.clone()).unwrap();
        assert_eq!(1, res.messages.len());
        assert_eq!(attr("action", "release"), res.attributes[0]);
        assert_eq!(attr("id", native_swap_id), res.attributes[1]);

        // Verify the resulting Native send message
        assert_eq!(
            res.messages[0],
            CosmosMsg::Bank(BankMsg::Send {
                from_address: HumanAddr::from(MOCK_CONTRACT_ADDR),
                to_address: native_rcpt,
                amount: native_coins,
            })
        );
    }
}
