#[cfg(not(feature = "library"))]
use cosmwasm_std::entry_point;
use cosmwasm_std::{
    attr, to_binary, Addr, Binary, Deps, DepsMut, Env, MessageInfo, Response, StdResult, Uint128,
    WasmMsg,
};
use cw2::set_contract_version;
use cw20::Cw20ExecuteMsg;
use cw_storage_plus::U8Key;
use sha3::Digest;
use std::convert::TryInto;

use crate::error::ContractError;
use crate::msg::{
    ConfigResponse, ExecuteMsg, InstantiateMsg, IsClaimedResponse, LatestStageResponse,
    MerkleRootResponse, MigrateMsg, QueryMsg,
};
use crate::state::{Config, CLAIM, CONFIG, LATEST_STAGE, MERKLE_ROOT};

// Version info, for migration info
const CONTRACT_NAME: &str = "crates.io:cw20-merkle-airdrop";
const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn instantiate(
    deps: DepsMut,
    _env: Env,
    info: MessageInfo,
    msg: InstantiateMsg,
) -> StdResult<Response> {
    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;

    let owner = msg
        .owner
        .map_or(Ok(info.sender), |o| deps.api.addr_validate(&o))?;

    let config = Config {
        owner: Some(owner),
        cw20_token_address: deps.api.addr_validate(&msg.cw20_token_address)?,
    };
    CONFIG.save(deps.storage, &config)?;

    let stage = 0;
    LATEST_STAGE.save(deps.storage, &stage)?;

    Ok(Response::default())
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn execute(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
) -> Result<Response, ContractError> {
    match msg {
        ExecuteMsg::UpdateConfig { new_owner } => execute_update_config(deps, env, info, new_owner),
        ExecuteMsg::RegisterMerkleRoot { merkle_root } => {
            execute_register_merkle_root(deps, env, info, merkle_root)
        }
        ExecuteMsg::Claim {
            stage,
            amount,
            proof,
        } => execute_claim(deps, env, info, stage, amount, proof),
    }
}

pub fn execute_update_config(
    deps: DepsMut,
    _env: Env,
    info: MessageInfo,
    new_owner: Option<String>,
) -> Result<Response, ContractError> {
    // authorize owner
    let cfg = CONFIG.load(deps.storage)?;
    let owner = cfg.owner.ok_or(ContractError::Unauthorized {})?;
    if info.sender != owner {
        return Err(ContractError::Unauthorized {});
    }

    // if owner some validated to addr, otherwise set to none
    let mut tmp_owner = None;
    if let Some(addr) = new_owner {
        tmp_owner = Some(deps.api.addr_validate(&addr)?)
    }

    CONFIG.update(deps.storage, |mut exists| -> StdResult<_> {
        exists.owner = tmp_owner;
        Ok(exists)
    })?;

    Ok(Response::new().add_attribute("action", "update_config"))
}

pub fn execute_register_merkle_root(
    deps: DepsMut,
    _env: Env,
    info: MessageInfo,
    merkle_root: String,
) -> Result<Response, ContractError> {
    let cfg = CONFIG.load(deps.storage)?;

    // if owner set validate, otherwise unauthorized
    let owner = cfg.owner.ok_or(ContractError::Unauthorized {})?;
    if info.sender != owner {
        return Err(ContractError::Unauthorized {});
    }

    // check merkle root length
    let mut root_buf: [u8; 32] = [0; 32];
    hex::decode_to_slice(merkle_root.to_string(), &mut root_buf)?;

    let stage = LATEST_STAGE.update(deps.storage, |stage| -> StdResult<_> { Ok(stage + 1) })?;

    MERKLE_ROOT.save(deps.storage, U8Key::from(stage), &merkle_root)?;
    LATEST_STAGE.save(deps.storage, &stage)?;

    Ok(Response::new().add_attributes(vec![
        attr("action", "register_merkle_root"),
        attr("stage", stage.to_string()),
        attr("merkle_root", merkle_root),
    ]))
}

pub fn execute_claim(
    deps: DepsMut,
    _env: Env,
    info: MessageInfo,
    stage: u8,
    amount: Uint128,
    proof: Vec<String>,
) -> Result<Response, ContractError> {
    // verify not claimed
    let claimed = CLAIM.may_load(deps.storage, (&info.sender, U8Key::from(stage)))?;
    if claimed.is_some() {
        return Err(ContractError::Claimed {});
    }

    let config = CONFIG.load(deps.storage)?;
    let merkle_root = MERKLE_ROOT.load(deps.storage, stage.into())?;

    let user_input = format!("{}{}", info.sender, amount);
    let hash = sha3::Keccak256::digest(user_input.as_bytes())
        .as_slice()
        .try_into()
        .map_err(|_| ContractError::WrongLength {})?;

    let hash = proof.into_iter().try_fold(hash, |hash, p| {
        let mut proof_buf = [0; 32];
        hex::decode_to_slice(p, &mut proof_buf)?;
        let mut hashes = [hash, proof_buf];
        hashes.sort_unstable();
        sha3::Keccak256::digest(&hashes.concat())
            .as_slice()
            .try_into()
            .map_err(|_| ContractError::WrongLength {})
    })?;

    let mut root_buf: [u8; 32] = [0; 32];
    hex::decode_to_slice(merkle_root, &mut root_buf)?;
    if root_buf != hash {
        return Err(ContractError::VerificationFailed {});
    }

    // Update claim index to the current stage
    CLAIM.save(deps.storage, (&info.sender, stage.into()), &true)?;

    let res = Response::new()
        .add_message(WasmMsg::Execute {
            contract_addr: config.cw20_token_address.to_string(),
            funds: vec![],
            msg: to_binary(&Cw20ExecuteMsg::Transfer {
                recipient: info.sender.to_string(),
                amount,
            })?,
        })
        .add_attributes(vec![
            attr("action", "claim"),
            attr("stage", stage.to_string()),
            attr("address", info.sender),
            attr("amount", amount),
        ]);
    Ok(res)
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn query(deps: Deps, _env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::Config {} => to_binary(&query_config(deps)?),
        QueryMsg::MerkleRoot { stage } => to_binary(&query_merkle_root(deps, stage)?),
        QueryMsg::LatestStage {} => to_binary(&query_latest_stage(deps)?),
        QueryMsg::IsClaimed { stage, address } => {
            to_binary(&query_is_claimed(deps, stage, address)?)
        }
    }
}

pub fn query_config(deps: Deps) -> StdResult<ConfigResponse> {
    let cfg = CONFIG.load(deps.storage)?;
    Ok(ConfigResponse {
        owner: cfg.owner.map(|o| o.to_string()),
        cw20_token_address: cfg.cw20_token_address.to_string(),
    })
}

pub fn query_merkle_root(deps: Deps, stage: u8) -> StdResult<MerkleRootResponse> {
    let merkle_root = MERKLE_ROOT.load(deps.storage, U8Key::from(stage))?;
    let resp = MerkleRootResponse { stage, merkle_root };

    Ok(resp)
}

pub fn query_latest_stage(deps: Deps) -> StdResult<LatestStageResponse> {
    let latest_stage = LATEST_STAGE.load(deps.storage)?;
    let resp = LatestStageResponse { latest_stage };

    Ok(resp)
}

pub fn query_is_claimed(deps: Deps, stage: u8, address: String) -> StdResult<IsClaimedResponse> {
    let key: (&Addr, U8Key) = (&deps.api.addr_validate(&address)?, stage.into());
    let is_claimed = CLAIM.load(deps.storage, key)?;
    let resp = IsClaimedResponse { is_claimed };

    Ok(resp)
}

#[entry_point]
pub fn migrate(_deps: DepsMut, _env: Env, _msg: MigrateMsg) -> StdResult<Response> {
    Ok(Response::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmwasm_std::testing::{mock_dependencies, mock_env, mock_info};
    use cosmwasm_std::{from_binary, CosmosMsg, SubMsg};

    #[test]
    fn proper_instantiation() {
        let mut deps = mock_dependencies(&[]);

        let msg = InstantiateMsg {
            owner: Some("owner0000".to_string()),
            cw20_token_address: "anchor0000".to_string(),
        };

        let env = mock_env();
        let info = mock_info("addr0000", &[]);

        // we can just call .unwrap() to assert this was a success
        let _res = instantiate(deps.as_mut(), env.clone(), info, msg).unwrap();

        // it worked, let's query the state
        let res = query(deps.as_ref(), env.clone(), QueryMsg::Config {}).unwrap();
        let config: ConfigResponse = from_binary(&res).unwrap();
        assert_eq!("owner0000", config.owner.unwrap().as_str());
        assert_eq!("anchor0000", config.cw20_token_address.as_str());

        let res = query(deps.as_ref(), env, QueryMsg::LatestStage {}).unwrap();
        let latest_stage: LatestStageResponse = from_binary(&res).unwrap();
        assert_eq!(0u8, latest_stage.latest_stage);
    }

    #[test]
    fn update_config() {
        let mut deps = mock_dependencies(&[]);

        let msg = InstantiateMsg {
            owner: None,
            cw20_token_address: "anchor0000".to_string(),
        };

        let env = mock_env();
        let info = mock_info("owner0000", &[]);
        let _res = instantiate(deps.as_mut(), env, info, msg).unwrap();

        // update owner
        let env = mock_env();
        let info = mock_info("owner0000", &[]);
        let msg = ExecuteMsg::UpdateConfig {
            new_owner: Some("owner0001".to_string()),
        };

        let res = execute(deps.as_mut(), env.clone(), info, msg).unwrap();
        assert_eq!(0, res.messages.len());

        // it worked, let's query the state
        let res = query(deps.as_ref(), env, QueryMsg::Config {}).unwrap();
        let config: ConfigResponse = from_binary(&res).unwrap();
        assert_eq!("owner0001", config.owner.unwrap().as_str());

        // Unauthorized err
        let env = mock_env();
        let info = mock_info("owner0000", &[]);
        let msg = ExecuteMsg::UpdateConfig { new_owner: None };

        let res = execute(deps.as_mut(), env, info, msg);
        match res {
            Err(ContractError::Unauthorized { .. }) => {}
            _ => panic!("Must return unauthorized error"),
        }
    }

    #[test]
    fn register_merkle_root() {
        let mut deps = mock_dependencies(&[]);

        let msg = InstantiateMsg {
            owner: Some("owner0000".to_string()),
            cw20_token_address: "anchor0000".to_string(),
        };

        let env = mock_env();
        let info = mock_info("addr0000", &[]);
        let _res = instantiate(deps.as_mut(), env, info, msg).unwrap();

        // register new merkle root
        let env = mock_env();
        let info = mock_info("owner0000", &[]);
        let msg = ExecuteMsg::RegisterMerkleRoot {
            merkle_root: "634de21cde1044f41d90373733b0f0fb1c1c71f9652b905cdf159e73c4cf0d37"
                .to_string(),
        };

        let res = execute(deps.as_mut(), env.clone(), info, msg).unwrap();
        assert_eq!(
            res.attributes,
            vec![
                attr("action", "register_merkle_root"),
                attr("stage", "1"),
                attr(
                    "merkle_root",
                    "634de21cde1044f41d90373733b0f0fb1c1c71f9652b905cdf159e73c4cf0d37"
                )
            ]
        );

        let res = query(deps.as_ref(), env.clone(), QueryMsg::LatestStage {}).unwrap();
        let latest_stage: LatestStageResponse = from_binary(&res).unwrap();
        assert_eq!(1u8, latest_stage.latest_stage);

        let res = query(
            deps.as_ref(),
            env,
            QueryMsg::MerkleRoot {
                stage: latest_stage.latest_stage,
            },
        )
        .unwrap();
        let merkle_root: MerkleRootResponse = from_binary(&res).unwrap();
        assert_eq!(
            "634de21cde1044f41d90373733b0f0fb1c1c71f9652b905cdf159e73c4cf0d37".to_string(),
            merkle_root.merkle_root
        );
    }

    #[test]
    fn claim() {
        let mut deps = mock_dependencies(&[]);

        let msg = InstantiateMsg {
            owner: Some("owner0000".to_string()),
            cw20_token_address: "token0000".to_string(),
        };

        let env = mock_env();
        let info = mock_info("addr0000", &[]);
        let _res = instantiate(deps.as_mut(), env, info, msg).unwrap();

        let env = mock_env();
        let info = mock_info("owner0000", &[]);
        let msg = ExecuteMsg::RegisterMerkleRoot {
            merkle_root: "5d4f48f147cb6cb742b376dce5626b2a036f69faec10cd73631c791780e150fc"
                .to_string(),
        };
        let _res = execute(deps.as_mut(), env, info, msg).unwrap();

        let msg = ExecuteMsg::Claim {
            amount: Uint128::from(10220u128),
            stage: 1u8,
            proof: vec![
                "561b09601236f43b0065d1ea601df754989911ed6da16ba8afed62c458f86352".to_string(),
                "280777995d054081cbf208bccb70f8d736c1766b81d90a1fd21cd97d2d83a5cc".to_string(),
                "3946ea1758a5a2bf55bae1186168ad35aa0329805bc8bff1ca3d51345faec04a".to_string(),
            ],
        };

        let env = mock_env();
        let info = mock_info("wasm1qzy8rg0f406uvvl54dlww6ptlh30303xq2u3xu", &[]);
        let res = execute(deps.as_mut(), env.clone(), info.clone(), msg.clone()).unwrap();
        let expected = SubMsg::new(CosmosMsg::Wasm(WasmMsg::Execute {
            contract_addr: "token0000".to_string(),
            funds: vec![],
            msg: to_binary(&Cw20ExecuteMsg::Transfer {
                recipient: "wasm1qzy8rg0f406uvvl54dlww6ptlh30303xq2u3xu".into(),
                amount: Uint128::from(10220u128),
            })
            .unwrap(),
        }));
        assert_eq!(res.messages, vec![expected]);

        assert_eq!(
            res.attributes,
            vec![
                attr("action", "claim"),
                attr("stage", "1"),
                attr("address", "wasm1qzy8rg0f406uvvl54dlww6ptlh30303xq2u3xu"),
                attr("amount", "10220")
            ]
        );

        /*
        TODO: enable after 0.16 release https://github.com/CosmWasm/cosmwasm/pull/989/files
        assert!(
            from_binary::<IsClaimedResponse>(
                &query(
                    deps.as_ref(),
                    env.clone(),
                    QueryMsg::IsClaimed {
                        stage: 1,
                        address: "wasm1qzy8rg0f406uvvl54dlww6ptlh30303xq2u3xu".to_string(),
                    }
                )
                .unwrap()
            )
            .unwrap()
            .is_claimed
        );
         */
        // check claimed
        let res = execute(deps.as_mut(), env, info, msg);
        match res {
            Err(ContractError::Claimed {}) => {}
            _ => panic!("DO NOT ENTER HERE"),
        }

        // register new drop
        let env = mock_env();
        let info = mock_info("owner0000", &[]);
        let msg = ExecuteMsg::RegisterMerkleRoot {
            merkle_root: "ebaa83c7eaf7467c378d2f37b5e46752d904d2d17acd380b24b02e3b398b3e5a"
                .to_string(),
        };
        let _res = execute(deps.as_mut(), env, info, msg).unwrap();

        // Claim next airdrop
        let msg = ExecuteMsg::Claim {
            amount: Uint128::from(1000000000u128),
            stage: 2u8,
            proof: vec![
                "60493754478e7886afa7fad8f059067244853c4170452d86876ab743059ea6c1".to_string(),
                "29c9253bb0baf21e54e4b2552703554f5c0d67288621549148a9e1e90aaf3bf8".to_string(),
                "807a97e1ac2196309938e8c12940f8c259172eb7687dbb5e04e7b88c45e5a5a6".to_string(),
                "c339fc3f7995f019f5b4b515875e17c00d7146e998b371804cfdfd98f1ae86ea".to_string(),
            ],
        };

        let env = mock_env();
        let info = mock_info("wasm1ylna88nach9sn5n7qe7u5l6lh7dmt6lp2y63xx", &[]);
        let res = execute(deps.as_mut(), env, info, msg).unwrap();
        let expected: SubMsg<_> = SubMsg::new(CosmosMsg::Wasm(WasmMsg::Execute {
            contract_addr: "token0000".to_string(),
            funds: vec![],
            msg: to_binary(&Cw20ExecuteMsg::Transfer {
                recipient: "wasm1ylna88nach9sn5n7qe7u5l6lh7dmt6lp2y63xx".into(),
                amount: Uint128::new(1000000000u128),
            })
            .unwrap(),
        }));
        assert_eq!(res.messages, vec![expected]);

        assert_eq!(
            res.attributes,
            vec![
                attr("action", "claim"),
                attr("stage", "2"),
                attr("address", "wasm1ylna88nach9sn5n7qe7u5l6lh7dmt6lp2y63xx"),
                attr("amount", "1000000000")
            ]
        );
    }

    #[test]
    fn owner_freeze() {
        let mut deps = mock_dependencies(&[]);

        let msg = InstantiateMsg {
            owner: Some("owner0000".to_string()),
            cw20_token_address: "token0000".to_string(),
        };

        let env = mock_env();
        let info = mock_info("addr0000", &[]);
        let _res = instantiate(deps.as_mut(), env, info, msg).unwrap();

        // can register merkle root
        let env = mock_env();
        let info = mock_info("owner0000", &[]);
        let msg = ExecuteMsg::RegisterMerkleRoot {
            merkle_root: "5d4f48f147cb6cb742b376dce5626b2a036f69faec10cd73631c791780e150fc"
                .to_string(),
        };
        let _res = execute(deps.as_mut(), env, info, msg).unwrap();

        // can update owner
        let env = mock_env();
        let info = mock_info("owner0000", &[]);
        let msg = ExecuteMsg::UpdateConfig {
            new_owner: Some("owner0001".to_string()),
        };

        let res = execute(deps.as_mut(), env, info, msg).unwrap();
        assert_eq!(0, res.messages.len());

        // freeze contract
        let env = mock_env();
        let info = mock_info("owner0001", &[]);
        let msg = ExecuteMsg::UpdateConfig { new_owner: None };

        let res = execute(deps.as_mut(), env, info, msg).unwrap();
        assert_eq!(0, res.messages.len());

        // cannot register new drop
        let env = mock_env();
        let info = mock_info("owner0001", &[]);
        let msg = ExecuteMsg::RegisterMerkleRoot {
            merkle_root: "ebaa83c7eaf7467c378d2f37b5e46752d904d2d17acd380b24b02e3b398b3e5a"
                .to_string(),
        };
        match execute(deps.as_mut(), env, info, msg) {
            Err(ContractError::Unauthorized { .. }) => {}
            _ => panic!("Must return unauthorized error"),
        }

        // cannot update config
        let env = mock_env();
        let info = mock_info("owner0001", &[]);
        let msg = ExecuteMsg::RegisterMerkleRoot {
            merkle_root: "ebaa83c7eaf7467c378d2f37b5e46752d904d2d17acd380b24b02e3b398b3e5a"
                .to_string(),
        };
        match execute(deps.as_mut(), env, info, msg) {
            Err(ContractError::Unauthorized { .. }) => {}
            _ => panic!("Must return unauthorized error"),
        }
    }
}
