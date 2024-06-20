use std::{
    error::Error,
    io::{Cursor, Read},
};

use bytes::{BufMut, BytesMut};
use num_bigint::BigInt;
use num_traits::{ToPrimitive, Zero};
use tokio_postgres::types::{to_sql_checked, FromSql, IsNull, ToSql, Type};

pub fn write_big_uint_to_pg_numeric_bytes(num: BigInt, out: &mut BytesMut) {
    let mut digits = vec![];

    let mut n = num.clone();
    let base_bigint = BigInt::from(10000);
    while !n.is_zero() {
        let remainder = (&n % &base_bigint).to_i16().unwrap();
        n /= &base_bigint;
        digits.push(remainder);
    }
    digits.reverse();

    let num_digits = digits.len();
    out.reserve(8 + num_digits * 2);
    out.put_u16(num_digits.try_into().unwrap());
    out.put_i16((num_digits - 1) as i16);
    out.put_u16(0x0000); // Always positive
    out.put_u16(0x0000); // No decimals
    for digit in digits[0..num_digits].iter() {
        out.put_i16(*digit);
    }
}

fn read_two_bytes(cursor: &mut Cursor<&[u8]>) -> std::io::Result<[u8; 2]> {
    let mut result = [0; 2];
    cursor.read_exact(&mut result)?;
    Ok(result)
}

// TODO: write tests for this
pub fn big_uint_from_pg_numeric_bytes(raw: &[u8]) -> BigInt {
    let mut raw = Cursor::new(raw);
    let num_groups = u16::from_be_bytes(read_two_bytes(&mut raw).unwrap());
    let weight = i16::from_be_bytes(read_two_bytes(&mut raw).unwrap());
    let _sign = u16::from_be_bytes(read_two_bytes(&mut raw).unwrap()); // Unused for uint
    let _scale = u16::from_be_bytes(read_two_bytes(&mut raw).unwrap()); // Unused for uint

    let mut groups = Vec::new();
    for _ in 0..num_groups as usize {
        groups.push(u16::from_be_bytes(read_two_bytes(&mut raw).unwrap()));
    }

    let mut digits = groups.into_iter().collect::<Vec<_>>();
    let integers_part_count = weight as i32 + 1;

    let mut result = BigInt::ZERO;
    if integers_part_count > 0 {
        let (start_integers, last) = if integers_part_count > digits.len() as i32 {
            (
                integers_part_count - digits.len() as i32,
                digits.len() as i32,
            )
        } else {
            (0, integers_part_count)
        };
        let integers: Vec<_> = digits.drain(..last as usize).collect();
        for digit in integers {
            result = result.checked_mul(&BigInt::from(10000)).unwrap();
            result = result.checked_add(&BigInt::from(digit)).unwrap();
        }
        result = result
            .checked_mul(&BigInt::from(10000).pow(4 * start_integers as u32))
            .unwrap();
    }
    result
}

#[derive(Debug, Clone, Copy)]
pub struct PgNumericU128(pub u128);

impl ToSql for PgNumericU128 {
    fn to_sql(
        &self,
        _ty: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn Error + Sync + Send>> {
        let num =
            BigInt::parse_bytes(&self.0.to_string().as_bytes(), 10).expect("Invalid number string");
        write_big_uint_to_pg_numeric_bytes(num, out);
        Ok(IsNull::No)
    }

    fn accepts(ty: &Type) -> bool {
        ty.name() == "numeric"
    }

    to_sql_checked!();
}

impl<'a> FromSql<'a> for PgNumericU128 {
    fn from_sql(_ty: &Type, raw: &'a [u8]) -> Result<PgNumericU128, Box<dyn Error + Sync + Send>> {
        let result = big_uint_from_pg_numeric_bytes(raw);
        Ok(PgNumericU128(result.to_u128().unwrap()))
    }

    fn accepts(ty: &Type) -> bool {
        ty.name() == "numeric"
    }
}
