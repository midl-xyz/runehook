CREATE TYPE ledger_operation AS ENUM ('etching', 'mint', 'burn', 'send', 'receive');

CREATE TABLE IF NOT EXISTS mempool_ledger (
    rune_id                 TEXT NOT NULL,
    event_index             BIGINT NOT NULL,
    tx_id                   TEXT NOT NULL,
    output                  BIGINT,
    address                 TEXT,
    receiver_address        TEXT,
    amount                  NUMERIC,
    operation               ledger_operation NOT NULL,
);

CREATE INDEX ledger_address_rune_id_index ON ledger (address, rune_id);
CREATE INDEX ledger_tx_id_output_index ON ledger (tx_id, output);
