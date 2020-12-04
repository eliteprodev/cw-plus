#![cfg(test)]

use cosmwasm_std::testing::{mock_env, MockApi, MockStorage};
use cosmwasm_std::{coins, to_binary, HumanAddr, Uint128};
use cw20::{Cw20CoinHuman, Cw20Contract, Cw20HandleMsg};
use cw_multi_test::{App, Contract, ContractWrapper, SimpleBank};

use crate::msg::{CreateMsg, DetailsResponse, HandleMsg, InitMsg, QueryMsg, ReceiveMsg};

fn mock_app() -> App {
    let env = mock_env();
    let api = Box::new(MockApi::default());
    let bank = SimpleBank {};

    App::new(api, env.block, bank, || Box::new(MockStorage::new()))
}

pub fn contract_escrow() -> Box<dyn Contract> {
    let contract = ContractWrapper::new(
        crate::contract::handle,
        crate::contract::init,
        crate::contract::query,
    );
    Box::new(contract)
}

pub fn contract_cw20() -> Box<dyn Contract> {
    let contract = ContractWrapper::new(
        cw20_base::contract::handle,
        cw20_base::contract::init,
        cw20_base::contract::query,
    );
    Box::new(contract)
}

#[test]
// receive cw20 tokens and release upon approval
fn escrow_happy_path_cw20_tokens() {
    let mut router = mock_app();

    // set personal balance
    let owner = HumanAddr::from("owner");
    let init_funds = coins(2000, "btc");
    router
        .set_bank_balance(owner.clone(), init_funds.clone())
        .unwrap();

    // set up cw20 contract with some tokens
    let cw20_id = router.store_code(contract_cw20());
    let msg = cw20_base::msg::InitMsg {
        name: "Cash Money".to_string(),
        symbol: "CASH".to_string(),
        decimals: 2,
        initial_balances: vec![Cw20CoinHuman {
            address: owner.clone(),
            amount: Uint128(5000),
        }],
        mint: None,
    };
    let cash_addr = router
        .instantiate_contract(cw20_id, &owner, &msg, &[], "CASH")
        .unwrap();

    // set up reflect contract
    let escrow_id = router.store_code(contract_escrow());
    let escrow_addr = router
        .instantiate_contract(escrow_id, &owner, &InitMsg {}, &[], "Escrow")
        .unwrap();

    // they are different
    assert_ne!(cash_addr, escrow_addr);

    // set up cw20 helpers
    let cash = Cw20Contract(cash_addr.clone());

    // ensure our balances
    let owner_balance = cash.balance(&router, owner.clone()).unwrap();
    assert_eq!(owner_balance, Uint128(5000));
    let escrow_balance = cash.balance(&router, escrow_addr.clone()).unwrap();
    assert_eq!(escrow_balance, Uint128(0));

    // send some tokens to create an escrow
    let arb = HumanAddr::from("arbiter");
    let ben = HumanAddr::from("beneficiary");
    let id = "demo".to_string();
    let create_msg = ReceiveMsg::Create(CreateMsg {
        id: id.clone(),
        arbiter: arb.clone(),
        recipient: ben.clone(),
        end_height: None,
        end_time: None,
        cw20_whitelist: None,
    });
    let create_bin = to_binary(&create_msg).unwrap();
    let send_msg = Cw20HandleMsg::Send {
        contract: escrow_addr.clone(),
        amount: Uint128(1200),
        msg: Some(create_bin),
    };
    let res = router
        .execute_contract(&owner, &cash_addr, &send_msg, &[])
        .unwrap();
    println!("{:?}", res.attributes);
    assert_eq!(6, res.attributes.len());

    // ensure balances updated
    let owner_balance = cash.balance(&router, owner.clone()).unwrap();
    assert_eq!(owner_balance, Uint128(3800));
    let escrow_balance = cash.balance(&router, escrow_addr.clone()).unwrap();
    assert_eq!(escrow_balance, Uint128(1200));

    // ensure escrow properly created
    let details: DetailsResponse = router
        .wrap()
        .query_wasm_smart(&escrow_addr, &QueryMsg::Details { id: id.clone() })
        .unwrap();
    assert_eq!(id, details.id);
    assert_eq!(arb, details.arbiter);
    assert_eq!(ben, details.recipient);
    assert_eq!(
        vec![Cw20CoinHuman {
            address: cash_addr.clone(),
            amount: Uint128(1200)
        }],
        details.cw20_balance
    );

    // release escrow
    let approve_msg = HandleMsg::Approve { id: id.clone() };
    let _ = router
        .execute_contract(&arb, &escrow_addr, &approve_msg, &[])
        .unwrap();

    // ensure balances updated - release to ben
    let owner_balance = cash.balance(&router, owner.clone()).unwrap();
    assert_eq!(owner_balance, Uint128(3800));
    let escrow_balance = cash.balance(&router, escrow_addr.clone()).unwrap();
    assert_eq!(escrow_balance, Uint128(0));
    let ben_balance = cash.balance(&router, ben.clone()).unwrap();
    assert_eq!(ben_balance, Uint128(1200));
}
