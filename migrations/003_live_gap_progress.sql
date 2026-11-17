-- Persist the last finality target enumerated for each live validator.
-- Existing tracking rows enumerate their history once after migration.
ALTER TABLE live_tracking_start ADD COLUMN gaps_through_epoch BIGINT;
