use std::collections::{HashMap, HashSet};
use std::process;
use std::sync::{Arc, Mutex, RwLock};

use crate::config::Config;
use crate::db::cache::index_cache::IndexCache;
use crate::db::cache::input_rune_balance::InputRuneBalance;
use crate::db::index::{get_rune_genesis_block_height, index_block, roll_back_block};
use crate::db::models::db_ledger_operation::DbLedgerOperation;
use crate::db::models::db_rune::DbRune;
use crate::db::types::pg_bigint_u32::PgBigIntU32;
use crate::db::types::pg_numeric_u128::PgNumericU128;
use crate::db::{pg_connect, pg_get_block_height, pg_get_input_rune_balances, pg_get_rune_by_id};
use crate::{try_error, try_info, try_warn};
use bitcoin::consensus::deserialize;
use bitcoin::hashes::Hash;
use bitcoin::{Address, Network, ScriptBuf, TxIn};

use chainhook_sdk::bitcoin::hashes::Hash as ChHash;
use chainhook_sdk::bitcoincore_rpc::{self, Auth, RpcApi};
use chainhook_sdk::indexer::bitcoin::build_http_client;
use chainhook_sdk::observer::{BitcoinBlockDataCached, EventObserverConfig};
use chainhook_sdk::types::{BitcoinBlockSignaling, BlockIdentifier};
use chainhook_sdk::{
    observer::{start_event_observer, ObserverEvent, ObserverSidecar},
    utils::Context,
};
use tokio_postgres::types::ToSql;
use tokio_postgres::{Client, Error, Transaction};

use crossbeam_channel::{select, Sender};
use lru::LruCache;
use ordinals::{Artifact, Edict, Etching, Rune, RuneId, Runestone};
use zmq::Socket;

// Todo:
// 1) Link all together
// 2) Check etching for cenotaph
// 3) Check the output fields and use output pointer
// 4) Connect to the block scanning part
//    by providing a map or a channel with tx update to remove included txs from postgres

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

pub struct ExtendedContext {
    ctx: Context,
    rune_cache: RuneCacheArcMut,
    network: Network,
}

pub async fn set_up_mempool_sidecar_runloop(
    config: &Config,
    rune_cache: RuneCacheArcMut,

    ctx: &Context,
) -> Result<(), ()> {
    let mut pg_client = pg_connect(config, false, ctx).await;

    let network = config.get_bitcoin_network();

    let extended_ctx = ExtendedContext {
        ctx: ctx.clone(),
        network,
        rune_cache: rune_cache.clone(),
    };

    // Cache storing currently added mempool transactions
    let mem_cache = Arc::new(RwLock::new(HashSet::new()));
    mem_cache.write().unwrap().insert("some".to_string());

    if let Err(e) = scan_mempool(&mut pg_client, &config.event_observer, &extended_ctx).await {
        try_error!(ctx, "Failed to scan mempool with rpc: {}", e.to_string());
    }

    // let event_observer_cln = config.event_observer.clone()
    // hiro_system_kit::thread_named("Observer Sidecar Runloop").spawn(move || {
    //     hiro_system_kit::nestable_block_on(async {
    //         start_zeromq_runloop(&config.event_observer, &mut pg_client, &extended_ctx).await
    //     });
    // });

    Ok(())
}

// subscribe to zmq
// pub async fn subscribe_to_new_tx() -> Option<()> {}

// there was a problem with dependencies, especially "bitcoin" package
// ordinals and chainhook both use it, but both of them have different versions of it
// this function was indended to fix the issue
// fn chainhook_tx_to_bitcoin_tx(ch_tx: chainhook_sdk::bitcoin::Transaction) -> bitcoin::Transaction {
//     let input = ch_tx
//         .input
//         .into_iter()
//         .map(|tx_in| bitcoin::TxIn {
//             previous_output: bitcoin::OutPoint {
//                 txid: bitcoin::Txid::from_byte_array(tx_in.previous_output.txid.to_byte_array()),
//                 vout: tx_in.previous_output.vout,
//             },
//             script_sig: bitcoin::ScriptBuf::from_bytes(tx_in.script_sig.to_bytes()),
//             sequence: bitcoin::Sequence(tx_in.sequence.0),
//             witness: bitcoin::Witness::from_slice(&tx_in.witness.to_vec()),
//         })
//         .collect();

//     let output = ch_tx
//         .output
//         .into_iter()
//         .map(|tx_out| bitcoin::TxOut {
//             value: tx_out.value,
//             script_pubkey: bitcoin::ScriptBuf::from_bytes(tx_out.script_pubkey.to_bytes()),
//         })
//         .collect();

//     bitcoin::Transaction {
//         version: ch_tx.version,
//         lock_time: bitcoin::locktime::absolute::LockTime::from_consensus(
//             ch_tx.lock_time.to_consensus_u32(),
//         ),
//         input,
//         output,
//     }
// }

// scan mempool
pub async fn scan_mempool(
    pg_client: &mut Client,
    event_observer_conf: &EventObserverConfig,
    extended_ctx: &ExtendedContext,
) -> Result<(), bitcoincore_rpc::Error> {
    // get all tx_ids from mempool
    let rpc_auth = Auth::UserPass(
        event_observer_conf.bitcoind_rpc_username.clone(),
        event_observer_conf.bitcoind_rpc_password.clone(),
    );
    let rpc = new_rpc_client(&event_observer_conf.bitcoind_rpc_url, rpc_auth)
        .expect("Failed to sart an rpc client.");

    let mut db_tx = pg_client
        .transaction()
        .await
        .expect("Unable to begin bitcoin tx processing pg transaction");
    let mut ledger_entries = Vec::new();

    for tx_id in rpc.get_raw_mempool()? {
        let tx: Option<bitcoin::Transaction> = rpc.get_by_id(&tx_id).ok().into();
        if let Some(tx) = tx {
            let new_entries = parse_mempool_tx(tx, &mut db_tx, extended_ctx).await;
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

fn new_rpc_client(
    url: &str,
    auth: Auth,
) -> Result<bitcoincore_rpc::Client, bitcoincore_rpc::Error> {
    bitcoincore_rpc::Client::new(url, auth)
}

fn new_zmq_socket() -> Socket {
    let context = zmq::Context::new();
    let socket = context.socket(zmq::SUB).unwrap();
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

type RuneCacheArcMut = Arc<Mutex<LruCache<RuneId, DbRune>>>;

// replace config
pub async fn start_zeromq_runloop(
    config: &EventObserverConfig,
    pg_client: &mut Client,
    extended_ctx: &ExtendedContext,
) {
    let BitcoinBlockSignaling::ZeroMQ(ref bitcoind_zmq_url) = config.bitcoin_block_signaling else {
        unreachable!()
    };

    let bitcoind_zmq_url = bitcoind_zmq_url.clone();
    // let bitcoin_config = config.get_bitcoin_config();

    try_info!(
        extended_ctx.ctx,
        "Waiting for ZMQ connection acknowledgment from bitcoind"
    );

    let mut socket = new_zmq_socket();
    assert!(socket.connect(&bitcoind_zmq_url).is_ok());
    try_info!(extended_ctx.ctx, "Waiting for ZMQ messages from bitcoind");

    // read transaction
    loop {
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

        let mut db_tx = pg_client
            .transaction()
            .await
            .expect("Unable to begin bitcoin tx processing pg transaction");
        // db_tx is used here to retrive data from postgres
        let ledger_entries = parse_mempool_tx(tx, &mut db_tx, extended_ctx).await;
        // Commit to db
        let _ =
            pg_insert_mempool_ledger_entries(&ledger_entries, &mut db_tx, &extended_ctx.ctx).await;

        db_tx
            .commit()
            .await
            .expect("Unable to commit pg transaction");
    }
}

async fn parse_mempool_tx(
    tx: bitcoin::Transaction,
    db_tx: &mut Transaction<'_>,
    extended_ctx: &ExtendedContext,
) -> Vec<DBMempoolLedgerEntry> {
    let tx_id = tx.txid().to_string();
    let mut ledger_entries = Vec::new();

    // // Transform to DBLedgerEntry Type
    // get runes
    // -- end_transaction
    // -- apply_cenotaph
    // -- apply_etching
    // -- apply_cenotaph_etching
    // -- apply_mint
    // -- apply_cenotaph_mint
    // -- apply_edict

    let mut output_pointer = None;
    let mut next_event_index: u32 = 0;

    let tx_total_outputs = tx.output.len() as u32;
    // might be reused for minting
    let mut tx_etching = None;

    if let Some(artifact) = Runestone::decipher(&tx) {
        let eligible_outputs = collect_eligible_outputs(&tx);
        let mut input_runes_balances =
            collect_input_rune_balances_pg(&tx.input, db_tx, &extended_ctx.ctx).await;

        match artifact {
            Artifact::Runestone(runestone) => {
                if let Some(new_pointer) = runestone.pointer {
                    output_pointer = Some(new_pointer);
                }
                // rune_id is 0:0 as they haven't been included in any block yet
                // RuneID is block_hight:tx_index_in_block
                // no timestamp
                if let Some(etching) = runestone.etching {
                    ledger_entries.push(DBMempoolLedgerEntry::new_mempool_entry_with_idx_incr(
                        None,
                        UNINCLUDED_RUNE_ID,
                        None,
                        None,
                        None,
                        DbLedgerOperation::Etching,
                        &tx_id,
                        &mut next_event_index,
                    ));
                    tx_etching = Some(etching);
                }

                if let Some(mint_rune_id) = runestone.mint {
                    // CHECK mint rules not checked
                    let terms_amount = get_terms_amount_pg_cached(
                        tx_etching,
                        &mint_rune_id,
                        extended_ctx.rune_cache.clone(),
                        db_tx,
                        &extended_ctx.ctx,
                    )
                    .await;
                    ledger_entries.push(DBMempoolLedgerEntry::new_mempool_entry_with_idx_incr(
                        terms_amount.map(|i| i.0),
                        mint_rune_id,
                        None,
                        None,
                        None,
                        DbLedgerOperation::Mint,
                        &tx_id,
                        &mut next_event_index,
                    ));
                    next_event_index += 1;
                }

                for edict in runestone.edicts.iter() {
                    ledger_entries.extend(collect_edicts(
                        edict,
                        &tx_etching,
                        &mut input_runes_balances,
                        &eligible_outputs,
                        &mut next_event_index,
                        &extended_ctx.network,
                        &tx_id,
                        tx_total_outputs,
                        &extended_ctx.ctx,
                    ));
                }
            }
            Artifact::Cenotaph(cenotaph) => {
                ledger_entries.extend(burn_input(
                    &tx_id,
                    input_runes_balances,
                    &mut next_event_index,
                ));

                // CHECK in the original code they swap ething with cached value
                if let Some(etching) = cenotaph.etching {
                    ledger_entries.push(apply_cenotaph_etching(
                        &RuneId { block: 0, tx: 0 },
                        &tx_id,
                        &mut next_event_index,
                    ));
                }
                if let Some(mint_rune_id) = cenotaph.mint {
                    // CHECK is it possible to check if rune is mintable
                    let terms_amount = get_terms_amount_pg_cached(
                        tx_etching,
                        &mint_rune_id,
                        extended_ctx.rune_cache.clone(),
                        db_tx,
                        &extended_ctx.ctx,
                    )
                    .await;
                    ledger_entries.push(DBMempoolLedgerEntry::new_mempool_entry_with_idx_incr(
                        terms_amount.map(|i| i.0),
                        mint_rune_id.clone(),
                        None,
                        None,
                        None,
                        DbLedgerOperation::Burn,
                        &tx_id,
                        &mut next_event_index,
                    ));
                }
            }
        }
    }
    ledger_entries
}

pub async fn pg_insert_mempool_ledger_entries(
    rows: &Vec<DBMempoolLedgerEntry>,
    db_tx: &mut Transaction<'_>,
    ctx: &Context,
) -> Result<bool, Error> {
    for chunk in rows.chunks(500) {
        let mut arg_num = 1;
        let mut arg_str = String::new();
        let mut params: Vec<&(dyn ToSql + Sync)> = vec![];
        let total_arg_cols = 8;
        for row in chunk.iter() {
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
                    "INSERT INTO ledger
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
                try_error!(ctx, "Error inserting ledger entries: {:?}", e);
                process::exit(1);
            }
        };
    }
    Ok(true)
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
    mut input_runes: HashMap<RuneId, Vec<InputRuneBalance>>,
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
    tx_etching: &Option<Etching>,
    input_runes_balances: &mut HashMap<RuneId, Vec<InputRuneBalance>>,
    eligible_outputs: &HashMap<u32, ScriptBuf>,
    mut next_event_index: &mut u32,
    network: &Network,
    tx_id: &String,
    tx_total_outputs: u32,
    ctx: &Context,
) -> Vec<DBMempoolLedgerEntry> {
    let rune_id = if edict.id.block == 0 && edict.id.tx == 0 {
        let Some(etching) = tx_etching.as_ref() else {
            try_warn!(ctx, "Attempted edict for nonexistent rune 0:0");
            return vec![];
        };
        RuneId { block: 0, tx: 0 }
    } else {
        edict.id
    };

    // Take all the available inputs for the rune we're trying to move.
    let Some(available_inputs) = input_runes_balances.get_mut(&rune_id) else {
        try_info!(ctx, "No unallocated runes remain for edict {}", rune_id);
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
        try_info!(ctx, "No eligible outputs for edict on rune {}", rune_id);
        results.extend(move_rune_balance_to_output(
            None, // This will force a burn.
            &rune_id,
            available_inputs,
            &eligible_outputs,
            edict.amount,
            &mut next_event_index,
            network,
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
                            network,
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
                            network,
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
                    network,
                    tx_id,
                    ctx,
                ));
            }
            _ => {
                try_info!(
                    ctx,
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
                    network,
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
    network: &Network,
    tx_id: &String,
    ctx: &Context,
) -> Vec<DBMempoolLedgerEntry> {
    let mut results = vec![];
    // Who is this balance going to?
    let receiver_address = if let Some(output) = output {
        match outputs.get(&output) {
            Some(script) => match Address::from_script(script, *network) {
                Ok(address) => Some(address.to_string()),
                Err(e) => {
                    try_warn!(ctx, "Unable to decode address for output {}, {}", output, e);
                    None
                }
            },
            None => {
                try_info!(
                    ctx,
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
            ctx,
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
            ctx,
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

async fn get_terms_amount_pg_cached(
    etching: Option<Etching>,
    rune_id: &RuneId,
    rune_cache: RuneCacheArcMut,
    db_tx: &mut Transaction<'_>,
    ctx: &Context,
) -> Option<PgNumericU128> {
    // Id 0:0 is used to mean the rune being etched in this transaction, if any.
    if rune_id.block == 0 && rune_id.tx == 0 {
        return etching.and_then(|e| e.terms.and_then(|t| t.amount).map(PgNumericU128));
    }
    if let Some(cached_rune) = rune_cache.lock().unwrap().get(&rune_id) {
        return cached_rune.terms_amount;
    }
    // Cache miss, look in DB.
    // self.db_cache.flush(db_tx, ctx).await;
    // might be optimized by just requesting terms amount
    let Some(db_rune) = pg_get_rune_by_id(rune_id, db_tx, ctx).await else {
        return None;
    };

    return db_rune.terms_amount;
}
