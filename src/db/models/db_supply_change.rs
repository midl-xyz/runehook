use crate::db::types::{pg_numeric_u64::PgNumericU64, pg_text_u128::PgTextU128};

/// An update to a rune that affects its total counts.
#[derive(Debug, Clone)]
pub struct DbSupplyChange {
    pub rune_id: String,
    pub block_height: PgNumericU64,
    pub minted: PgTextU128,
    pub total_mints: PgTextU128,
    pub burned: PgTextU128,
    pub total_burns: PgTextU128,
    pub total_operations: PgTextU128,
}

impl DbSupplyChange {
    pub fn from_mint(id: String, block_height: PgNumericU64, amount: PgTextU128) -> Self {
        DbSupplyChange {
            rune_id: id,
            block_height,
            minted: amount,
            total_mints: PgTextU128(1),
            burned: PgTextU128(0),
            total_burns: PgTextU128(0),
            total_operations: PgTextU128(1),
        }
    }

    pub fn from_burn(id: String, block_height: PgNumericU64, amount: PgTextU128) -> Self {
        DbSupplyChange {
            rune_id: id,
            block_height,
            minted: PgTextU128(0),
            total_mints: PgTextU128(0),
            burned: amount,
            total_burns: PgTextU128(1),
            total_operations: PgTextU128(1),
        }
    }

    pub fn from_operation(id: String, block_height: PgNumericU64) -> Self {
        DbSupplyChange {
            rune_id: id,
            block_height,
            minted: PgTextU128(0),
            total_mints: PgTextU128(0),
            burned: PgTextU128(0),
            total_burns: PgTextU128(0),
            total_operations: PgTextU128(1),
        }
    }
}
