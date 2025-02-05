use bytes::{BufMut, BytesMut};
use tokio_postgres::types::{to_sql_checked, FromSql, IsNull, ToSql, Type};

use std::{
    error::Error,
    ops::{AddAssign, SubAssign},
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PgTextU128(pub u128);

impl ToSql for PgTextU128 {
    fn to_sql(
        &self,
        _ty: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn Error + Sync + Send>> {
        let s = self.0.to_string();
        out.put_slice(s.as_bytes());
        Ok(IsNull::No)
    }

    fn accepts(ty: &Type) -> bool {
        ty.name() == "text"
    }

    to_sql_checked!();
}

impl<'a> FromSql<'a> for PgTextU128 {
    fn from_sql(_ty: &Type, raw: &'a [u8]) -> Result<PgTextU128, Box<dyn Error + Sync + Send>> {
        let s = std::str::from_utf8(raw)?;
        let value = s.parse::<u128>()?;
        Ok(PgTextU128(value))
    }

    fn accepts(ty: &Type) -> bool {
        ty.name() == "text"
    }
}

impl AddAssign for PgTextU128 {
    fn add_assign(&mut self, other: Self) {
        self.0 += other.0;
    }
}

impl AddAssign<u128> for PgTextU128 {
    fn add_assign(&mut self, other: u128) {
        self.0 += other;
    }
}

impl SubAssign for PgTextU128 {
    fn sub_assign(&mut self, other: Self) {
        self.0 -= other.0;
    }
}

impl SubAssign<u128> for PgTextU128 {
    fn sub_assign(&mut self, other: u128) {
        self.0 -= other;
    }
}

#[cfg(test)]
mod test {
    use chainhook_sdk::utils::Context;
    use test_case::test_case;

    use crate::db::pg_test_client;

    use super::PgTextU128;

    #[test_case(340282366920938463463374607431768211455; "u128 max")]
    #[test_case(80000000000000000; "with trailing zeros")]
    #[test_case(0; "zero")]
    #[tokio::test]
    async fn test_u128_to_postgres(val: u128) {
        let mut client = pg_test_client(false, &Context::empty()).await;
        let value = PgTextU128(val);
        let tx = client.transaction().await.unwrap();
        let _ = tx.query("CREATE TABLE test (value TEXT)", &[]).await;
        let _ = tx
            .query("INSERT INTO test (value) VALUES ($1)", &[&value])
            .await;
        let rows = tx.query_one("SELECT value FROM test", &[]).await.unwrap();
        let res: PgTextU128 = rows.get("value");
        let _ = tx.rollback().await;
        assert_eq!(res.0, value.0);
    }
}
