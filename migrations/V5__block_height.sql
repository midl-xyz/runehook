CREATE TABLE IF NOT EXISTS block_height (
    last_scanned_height NUMERIC
);

INSERT INTO block_height (last_scanned_height)
SELECT 0
WHERE NOT EXISTS (SELECT 1 FROM block_height);