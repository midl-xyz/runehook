CREATE TABLE IF NOT EXISTS mempool_ledger (
    rune_id                 TEXT NOT NULL,
    event_index             BIGINT NOT NULL,
    tx_id                   TEXT NOT NULL,
    output                  BIGINT,
    address                 TEXT,
    receiver_address        TEXT,
    amount                  NUMERIC,
    operation               ledger_operation NOT NULL
);

CREATE INDEX mempool_ledger_address_rune_id_index ON mempool_ledger (address, rune_id);
CREATE INDEX mempool_ledger_tx_id_output_index ON mempool_ledger (tx_id, output);
