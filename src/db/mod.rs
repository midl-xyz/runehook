use std::{collections::HashMap, process, str::FromStr};

use cache::input_rune_balance::InputRuneBalance;
use chainhook_sdk::utils::Context;
use models::{
    db_balance_change::DbBalanceChange, db_ledger_entry::DbLedgerEntry, db_rune::DbRune,
    db_supply_change::DbSupplyChange,
};
use ordinals::RuneId;
use refinery::embed_migrations;
use tokio_postgres::{types::ToSql, Client, Error, GenericClient, NoTls, Transaction};
use types::{pg_bigint_u32::PgBigIntU32, pg_numeric_u64::PgNumericU64, pg_text_u128::PgTextU128};

use crate::{config::Config, try_error, try_info};

pub mod cache;
pub mod index;
pub mod models;
pub mod types;

embed_migrations!("migrations");

async fn pg_run_migrations(pg_client: &mut Client, ctx: &Context) {
    try_info!(ctx, "Running postgres migrations");
    match migrations::runner()
        .set_migration_table_name("pgmigrations")
        .run_async(pg_client)
        .await
    {
        Ok(_) => {}
        Err(e) => {
            try_error!(ctx, "Error running pg migrations: {}", e.to_string());
            process::exit(1);
        }
    };
    try_info!(ctx, "Postgres migrations complete");
}

pub async fn pg_connect(config: &Config, run_migrations: bool, ctx: &Context) -> Client {
    let mut pg_config = tokio_postgres::Config::new();
    pg_config
        .dbname(&config.postgres.database)
        .host(&config.postgres.host)
        .port(config.postgres.port)
        .user(&config.postgres.username);
    if let Some(password) = config.postgres.password.as_ref() {
        pg_config.password(password);
    }

    try_info!(
        ctx,
        "Connecting to postgres at {}:{}",
        config.postgres.host,
        config.postgres.port
    );
    let mut pg_client: Client;
    loop {
        match pg_config.connect(NoTls).await {
            Ok((client, connection)) => {
                tokio::spawn(async move {
                    if let Err(e) = connection.await {
                        eprintln!("Postgres connection error: {}", e.to_string());
                        process::exit(1);
                    }
                });
                pg_client = client;
                break;
            }
            Err(e) => {
                try_error!(ctx, "Error connecting to postgres: {}", e.to_string());
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
    }
    if run_migrations {
        pg_run_migrations(&mut pg_client, ctx).await;
    }
    pg_client
}

pub async fn pg_insert_runes(
    rows: &Vec<DbRune>,
    db_tx: &mut Transaction<'_>,
    ctx: &Context,
) -> Result<bool, Error> {
    for chunk in rows.chunks(500) {
        let mut arg_num = 1;
        let mut arg_str = String::new();
        let mut params: Vec<&(dyn ToSql + Sync)> = vec![];
        for row in chunk.iter() {
            arg_str.push_str("(");
            for i in 0..19 {
                arg_str.push_str(format!("${},", arg_num + i).as_str());
            }
            arg_str.pop();
            arg_str.push_str("),");
            arg_num += 19;
            params.push(&row.id);
            params.push(&row.number);
            params.push(&row.name);
            params.push(&row.spaced_name);
            params.push(&row.block_hash);
            params.push(&row.block_height);
            params.push(&row.tx_index);
            params.push(&row.tx_id);
            params.push(&row.divisibility);
            params.push(&row.premine);
            params.push(&row.symbol);
            params.push(&row.terms_amount);
            params.push(&row.terms_cap);
            params.push(&row.terms_height_start);
            params.push(&row.terms_height_end);
            params.push(&row.terms_offset_start);
            params.push(&row.terms_offset_end);
            params.push(&row.turbo);
            params.push(&row.timestamp);
        }
        arg_str.pop();
        match db_tx
            .query(
                &format!("INSERT INTO runes
                    (id, number, name, spaced_name, block_hash, block_height, tx_index, tx_id, divisibility, premine, symbol,
                    terms_amount, terms_cap, terms_height_start, terms_height_end, terms_offset_start, terms_offset_end, turbo,
                    timestamp) VALUES {}
                    ON CONFLICT (name) DO NOTHING", arg_str),
                &params,
            )
            .await
        {
            Ok(_) => {}
            Err(e) => {
                try_error!(ctx, "Error inserting runes: {:?}", e);
                process::exit(1);
            }
        };
    }
    Ok(true)
}

pub async fn pg_insert_supply_changes(
    rows: &Vec<DbSupplyChange>,
    db_tx: &mut Transaction<'_>,
    ctx: &Context,
) -> Result<bool, Error> {
    for chunk in rows.chunks(500) {
        let rune_ids: Vec<&String> = chunk.iter().map(|row| &row.rune_id).collect();

        // Fetch the latest supply_changes for each rune_id
        let previous_records: Vec<DbSupplyChange> = db_tx
            .query(
                "SELECT * FROM (
                SELECT DISTINCT ON (rune_id) *
                FROM supply_changes
                WHERE rune_id = ANY($1::text[])
                ORDER BY rune_id, block_height DESC
            ) sub",
                &[&rune_ids],
            )
            .await?
            .iter()
            .map(|row| DbSupplyChange {
                // Assuming SupplyChange is a struct that matches the table schema
                // Replace with actual field mappings
                rune_id: row.get("rune_id"),
                block_height: row.get("block_height"),
                minted: row.get("minted"),
                total_mints: row.get("total_mints"),
                burned: row.get("burned"),
                total_burns: row.get("total_burns"),
                total_operations: row.get("total_operations"),
            })
            .collect();

        // Create a map from rune_id to previous record
        let prev_map: HashMap<String, DbSupplyChange> = previous_records
            .into_iter()
            .map(|rec| (rec.rune_id.clone(), rec))
            .collect();

        // Group input rows by rune_id and sum the changes
        let mut changes_map: HashMap<String, DbSupplyChange> = HashMap::new();
        for row in chunk {
            changes_map.insert(
                row.rune_id.clone(),
                DbSupplyChange {
                    rune_id: row.rune_id.clone(),
                    block_height: row.block_height,
                    minted: row.minted,
                    total_mints: row.total_mints,
                    burned: row.burned,
                    total_burns: row.total_burns,
                    total_operations: row.total_operations,
                },
            );

            if let Some(prev_row) = prev_map.get(&row.rune_id) {
                changes_map
                    .entry(row.rune_id.clone())
                    .and_modify(|db_supply_change| {
                        db_supply_change.minted += prev_row.minted;
                        db_supply_change.total_mints += prev_row.total_mints;
                        db_supply_change.burned += prev_row.burned;
                        db_supply_change.total_burns += prev_row.total_burns;
                        db_supply_change.total_operations += prev_row.total_operations;
                    });
            }
        }

        let changes_chunk = changes_map.values();

        let mut insert_args: Vec<&(dyn ToSql + Sync)> = Vec::with_capacity(changes_chunk.len() * 7);
        let placeholders: String = (0..changes_chunk.len())
            .map(|i| {
                let base = i * 7;
                format!(
                    "(${}, ${}, ${}, ${}, ${}, ${}, ${})",
                    base + 1,
                    base + 2,
                    base + 3,
                    base + 4,
                    base + 5,
                    base + 6,
                    base + 7
                )
            })
            .collect::<Vec<String>>()
            .join(", ");

        for rec in changes_chunk {
            insert_args.push(&rec.rune_id);
            insert_args.push(&rec.block_height);
            insert_args.push(&rec.minted);
            insert_args.push(&rec.total_mints);
            insert_args.push(&rec.burned);
            insert_args.push(&rec.total_burns);
            insert_args.push(&rec.total_operations);
        }

        match db_tx.query(
            &format!(
                "INSERT INTO supply_changes (rune_id, block_height, minted, total_mints, burned, total_burns, total_operations)
                 VALUES {} 
                 ON CONFLICT (rune_id, block_height) DO UPDATE SET
                     minted = EXCLUDED.minted,
                     total_mints = EXCLUDED.total_mints,
                     burned = EXCLUDED.burned,
                     total_burns = EXCLUDED.total_burns,
                     total_operations = EXCLUDED.total_operations",
                placeholders
            ),
            &insert_args,
        ).await {
            Ok(_) => (),
            Err(e) => {
                try_error!(ctx, "Error inserting supply changes: {:?}", e);
                process::exit(1);
            }
        };
    }
    Ok(true)
}

pub async fn pg_insert_balance_changes(
    rows: &Vec<DbBalanceChange>,
    increase: bool,
    db_tx: &mut Transaction<'_>,
    ctx: &Context,
) -> Result<bool, Error> {
    for chunk in rows.chunks(500) {
        let pairs: Vec<_> = chunk
            .iter()
            .map(|row| (&row.rune_id, &row.address))
            .collect();

        let (rune_ids, addresses): (Vec<&String>, Vec<&String>) = pairs.into_iter().unzip();
        let previous_records = db_tx
            .query(
                "SELECT DISTINCT ON (rune_id, address) *
             FROM balance_changes
             WHERE (rune_id, address) IN (
                 SELECT rune_id, address 
                 FROM unnest($1::text[], $2::text[]) AS t(rune_id, address)
             )
             ORDER BY rune_id, address, block_height DESC",
                &[&rune_ids, &addresses],
            )
            .await?
            .iter()
            .map(|row| DbBalanceChange {
                rune_id: row.get("rune_id"),
                address: row.get("address"),
                block_height: row.get("block_height"),
                balance: row.get("balance"),
                total_operations: row.get("total_operations"),
            })
            .collect::<Vec<_>>();

        let prev_map: HashMap<(String, String), DbBalanceChange> = previous_records
            .into_iter()
            .map(|rec| ((rec.rune_id.clone(), rec.address.clone()), rec))
            .collect();

        // Group input rows by rune_id and sum the changes
        let mut changes_map: HashMap<(String, String), DbBalanceChange> = HashMap::new();
        for row in chunk {
            let cur_key = (row.rune_id.clone(), row.address.clone());
            changes_map.insert(
                cur_key.clone(),
                DbBalanceChange {
                    rune_id: row.rune_id.clone(),
                    address: row.address.clone(),
                    block_height: row.block_height,
                    balance: row.balance,
                    total_operations: row.total_operations,
                },
            );

            let prev_key = (row.rune_id.clone(), row.address.clone());
            if let Some(prev_row) = prev_map.get(&prev_key) {
                changes_map.entry(cur_key).and_modify(|db_balance_change| {
                    if increase {
                        db_balance_change.balance += prev_row.balance;
                    } else {
                        let mut sub_balance = prev_row.balance;
                        sub_balance -= row.balance;
                        db_balance_change.balance = sub_balance;
                    }
                    db_balance_change.total_operations += prev_row.total_operations;
                });
            }
        }

        let changes_chunk = changes_map.values();

        let mut insert_args: Vec<&(dyn ToSql + Sync)> = Vec::with_capacity(changes_chunk.len() * 5);
        let placeholders: String = (0..changes_chunk.len())
            .map(|i| {
                let base = i * 5;
                format!(
                    "(${}, ${}, ${}, ${}, ${})",
                    base + 1,
                    base + 2,
                    base + 3,
                    base + 4,
                    base + 5,
                )
            })
            .collect::<Vec<String>>()
            .join(", ");

        for rec in changes_chunk {
            insert_args.push(&rec.rune_id);
            insert_args.push(&rec.block_height);
            insert_args.push(&rec.address);
            insert_args.push(&rec.balance);
            insert_args.push(&rec.total_operations);
        }

        match db_tx
            .query(
                &format!(
                    "INSERT INTO balance_changes 
                        (rune_id, block_height, address, balance, total_operations)
                    VALUES {}
                    ON CONFLICT (rune_id, block_height, address) DO UPDATE SET
                        balance = EXCLUDED.balance,
                        total_operations = EXCLUDED.total_operations",
                    placeholders
                ),
                &insert_args,
            )
            .await
        {
            Ok(_) => (),
            Err(e) => {
                try_error!(ctx, "Error inserting supply changes: {:?}", e);
                process::exit(1);
            }
        };
    }
    Ok(true)
}

pub async fn pg_insert_ledger_entries(
    rows: &Vec<DbLedgerEntry>,
    db_tx: &mut Transaction<'_>,
    ctx: &Context,
) -> Result<bool, Error> {
    for chunk in rows.chunks(500) {
        let mut arg_num = 1;
        let mut arg_str = String::new();
        let mut params: Vec<&(dyn ToSql + Sync)> = vec![];
        for row in chunk.iter() {
            arg_str.push_str("(");
            for i in 0..12 {
                arg_str.push_str(format!("${},", arg_num + i).as_str());
            }
            arg_str.pop();
            arg_str.push_str("),");
            arg_num += 12;
            params.push(&row.rune_id);
            params.push(&row.block_hash);
            params.push(&row.block_height);
            params.push(&row.tx_index);
            params.push(&row.event_index);
            params.push(&row.tx_id);
            params.push(&row.output);
            params.push(&row.address);
            params.push(&row.receiver_address);
            params.push(&row.amount);
            params.push(&row.operation);
            params.push(&row.timestamp);
        }
        arg_str.pop();
        match db_tx
            .query(
                &format!("INSERT INTO ledger
                    (rune_id, block_hash, block_height, tx_index, event_index, tx_id, output, address, receiver_address, amount,
                    operation, timestamp)
                    VALUES {}", arg_str),
                &params,
            )
            .await
        {
            Ok(_) => {}
            Err(e) => {
                try_error!(ctx, "Error inserting ledger entries: {:?}", e);
                process::exit(1);
            }
        };
    }
    Ok(true)
}

pub async fn pg_roll_back_block(block_height: u64, db_tx: &mut Transaction<'_>, _ctx: &Context) {
    db_tx
        .execute(
            "DELETE FROM balance_changes WHERE block_height = $1",
            &[&PgNumericU64(block_height)],
        )
        .await
        .expect("error rolling back balance_changes");
    db_tx
        .execute(
            "DELETE FROM supply_changes WHERE block_height = $1",
            &[&PgNumericU64(block_height)],
        )
        .await
        .expect("error rolling back supply_changes");
    db_tx
        .execute(
            "DELETE FROM ledger WHERE block_height = $1",
            &[&PgNumericU64(block_height)],
        )
        .await
        .expect("error rolling back ledger");
    db_tx
        .execute(
            "DELETE FROM runes WHERE block_height = $1",
            &[&PgNumericU64(block_height)],
        )
        .await
        .expect("error rolling back runes");
}

pub async fn pg_get_max_rune_number<T: GenericClient>(client: &T, _ctx: &Context) -> u32 {
    let row = client
        .query_opt("SELECT MAX(number) AS max FROM runes", &[])
        .await
        .expect("error getting max rune number");
    let Some(row) = row else {
        return 0;
    };
    let max: PgBigIntU32 = row.get("max");
    max.0
}

pub async fn pg_get_block_height(client: &mut Client, _ctx: &Context) -> Option<u64> {
    let row = client
        .query_opt("SELECT MAX(block_height) AS max FROM ledger", &[])
        .await
        .expect("error getting max block height")?;
    let max: Option<PgNumericU64> = row.get("max");
    if let Some(max) = max {
        Some(max.0)
    } else {
        None
    }
}

pub async fn pg_get_last_block_height(client: &mut Client, _ctx: &Context) -> Option<u64> {
    let row = client
        .query_opt(
            "SELECT last_scanned_height as height FROM block_height",
            &[],
        )
        .await
        .expect("error getting max block height")?;
    let height: Option<PgNumericU64> = row.get("height");
    if let Some(height) = height {
        Some(height.0)
    } else {
        None
    }
}

pub async fn pg_get_rune_by_id(
    id: &RuneId,
    db_tx: &mut Transaction<'_>,
    ctx: &Context,
) -> Option<DbRune> {
    let row = match db_tx
        .query_opt("SELECT * FROM runes WHERE id = $1", &[&id.to_string()])
        .await
    {
        Ok(row) => row,
        Err(e) => {
            try_error!(ctx, "error retrieving rune: {}", e.to_string());
            process::exit(1);
        }
    };
    let Some(row) = row else {
        return None;
    };
    Some(DbRune::from_pg_row(&row))
}

pub async fn pg_get_rune_total_mints(
    id: &RuneId,
    db_tx: &mut Transaction<'_>,
    ctx: &Context,
) -> Option<u128> {
    let row = match db_tx
        .query_opt(
            "SELECT total_mints FROM supply_changes WHERE rune_id = $1 ORDER BY block_height DESC LIMIT 1",
            &[&id.to_string()],
        )
        .await
    {
        Ok(row) => row,
        Err(e) => {
            try_error!(
                ctx,
                "error retrieving rune minted total: {}",
                e.to_string()
            );
            process::exit(1);
        }
    };
    let Some(row) = row else {
        return None;
    };
    let minted: PgTextU128 = row.get("total_mints");
    Some(minted.0)
}

/// Retrieves the rune balance for an array of transaction inputs represented by `(vin, tx_id, vout)` where `vin` is the index of
/// this transaction input, `tx_id` is the transaction ID that produced this input and `vout` is the output index of this previous
/// tx.
pub async fn pg_get_input_rune_balances(
    outputs: Vec<(u32, String, u32)>,
    db_tx: &mut Transaction<'_>,
    ctx: &Context,
) -> HashMap<u32, HashMap<RuneId, Vec<InputRuneBalance>>> {
    // Instead of preparing a statement and running it thousands of times, pull all rows with 1 query.
    let mut arg_num = 1;
    let mut args = String::new();
    let mut data = vec![];
    for (input_index, tx_id, output) in outputs.iter() {
        args.push_str(
            format!(
                "(${}::bigint,${},${}::bigint),",
                arg_num,
                arg_num + 1,
                arg_num + 2
            )
            .as_str(),
        );
        arg_num += 3;
        data.push((PgBigIntU32(*input_index), tx_id, PgBigIntU32(*output)));
    }
    args.pop();
    let mut params: Vec<&(dyn ToSql + Sync)> = vec![];
    for d in data.iter() {
        params.push(&d.0);
        params.push(d.1);
        params.push(&d.2);
    }
    let rows = match db_tx
        .query(
            format!(
                "WITH inputs (index, tx_id, output) AS (VALUES {})
                SELECT i.index, l.rune_id, l.address, l.amount
                FROM ledger AS l
                INNER JOIN inputs AS i USING (tx_id, output)
                WHERE l.operation = 'receive'",
                args
            )
            .as_str(),
            &params,
        )
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            try_error!(
                ctx,
                "error retrieving output rune balances: {}",
                e.to_string()
            );
            process::exit(1);
        }
    };
    let mut results: HashMap<u32, HashMap<RuneId, Vec<InputRuneBalance>>> = HashMap::new();
    for row in rows.iter() {
        let key: PgBigIntU32 = row.get("index");
        let rune_str: String = row.get("rune_id");
        let rune_id = RuneId::from_str(rune_str.as_str()).unwrap();
        let address: Option<String> = row.get("address");
        let amount: PgTextU128 = row.get("amount");
        let input_bal = InputRuneBalance {
            address,
            amount: amount.0,
        };
        if let Some(input) = results.get_mut(&key.0) {
            if let Some(rune_bal) = input.get_mut(&rune_id) {
                rune_bal.push(input_bal);
            } else {
                input.insert(rune_id, vec![input_bal]);
            }
        } else {
            let mut map = HashMap::new();
            map.insert(rune_id, vec![input_bal]);
            results.insert(key.0, map);
        }
    }
    results
}

pub async fn pg_update_block_height(
    new_block_height: u64,
    db_tx: &mut Transaction<'_>,
    ctx: &Context,
) -> Result<(), Error> {
    match db_tx
        .query(
            "UPDATE block_height SET last_scanned_height = $1;",
            &[&PgNumericU64(new_block_height)],
        )
        .await
    {
        Ok(updated_rows) => {
            assert_eq!(updated_rows.len(), 0);
        }
        Err(e) => {
            try_error!(ctx, "Error updating block height: {:?}", e);
        }
    };
    Ok(())
}

#[cfg(test)]
pub async fn pg_test_client(run_migrations: bool, ctx: &Context) -> Client {
    let (mut client, connection) =
        tokio_postgres::connect("host=localhost user=postgres password=postgres", NoTls)
            .await
            .unwrap();
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            eprintln!("test connection error: {}", e);
        }
    });
    if run_migrations {
        pg_run_migrations(&mut client, ctx).await;
    }
    client
}

#[cfg(test)]
pub async fn pg_test_roll_back_migrations(pg_client: &mut Client, ctx: &Context) {
    match pg_client
        .batch_execute(
            "
            DO $$ DECLARE
                r RECORD;
            BEGIN
                FOR r IN (SELECT tablename FROM pg_tables WHERE schemaname = current_schema()) LOOP
                    EXECUTE 'DROP TABLE IF EXISTS ' || quote_ident(r.tablename) || ' CASCADE';
                END LOOP;
            END $$;
            DO $$ DECLARE
                r RECORD;
            BEGIN
                FOR r IN (SELECT typname FROM pg_type WHERE typtype = 'e' AND typnamespace = (SELECT oid FROM pg_namespace WHERE nspname = current_schema())) LOOP
                    EXECUTE 'DROP TYPE IF EXISTS ' || quote_ident(r.typname) || ' CASCADE';
                END LOOP;
            END $$;",
        )
        .await {
            Ok(rows) => rows,
            Err(e) => {
                try_error!(
                    ctx,
                    "error rolling back test migrations: {}",
                    e.to_string()
                );
                process::exit(1);
            }
        };
}
