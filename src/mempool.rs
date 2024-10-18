use std::collections::{HashMap, HashSet};
use std::process;
use std::sync::{Arc, RwLock};

use crate::config::Config;
use crate::db::cache::input_rune_balance::InputRuneBalance;

use crate::db::models::db_ledger_operation::DbLedgerOperation;
use crate::db::types::pg_bigint_u32::PgBigIntU32;
use crate::db::types::pg_numeric_u128::PgNumericU128;
use crate::db::{pg_connect, pg_get_input_rune_balances};
use crate::{try_debug, try_error, try_info, try_warn};
use bitcoin::consensus::deserialize;
use bitcoin::{Address, Network, ScriptBuf, TxIn};

use chainhook_sdk::bitcoincore_rpc::{self, Auth, RpcApi};

use chainhook_sdk::types::BitcoinBlockSignaling;
use chainhook_sdk::utils::Context;
use crossbeam_channel::{select, Receiver, Sender};
use tokio_postgres::types::ToSql;
use tokio_postgres::{Client, Error, Transaction};

use ordinals::{Artifact, Edict, RuneId, Runestone};
use zmq::Socket;

const UNINCLUDED_RUNE_ID: RuneId = RuneId { block: 0, tx: 0 };

pub struct DBMempoolLedgerEntry {
    pub rune_id: String,
    pub event_index: PgBigIntU32,
    pub tx_id: String,
    pub output: Option<PgBigIntU32>,
    pub address: Option<String>,
    pub receiver_address: Option<String>,
    pub amount: Option<PgNumericU128>,
    pub operation: DbLedgerOperation,
}
impl DBMempoolLedgerEntry {
    fn from_values(
        amount: Option<u128>,
        rune_id: RuneId,
        output: Option<u32>,
        address: Option<&String>,
        receiver_address: Option<&String>,
        operation: DbLedgerOperation,
        tx_id: &String,
        event_index: u32,
    ) -> Self {
        Self {
            rune_id: rune_id.to_string(),
            event_index: PgBigIntU32(event_index),
            tx_id: tx_id.clone(),
            output: output.map(PgBigIntU32),
            address: address.cloned(),
            receiver_address: receiver_address.cloned(),
            amount: amount.map(PgNumericU128),
            operation,
        }
    }
    fn new_mempool_entry_with_idx_incr(
        amount: Option<u128>,
        rune_id: RuneId,
        output: Option<u32>,
        address: Option<&String>,
        receiver_address: Option<&String>,
        operation: DbLedgerOperation,
        tx_id: &String,
        next_event_index: &mut u32,
    ) -> DBMempoolLedgerEntry {
        let entry = DBMempoolLedgerEntry::from_values(
            amount,
            rune_id,
            output,
            address,
            receiver_address,
            operation,
            tx_id,
            *next_event_index,
        );
        *next_event_index += 1;
        entry
    }
}

#[derive(Clone)]
pub struct ExtendedContext {
    ctx: Context,
    // Cache storing currently added mempool transactions
    // Use of dashmap might be more appropriate heere
    mempool_cache: MempoolCacheArcRw,
    network: Network,
}

pub async fn set_up_mempool_sidecar_runloop(
    config: &Config,
    ctx: &Context,
) -> Result<Sender<String>, ()> {
    let network = config.get_bitcoin_network();

    let extended_ctx = ExtendedContext {
        ctx: ctx.clone(),
        mempool_cache: Arc::new(RwLock::new(HashSet::new())),
        network,
    };

    // actualize mempool cache from postgres
    {
        let mut pg_client = pg_connect(config, false, &extended_ctx.ctx).await;
        pg_actualize_mempool_cache(&extended_ctx, &mut pg_client).await;

        try_info!(ctx, "Scanning mempool for txs");
        if let Err(e) = scan_mempool(&config, &extended_ctx, &mut pg_client).await {
            try_error!(ctx, "Failed to scan mempool with rpc: {}", e.to_string());
        }
        try_info!(ctx, "Finished scanning mempool for txs.");
    }

    try_info!(ctx, "Starting to watch mempool updates.");
    let config_cln = config.clone();
    let extended_ctx_cln = extended_ctx.clone();
    let _ =
        hiro_system_kit::thread_named("Transactions Observer Sidecar Runloop").spawn(move || {
            hiro_system_kit::nestable_block_on(async {
                start_zeromq_runloop(&config_cln, &extended_ctx_cln).await;
            });
        });

    let config_cln = config.clone();
    let extended_ctx_cln = extended_ctx.clone();
    let (transaction_id_tx, transaction_id_rx) = crossbeam_channel::unbounded();
    let _ =
        hiro_system_kit::thread_named("Transactions Observer Sidecar Runloop").spawn(move || {
            hiro_system_kit::nestable_block_on(async {
                remove_escaped_transactions(transaction_id_rx, &config_cln, &extended_ctx_cln)
                    .await;
            });
        });

    Ok(transaction_id_tx)
}

fn new_rpc_client(
    url: &str,
    auth: Auth,
) -> Result<bitcoincore_rpc::Client, bitcoincore_rpc::Error> {
    bitcoincore_rpc::Client::new(url, auth)
}

fn new_zmq_socket() -> Socket {
    let context = zmq::Context::new();
    let socket = context.socket(zmq::SUB).unwrap();
    // watching the raw transaction
    assert!(socket.set_subscribe(b"rawtx").is_ok());
    assert!(socket.set_rcvhwm(0).is_ok());
    // We override the OS default behavior:
    assert!(socket.set_tcp_keepalive(1).is_ok());
    // The keepalive routine will wait for 5 minutes
    assert!(socket.set_tcp_keepalive_idle(300).is_ok());
    // And then resend it every 60 seconds
    assert!(socket.set_tcp_keepalive_intvl(60).is_ok());
    // 120 times
    assert!(socket.set_tcp_keepalive_cnt(120).is_ok());
    socket
}

type MempoolCacheArcRw = Arc<RwLock<HashSet<String>>>;

pub async fn start_zeromq_runloop(config: &Config, extended_ctx: &ExtendedContext) {
    let BitcoinBlockSignaling::ZeroMQ(ref bitcoind_zmq_url) =
        config.event_observer.bitcoin_block_signaling
    else {
        unreachable!()
    };

    let mut pg_client = pg_connect(config, false, &extended_ctx.ctx).await;

    try_info!(
        extended_ctx.ctx,
        "Waiting for ZMQ connection acknowledgment from bitcoind"
    );

    let mut socket = new_zmq_socket();
    assert!(socket.connect(bitcoind_zmq_url).is_ok());
    try_info!(extended_ctx.ctx, "Waiting for ZMQ messages from bitcoind");

    // watch the incoming messages from zmq
    loop {
        // read transaction
        let msg = match socket.recv_multipart(0) {
            Ok(msg) => msg,
            Err(e) => {
                try_error!(
                    extended_ctx.ctx,
                    "Unable to receive ZMQ message: {}",
                    e.to_string()
                );
                socket = new_zmq_socket();
                assert!(socket.connect(&bitcoind_zmq_url).is_ok());
                continue;
            }
        };
        let (_topic, data, _sequence) = (&msg[0], &msg[1], &msg[2]);

        let tx = match deserialize::<bitcoin::Transaction>(&data) {
            Ok(transaction) => transaction,
            Err(e) => {
                try_error!(
                    extended_ctx.ctx,
                    "Failed to decode transaction: {}",
                    e.to_string()
                );
                continue;
            }
        };

        // Might be a case, when TX is in the cache but not yet in Postgres
        // and when the request to delete TX comes it can't be executed
        // Hashmap with flags that indicates processing step might be needed
        let tx_id = tx.compute_txid().to_string();
        let insertion_res = extended_ctx
            .mempool_cache
            .write()
            .and_then(|mut hs| Ok(hs.insert(tx_id.clone())));
        match insertion_res {
            Ok(true) => {}
            Ok(false) => continue,
            Err(e) => {
                try_error!(
                    extended_ctx.ctx,
                    "ZMQ Failed to write to mempool cache: {}",
                    e
                );
            }
        }

        let mut db_tx = pg_client
            .transaction()
            .await
            .expect("Unable to begin bitcoin tx processing pg transaction");
        // db_tx is used here to retrive data from postgres
        let ledger_entries = parse_mempool_tx(tx, tx_id, &mut db_tx, extended_ctx).await;
        // Commit to db
        let _ =
            pg_insert_mempool_ledger_entries(&ledger_entries, &mut db_tx, &extended_ctx.ctx).await;

        db_tx
            .commit()
            .await
            .expect("Unable to commit pg transaction");
    }
}

// get all tx from the current state of mempool using bitcoin rpc
pub async fn scan_mempool(
    config: &Config,
    extended_ctx: &ExtendedContext,
    pg_client: &mut Client,
) -> Result<(), bitcoincore_rpc::Error> {
    let rpc_auth = Auth::UserPass(
        config.event_observer.bitcoind_rpc_username.clone(),
        config.event_observer.bitcoind_rpc_password.clone(),
    );
    let rpc = new_rpc_client(&config.event_observer.bitcoind_rpc_url, rpc_auth)
        .expect("Failed to sart an rpc client.");

    let mut db_tx = pg_client
        .transaction()
        .await
        .expect("Unable to begin bitcoin tx processing pg transaction");
    let mut ledger_entries = Vec::new();

    let txs = rpc.get_raw_mempool()?;
    let txs_hs: HashSet<String> = txs
        .clone()
        .into_iter()
        .map(|tx_id| tx_id.to_string())
        .collect();
    // removing transaction ids that do not represent current mempool state
    let tx_to_delete = extended_ctx
        .mempool_cache
        .read()
        .and_then(|hs| {
            let difference: Vec<String> = hs.difference(&txs_hs).cloned().collect();
            Ok(difference)
        })
        .expect("Faielde to get read lock on memcache");

    pg_remove_mempool_tx(&tx_to_delete, &mut db_tx, &extended_ctx.ctx).await;

    for tx_id in txs.into_iter() {
        try_debug!(
            extended_ctx.ctx,
            "Got new tx_id from scanning mempool {}",
            tx_id
        );
        let err = extended_ctx
            .mempool_cache
            .write()
            .and_then(|mut hs| Ok(hs.insert(tx_id.to_string())));
        match err {
            Err(e) => {
                try_error!(
                    extended_ctx.ctx,
                    "RPC Failed to write to mempool cache: {}",
                    e
                );
            }
            Ok(true) => {}
            // Already stored, can skip
            Ok(false) => continue,
        }

        let tx: Option<bitcoin::Transaction> = rpc.get_by_id(&tx_id).ok().into();
        if let Some(tx) = tx {
            let new_entries =
                parse_mempool_tx(tx, tx_id.to_string(), &mut db_tx, extended_ctx).await;
            ledger_entries.extend(new_entries);
        }
    }
    let _ = pg_insert_mempool_ledger_entries(&ledger_entries, &mut db_tx, &extended_ctx.ctx).await;
    db_tx
        .commit()
        .await
        .expect("Unable to commit pg transaction");

    Ok(())
}

async fn remove_escaped_transactions(
    transaction_id_rx: Receiver<String>,
    config: &Config,
    extended_ctx: &ExtendedContext,
) {
    let mut pg_client = pg_connect(config, false, &extended_ctx.ctx).await;
    loop {
        select! {
            recv(transaction_id_rx) -> msg => {
                if let Ok(tx_id) = msg {
                    let removed =  extended_ctx
                        .mempool_cache
                        .write()
                        .and_then(|mut hs| Ok(hs.remove(&tx_id)));
                    match removed {
                        Ok(true) => {
                            let mut db_tx = pg_client
                                .transaction()
                                .await
                                .expect("Unable to begin bitcoin tx processing pg transaction");
                            pg_remove_mempool_tx(&[tx_id], &mut db_tx, &extended_ctx.ctx).await;
                            db_tx
                                .commit()
                                .await
                                .expect("Unable to commit pg transaction");
                        },
                        Ok(false) => {}
                        Err(e) => {try_error!(extended_ctx.ctx, "Error while trying to remove a TX ID from Mempool cache: {}", e.to_string());
                        },
                    }
                }
            }
        }
    }
}

async fn parse_mempool_tx(
    tx: bitcoin::Transaction,
    tx_id: String,
    db_tx: &mut Transaction<'_>,
    extended_ctx: &ExtendedContext,
) -> Vec<DBMempoolLedgerEntry> {
    let mut ledger_entries = Vec::new();

    let mut output_pointer = None;
    let mut next_event_index: u32 = 0;

    let tx_total_outputs = tx.output.len() as u32;
    try_debug!(extended_ctx.ctx, "New tx to parse {}", tx_id);

    if let Some(artifact) = Runestone::decipher(&tx) {
        let mut eligible_outputs = collect_eligible_outputs(&tx);
        let mut input_runes_balances =
            collect_input_rune_balances_pg(&tx.input, db_tx, &extended_ctx.ctx).await;

        match artifact {
            Artifact::Runestone(runestone) => {
                try_debug!(extended_ctx.ctx, "The tx contains Runestone");
                // apply_runestone
                if let Some(new_pointer) = runestone.pointer {
                    output_pointer = Some(new_pointer);
                }
                // rune_id is 0:0 as they haven't been included in any block yet
                // RuneID is block_hight:tx_index_in_block

                for edict in runestone.edicts.iter() {
                    // Not supporting TXs with uincluded runde edicts
                    if edict.id == UNINCLUDED_RUNE_ID {
                        ledger_entries.clear();
                        input_runes_balances.clear();
                        eligible_outputs.clear();
                        break;
                    }
                    // apply_edict
                    ledger_entries.extend(collect_edicts(
                        edict,
                        &mut input_runes_balances,
                        &eligible_outputs,
                        &mut next_event_index,
                        &tx_id,
                        tx_total_outputs,
                        &extended_ctx,
                    ));
                }
            }
            Artifact::Cenotaph(cenotaph) => {
                // apply_cenotaph
                ledger_entries.extend(burn_input(
                    &tx_id,
                    &mut input_runes_balances,
                    &mut next_event_index,
                ));

                if let Some(_etching) = cenotaph.etching {
                    // apply_cenotaph_etching
                    ledger_entries.push(apply_cenotaph_etching(
                        &UNINCLUDED_RUNE_ID,
                        &tx_id,
                        &mut next_event_index,
                    ));
                }
            }
        }
        let allocate_remain_balance_entries = allocate_remaining_balances(
            &tx_id,
            output_pointer,
            &mut input_runes_balances,
            &eligible_outputs,
            &mut next_event_index,
            extended_ctx,
        );
        ledger_entries.extend(allocate_remain_balance_entries);
    }
    ledger_entries
}

fn allocate_remaining_balances(
    tx_id: &String,
    output_pointer: Option<u32>,
    input_runes_balances: &mut HashMap<RuneId, Vec<InputRuneBalance>>,
    eligible_outputs: &HashMap<u32, ScriptBuf>,
    next_event_index: &mut u32,
    ctx: &ExtendedContext,
) -> Vec<DBMempoolLedgerEntry> {
    let mut results = vec![];
    for (rune_id, unallocated) in input_runes_balances.iter_mut() {
        #[cfg(not(feature = "release"))]
        for input in unallocated.iter() {
            try_debug!(
                ctx.ctx,
                "Assign unallocated {} to pointer {:?} {:?} ({})",
                rune_id,
                output_pointer,
                input.address,
                input.amount,
            );
        }
        results.extend(move_rune_balance_to_output(
            output_pointer,
            rune_id,
            unallocated,
            &eligible_outputs,
            0, // All of it
            next_event_index,
            tx_id,
            ctx,
        ));
    }
    input_runes_balances.clear();
    results
}

async fn pg_remove_mempool_tx(tx_ids: &[String], db_tx: &mut Transaction<'_>, ctx: &Context) {
    let mut arg_str = String::new();
    let mut params: Vec<&(dyn ToSql + Sync)> = vec![];
    for i in 1..=tx_ids.len() {
        arg_str.push_str(format!("${},", i).as_str());
    }
    // removing last comma
    arg_str.pop();
    for tx_id in tx_ids {
        params.push(tx_id);
    }
    // Maybe its better idea to keep a batch of tx to delete
    let query_str = format!(
        "DELETE FROM mempool_ledger WHERE transaction_id IN ({})",
        arg_str
    );
    match db_tx.query(&query_str, &params).await {
        Ok(_) => {}
        Err(e) => {
            try_error!(ctx, "Error deleting mempool_ledger entries: {:?}", e);
            process::exit(1);
        }
    };
}

pub async fn pg_insert_mempool_ledger_entries(
    rows: &Vec<DBMempoolLedgerEntry>,
    db_tx: &mut Transaction<'_>,
    ctx: &Context,
) -> Result<bool, Error> {
    if rows.is_empty() {
        return Ok(false);
    }
    let mut arg_num = 1;
    let mut arg_str = String::new();
    let mut params: Vec<&(dyn ToSql + Sync)> = vec![];
    let total_arg_cols = 8;
    for row in rows.iter() {
        arg_str.push_str("(");
        for i in 0..total_arg_cols {
            arg_str.push_str(format!("${},", arg_num + i).as_str());
        }
        arg_str.pop();
        arg_str.push_str("),");
        arg_num += total_arg_cols;
        params.push(&row.rune_id);
        params.push(&row.event_index);
        params.push(&row.tx_id);
        params.push(&row.output);
        params.push(&row.address);
        params.push(&row.receiver_address);
        params.push(&row.amount);
        params.push(&row.operation);
    }
    arg_str.pop();
    match db_tx
        .query(
            &format!(
                "INSERT INTO mempool_ledger
                    (rune_id, event_index, tx_id, output, address, receiver_address, amount,
                    operation)
                    VALUES {}",
                arg_str
            ),
            &params,
        )
        .await
    {
        Ok(_) => {}
        Err(e) => {
            try_error!(ctx, "Error inserting mempool_ledger entries: {:?}", e);
            process::exit(1);
        }
    };

    Ok(true)
}

async fn pg_actualize_mempool_cache(ctx: &ExtendedContext, pg_client: &mut Client) {
    let rows = pg_client
        .query("SELECT tx_id FROM mempool_ledger;", &[])
        .await
        .expect("error getting tx_ids of the mempool");
    for row in rows.iter() {
        let tx_id = row.get("tx_id");
        ctx.mempool_cache
            .write()
            .and_then(|mut hs| Ok(hs.insert(tx_id)))
            .expect("Failed to write to mempool cache");
    }
}

fn apply_cenotaph_etching(
    rune_id: &RuneId,
    tx_id: &String,
    mut next_event_index: &mut u32,
) -> DBMempoolLedgerEntry {
    DBMempoolLedgerEntry::new_mempool_entry_with_idx_incr(
        None,
        *rune_id,
        None,
        None,
        None,
        DbLedgerOperation::Etching,
        tx_id,
        &mut next_event_index,
    )
}
fn burn_input(
    tx_id: &String,
    input_runes: &mut HashMap<RuneId, Vec<InputRuneBalance>>,
    next_event_index: &mut u32,
) -> Vec<DBMempoolLedgerEntry> {
    let mut results = vec![];
    for (rune_id, unallocated) in input_runes.iter() {
        for balance in unallocated {
            results.push(DBMempoolLedgerEntry::new_mempool_entry_with_idx_incr(
                Some(balance.amount),
                *rune_id,
                None,
                balance.address.as_ref(),
                None,
                DbLedgerOperation::Burn,
                tx_id,
                next_event_index,
            ));
        }
    }
    input_runes.clear();
    results
}

fn collect_edicts(
    edict: &Edict,
    input_runes_balances: &mut HashMap<RuneId, Vec<InputRuneBalance>>,
    eligible_outputs: &HashMap<u32, ScriptBuf>,
    mut next_event_index: &mut u32,
    tx_id: &String,
    tx_total_outputs: u32,
    ctx: &ExtendedContext,
) -> Vec<DBMempoolLedgerEntry> {
    let rune_id = if edict.id.block == 0 && edict.id.tx == 0 {
        return vec![];
    } else {
        edict.id
    };

    // Take all the available inputs for the rune we're trying to move.
    let Some(available_inputs) = input_runes_balances.get_mut(&rune_id) else {
        try_info!(ctx.ctx, "No unallocated runes remain for edict {}", rune_id);
        return vec![];
    };
    // Calculate the maximum unallocated balance we can move.
    let unallocated = available_inputs
        .iter()
        .map(|b| b.amount)
        .reduce(|acc, e| acc + e)
        .unwrap_or(0);
    // Perform movements.
    let mut results = vec![];
    if eligible_outputs.len() == 0 {
        // No eligible outputs means burn.
        try_info!(ctx.ctx, "No eligible outputs for edict on rune {}", rune_id);
        results.extend(move_rune_balance_to_output(
            None, // This will force a burn.
            &rune_id,
            available_inputs,
            &eligible_outputs,
            edict.amount,
            &mut next_event_index,
            tx_id,
            ctx,
        ));
    } else {
        match edict.output {
            // An edict with output equal to the number of transaction outputs allocates `amount` runes to each non-OP_RETURN
            // output in order.
            output if output == tx_total_outputs => {
                let mut output_keys: Vec<u32> = eligible_outputs.keys().cloned().collect();
                output_keys.sort();
                if edict.amount == 0 {
                    // Divide equally. If the number of unallocated runes is not divisible by the number of non-OP_RETURN outputs,
                    // 1 additional rune is assigned to the first R non-OP_RETURN outputs, where R is the remainder after dividing
                    // the balance of unallocated units of rune id by the number of non-OP_RETURN outputs.
                    let len = eligible_outputs.len() as u128;
                    let per_output = unallocated / len;
                    let mut remainder = unallocated % len;
                    for output in output_keys {
                        let mut extra = 0;
                        if remainder > 0 {
                            extra = 1;
                            remainder -= 1;
                        }
                        results.extend(move_rune_balance_to_output(
                            Some(output),
                            &rune_id,
                            available_inputs,
                            &eligible_outputs,
                            per_output + extra,
                            &mut next_event_index,
                            tx_id,
                            ctx,
                        ));
                    }
                } else {
                    // Give `amount` to all outputs or until unallocated runs out.
                    for output in output_keys {
                        let amount = edict.amount.min(unallocated);
                        results.extend(move_rune_balance_to_output(
                            Some(output),
                            &rune_id,
                            available_inputs,
                            &eligible_outputs,
                            amount,
                            &mut next_event_index,
                            tx_id,
                            ctx,
                        ));
                    }
                }
            }
            // Send balance to the output specified by the edict.
            output if output < tx_total_outputs => {
                let mut amount = edict.amount;
                if edict.amount == 0 {
                    amount = unallocated;
                }
                results.extend(move_rune_balance_to_output(
                    Some(edict.output),
                    &rune_id,
                    available_inputs,
                    &eligible_outputs,
                    amount,
                    &mut next_event_index,
                    tx_id,
                    ctx,
                ));
            }
            _ => {
                try_info!(
                    ctx.ctx,
                    "Edict for {} attempted move to nonexistent output {}, amount will be burnt",
                    edict.id,
                    edict.output,
                );
                results.extend(move_rune_balance_to_output(
                    None, // Burn.
                    &rune_id,
                    available_inputs,
                    &eligible_outputs,
                    edict.amount,
                    &mut next_event_index,
                    tx_id,
                    ctx,
                ));
            }
        }
    }
    results
}

fn move_rune_balance_to_output(
    output: Option<u32>,
    rune_id: &RuneId,
    input_balances: &mut Vec<InputRuneBalance>,
    outputs: &HashMap<u32, ScriptBuf>,
    amount: u128,
    next_event_index: &mut u32,
    tx_id: &String,
    ctx: &ExtendedContext,
) -> Vec<DBMempoolLedgerEntry> {
    let mut results = vec![];
    // Who is this balance going to?
    let receiver_address = if let Some(output) = output {
        match outputs.get(&output) {
            Some(script) => match Address::from_script(script, ctx.network.clone()) {
                Ok(address) => Some(address.to_string()),
                Err(e) => {
                    try_warn!(
                        ctx.ctx,
                        "Unable to decode address for output {}, {}",
                        output,
                        e
                    );
                    None
                }
            },
            None => {
                try_info!(
                    ctx.ctx,
                    "Attempted move to non-eligible output {}, runes will be burnt",
                    output
                );
                None
            }
        }
    } else {
        None
    };
    let operation = if receiver_address.is_some() {
        DbLedgerOperation::Send
    } else {
        DbLedgerOperation::Burn
    };

    // Gather balance to be received by taking it from the available inputs until the amount to move is satisfied.
    let mut total_sent = 0;
    let mut senders = vec![];
    loop {
        // Do we still have input balance left to move?
        // CHECK here, previously POP FRONT
        let Some(input_bal) = input_balances.pop() else {
            break;
        };
        // Select the correct move amount.
        let balance_taken = if amount == 0 {
            input_bal.amount
        } else {
            input_bal.amount.min(amount - total_sent)
        };
        total_sent += balance_taken;
        // If the input balance came from an address, add to `Send` operations.
        if let Some(sender_address) = input_bal.address.clone() {
            senders.push((balance_taken, sender_address));
        }
        // Is there still some balance left on this input? If so, keep it for later but break the loop because we've satisfied the
        // move amount.
        if balance_taken < input_bal.amount {
            input_balances.push(InputRuneBalance {
                address: input_bal.address,
                amount: input_bal.amount - balance_taken,
            });
            break;
        }
        // Have we finished moving balance?
        if total_sent == amount {
            break;
        }
    }
    // Add the "receive" entry, if applicable.
    if receiver_address.is_some() && total_sent > 0 {
        results.push(DBMempoolLedgerEntry::new_mempool_entry_with_idx_incr(
            Some(total_sent),
            *rune_id,
            output,
            receiver_address.as_ref(),
            None,
            DbLedgerOperation::Receive,
            tx_id,
            next_event_index,
        ));
        try_info!(
            ctx.ctx,
            "{} {} ({}) {}",
            DbLedgerOperation::Receive,
            rune_id,
            total_sent,
            receiver_address.as_ref().unwrap(),
        );
    }
    // Add the "send"/"burn" entries.
    for (balance_taken, sender_address) in senders.iter() {
        results.push(DBMempoolLedgerEntry::new_mempool_entry_with_idx_incr(
            Some(*balance_taken),
            *rune_id,
            output,
            Some(sender_address),
            receiver_address.as_ref(),
            operation.clone(),
            tx_id,
            next_event_index,
        ));
        try_info!(
            ctx.ctx,
            "{} {} ({}) {} -> {:?}",
            operation,
            rune_id,
            balance_taken,
            sender_address,
            receiver_address
        );
    }
    results
}

// add output_cache
// not sure about either using arc with mutex or storing a local copy of the cache
async fn collect_input_rune_balances_pg(
    inputs: &Vec<TxIn>,
    db_tx: &mut Transaction<'_>,
    ctx: &Context,
) -> HashMap<RuneId, Vec<InputRuneBalance>> {
    let outputs = inputs
        .iter()
        .enumerate()
        .map(|(i, inpt)| {
            // hash in chainhook is string –– watch the parsing process to replicate
            let tx_id = inpt.previous_output.txid.to_string();
            // let tx_id = inpt.previous_output.txid.hash[2..].to_string();
            let vout = inpt.previous_output.vout;
            (i as u32, tx_id, vout)
        })
        .collect();
    let output_balances: HashMap<u32, HashMap<RuneId, Vec<InputRuneBalance>>> =
        pg_get_input_rune_balances(outputs, db_tx, ctx).await;
    // convert them into RuneId to Vec<InputRuneBalance>
    output_balances
        .values()
        .fold(HashMap::new(), |mut acc, rune_to_vec| {
            rune_to_vec.iter().for_each(|(k, v)| {
                acc.entry(*k)
                    .and_modify(|e: &mut Vec<InputRuneBalance>| e.extend(v.clone()))
                    .or_insert(v.clone());
            });
            acc
        })
}

fn collect_eligible_outputs(tx: &bitcoin::Transaction) -> HashMap<u32, ScriptBuf> {
    let mut eligible_outputs = HashMap::new();
    for (i, output) in tx.output.iter().enumerate() {
        if !output.script_pubkey.is_op_return() {
            eligible_outputs.insert(i as u32, output.script_pubkey.clone());
        }
    }
    eligible_outputs
}
