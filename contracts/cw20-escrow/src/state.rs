use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use cosmwasm_std::{
    Api, CanonicalAddr, Env, HumanAddr, Order, ReadonlyStorage, StdError, StdResult, Storage,
};
use cosmwasm_storage::{bucket, bucket_read, prefixed_read, Bucket, ReadonlyBucket};

use cw0::NativeBalance;
use cw20::Cw20Coin;

pub(crate) type EscrowBalance = (NativeBalance, Vec<Cw20Coin>);

#[derive(Serialize, Deserialize, Clone, PartialEq, JsonSchema, Debug, Default)]
pub struct Escrow {
    /// arbiter can decide to approve or refund the escrow
    pub arbiter: CanonicalAddr,
    /// if approved, funds go to the recipient
    pub recipient: CanonicalAddr,
    /// if refunded, funds go to the source
    pub source: CanonicalAddr,
    /// When end height set and block height exceeds this value, the escrow is expired.
    /// Once an escrow is expired, it can be returned to the original funder (via "refund").
    pub end_height: Option<u64>,
    /// When end time (in seconds since epoch 00:00:00 UTC on 1 January 1970) is set and
    /// block time exceeds this value, the escrow is expired.
    /// Once an escrow is expired, it can be returned to the original funder (via "refund").
    pub end_time: Option<u64>,
    /// Balance in Native tokens and Cw20 coins
    pub balance: EscrowBalance,
    /// All possible contracts that we accept tokens from
    pub cw20_whitelist: Vec<CanonicalAddr>,
}

impl Escrow {
    pub fn is_expired(&self, env: &Env) -> bool {
        if let Some(end_height) = self.end_height {
            if env.block.height > end_height {
                return true;
            }
        }

        if let Some(end_time) = self.end_time {
            if env.block.time > end_time {
                return true;
            }
        }

        false
    }

    pub fn human_whitelist<A: Api>(&self, api: &A) -> StdResult<Vec<HumanAddr>> {
        self.cw20_whitelist
            .iter()
            .map(|h| api.human_address(h))
            .collect()
    }
}

pub const PREFIX_ESCROW: &[u8] = b"escrow";

pub fn escrows<S: Storage>(storage: &mut S) -> Bucket<S, Escrow> {
    bucket(PREFIX_ESCROW, storage)
}

pub fn escrows_read<S: ReadonlyStorage>(storage: &S) -> ReadonlyBucket<S, Escrow> {
    bucket_read(PREFIX_ESCROW, storage)
}

/// This returns the list of ids for all registered escrows
pub fn all_escrow_ids<S: ReadonlyStorage>(storage: &S) -> StdResult<Vec<String>> {
    prefixed_read(PREFIX_ESCROW, storage)
        .range(None, None, Order::Ascending)
        .map(|(k, _)| {
            String::from_utf8(k).map_err(|_| StdError::invalid_utf8("parsing escrow key"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use cosmwasm_std::testing::MockStorage;
    use cosmwasm_std::Binary;

    #[test]
    fn no_escrow_ids() {
        let storage = MockStorage::new();
        let ids = all_escrow_ids(&storage).unwrap();
        assert_eq!(0, ids.len());
    }

    fn dummy_escrow() -> Escrow {
        Escrow {
            arbiter: CanonicalAddr(Binary(b"arb".to_vec())),
            recipient: CanonicalAddr(Binary(b"recip".to_vec())),
            source: CanonicalAddr(Binary(b"source".to_vec())),
            ..Escrow::default()
        }
    }

    #[test]
    fn all_escrow_ids_in_order() {
        let mut storage = MockStorage::new();
        escrows(&mut storage)
            .save("lazy".as_bytes(), &dummy_escrow())
            .unwrap();
        escrows(&mut storage)
            .save("assign".as_bytes(), &dummy_escrow())
            .unwrap();
        escrows(&mut storage)
            .save("zen".as_bytes(), &dummy_escrow())
            .unwrap();

        let ids = all_escrow_ids(&storage).unwrap();
        assert_eq!(3, ids.len());
        assert_eq!(
            vec!["assign".to_string(), "lazy".to_string(), "zen".to_string()],
            ids
        )
    }
}
