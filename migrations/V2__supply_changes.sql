CREATE TABLE IF NOT EXISTS supply_changes (
    rune_id                 TEXT NOT NULL,
    block_height            NUMERIC NOT NULL,
    minted                  TEXT NOT NULL DEFAULT '0',
    total_mints             TEXT NOT NULL DEFAULT '0',
    burned                  TEXT NOT NULL DEFAULT '0',
    total_burns             TEXT NOT NULL DEFAULT '0',
    total_operations        TEXT NOT NULL DEFAULT '0',
    PRIMARY KEY (rune_id, block_height)
);
