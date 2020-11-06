use cosmwasm_std::{
    coin, from_slice, to_binary, to_vec, AllBalanceResponse, BalanceResponse, BankMsg, BankQuery,
    Binary, Coin, HumanAddr, Storage,
};

//*** TODO: remove this and import cw0::balance when we are both on 0.12 ***/
use crate::balance::NativeBalance;
use crate::transactions::{RepLog, StorageTransaction};

/// Bank is a minimal contract-like interface that implements a bank module
/// It is initialized outside of the trait
pub trait Bank {
    fn handle(
        &self,
        storage: &mut dyn Storage,
        sender: HumanAddr,
        msg: BankMsg,
    ) -> Result<(), String>;

    fn query(&self, storage: &dyn Storage, request: BankQuery) -> Result<Binary, String>;

    // this is an "admin" function to let us adjust bank accounts
    fn set_balance(
        &self,
        storage: &mut dyn Storage,
        account: HumanAddr,
        amount: Vec<Coin>,
    ) -> Result<(), String>;

    fn clone(&self) -> Box<dyn Bank>;
}

pub struct BankRouter {
    bank: Box<dyn Bank>,
    storage: Box<dyn Storage>,
}

impl BankRouter {
    pub fn new<B: Bank + 'static>(bank: B, storage: Box<dyn Storage>) -> Self {
        BankRouter {
            bank: Box::new(bank),
            storage,
        }
    }

    // this is an "admin" function to let us adjust bank accounts
    pub fn set_balance(&mut self, account: HumanAddr, amount: Vec<Coin>) -> Result<(), String> {
        self.bank
            .set_balance(self.storage.as_mut(), account, amount)
    }

    pub fn cache(&'_ self) -> BankCache<'_> {
        BankCache::new(self)
    }

    pub fn query(&self, request: BankQuery) -> Result<Binary, String> {
        self.bank.query(self.storage.as_ref(), request)
    }
}

pub struct BankCache<'a> {
    // and this into one with reference
    router: &'a BankRouter,
    state: StorageTransaction<'a>,
}

pub struct BankOps(RepLog);

impl BankOps {
    pub fn commit(self, router: &mut BankRouter) {
        self.0.commit(router.storage.as_mut())
    }
}

impl<'a> BankCache<'a> {
    fn new(router: &'a BankRouter) -> Self {
        BankCache {
            router,
            state: StorageTransaction::new(router.storage.as_ref()),
        }
    }

    pub fn prepare(self) -> BankOps {
        BankOps(self.state.prepare())
    }

    pub fn execute(&mut self, sender: HumanAddr, msg: BankMsg) -> Result<(), String> {
        self.router.bank.handle(&mut self.state, sender, msg)
    }
}

#[derive(Default)]
pub struct SimpleBank {}

impl SimpleBank {
    // this is an "admin" function to let us adjust bank accounts
    pub fn get_balance(
        &self,
        storage: &dyn Storage,
        account: HumanAddr,
    ) -> Result<Vec<Coin>, String> {
        let raw = storage.get(account.as_bytes());
        match raw {
            Some(data) => {
                let balance: NativeBalance = from_slice(&data).map_err(|e| e.to_string())?;
                Ok(balance.into_vec())
            }
            None => Ok(vec![]),
        }
    }

    fn send(
        &self,
        storage: &mut dyn Storage,
        from_address: HumanAddr,
        to_address: HumanAddr,
        amount: Vec<Coin>,
    ) -> Result<(), String> {
        let a = self.get_balance(storage, from_address.clone())?;
        let a = (NativeBalance(a) - amount.clone()).map_err(|e| e.to_string())?;
        self.set_balance(storage, from_address, a.into_vec())?;

        let b = self.get_balance(storage, to_address.clone())?;
        let b = NativeBalance(b) + NativeBalance(amount);
        self.set_balance(storage, to_address, b.into_vec())?;

        Ok(())
    }
}

// TODO: use storage-plus when that is on 0.12.. for now just do this by hand
impl Bank for SimpleBank {
    fn handle(
        &self,
        storage: &mut dyn Storage,
        sender: HumanAddr,
        msg: BankMsg,
    ) -> Result<(), String> {
        match msg {
            BankMsg::Send {
                from_address,
                to_address,
                amount,
            } => {
                if sender != from_address {
                    Err("Sender must equal from_address".into())
                } else {
                    self.send(storage, from_address, to_address, amount)
                }
            }
        }
    }

    fn query(&self, storage: &dyn Storage, request: BankQuery) -> Result<Binary, String> {
        match request {
            BankQuery::AllBalances { address } => {
                let amount = self.get_balance(storage, address)?;
                let res = AllBalanceResponse { amount };
                Ok(to_binary(&res).map_err(|e| e.to_string())?)
            }
            BankQuery::Balance { address, denom } => {
                let all_amounts = self.get_balance(storage, address)?;
                let amount = all_amounts
                    .into_iter()
                    .find(|c| c.denom == denom)
                    .unwrap_or_else(|| coin(0, denom));
                let res = BalanceResponse { amount };
                Ok(to_binary(&res).map_err(|e| e.to_string())?)
            }
        }
    }

    // this is an "admin" function to let us adjust bank accounts
    fn set_balance(
        &self,
        storage: &mut dyn Storage,
        account: HumanAddr,
        amount: Vec<Coin>,
    ) -> Result<(), String> {
        let mut balance = NativeBalance(amount);
        balance.normalize();
        let key = account.as_bytes();
        let value = to_vec(&balance).map_err(|e| e.to_string())?;
        storage.set(key, &value);
        Ok(())
    }

    fn clone(&self) -> Box<dyn Bank> {
        Box::new(SimpleBank {})
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use cosmwasm_std::coins;
    use cosmwasm_std::testing::MockStorage;

    #[test]
    fn get_set_balance() {
        let mut store = MockStorage::new();

        let owner = HumanAddr::from("owner");
        let rcpt = HumanAddr::from("receiver");
        let init_funds = vec![coin(100, "eth"), coin(20, "btc")];
        let norm = vec![coin(20, "btc"), coin(100, "eth")];

        // set money
        let bank = SimpleBank {};
        bank.set_balance(&mut store, owner.clone(), init_funds)
            .unwrap();

        // get balance work
        let rich = bank.get_balance(&store, owner.clone()).unwrap();
        assert_eq!(rich, norm);
        let poor = bank.get_balance(&store, rcpt.clone()).unwrap();
        assert_eq!(poor, vec![]);

        // proper queries work
        let req = BankQuery::AllBalances {
            address: owner.clone(),
        };
        let raw = bank.query(&store, req).unwrap();
        let res: AllBalanceResponse = from_slice(&raw).unwrap();
        assert_eq!(res.amount, norm);

        let req = BankQuery::AllBalances {
            address: rcpt.clone(),
        };
        let raw = bank.query(&store, req).unwrap();
        let res: AllBalanceResponse = from_slice(&raw).unwrap();
        assert_eq!(res.amount, vec![]);

        let req = BankQuery::Balance {
            address: owner.clone(),
            denom: "eth".into(),
        };
        let raw = bank.query(&store, req).unwrap();
        let res: BalanceResponse = from_slice(&raw).unwrap();
        assert_eq!(res.amount, coin(100, "eth"));

        let req = BankQuery::Balance {
            address: owner.clone(),
            denom: "foobar".into(),
        };
        let raw = bank.query(&store, req).unwrap();
        let res: BalanceResponse = from_slice(&raw).unwrap();
        assert_eq!(res.amount, coin(0, "foobar"));

        let req = BankQuery::Balance {
            address: rcpt.clone(),
            denom: "eth".into(),
        };
        let raw = bank.query(&store, req).unwrap();
        let res: BalanceResponse = from_slice(&raw).unwrap();
        assert_eq!(res.amount, coin(0, "eth"));
    }

    #[test]
    fn send_coins() {
        let mut store = MockStorage::new();

        let owner = HumanAddr::from("owner");
        let rcpt = HumanAddr::from("receiver");
        let init_funds = vec![coin(20, "btc"), coin(100, "eth")];
        let rcpt_funds = vec![coin(5, "btc")];

        // set money
        let bank = SimpleBank {};
        bank.set_balance(&mut store, owner.clone(), init_funds.clone())
            .unwrap();
        bank.set_balance(&mut store, rcpt.clone(), rcpt_funds.clone())
            .unwrap();

        // send both tokens
        let to_send = vec![coin(30, "eth"), coin(5, "btc")];
        let msg = BankMsg::Send {
            from_address: owner.clone(),
            to_address: rcpt.clone(),
            amount: to_send.clone(),
        };
        bank.handle(&mut store, owner.clone(), msg.clone()).unwrap();
        let rich = bank.get_balance(&store, owner.clone()).unwrap();
        assert_eq!(vec![coin(15, "btc"), coin(70, "eth")], rich);
        let poor = bank.get_balance(&store, rcpt.clone()).unwrap();
        assert_eq!(vec![coin(10, "btc"), coin(30, "eth")], poor);

        // cannot send from other account
        bank.handle(&mut store, rcpt.clone(), msg).unwrap_err();

        // cannot send too much
        let msg = BankMsg::Send {
            from_address: owner.clone(),
            to_address: rcpt.clone(),
            amount: coins(20, "btc"),
        };
        bank.handle(&mut store, owner.clone(), msg.clone())
            .unwrap_err();

        let rich = bank.get_balance(&store, owner.clone()).unwrap();
        assert_eq!(vec![coin(15, "btc"), coin(70, "eth")], rich);
    }
}
