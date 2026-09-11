-- Consolidated undeployed collection/recovery schema. Finality alone does not
-- establish completion; existing rows remain eligible for archive verification.
CREATE TABLE completed_scans (
    validator_index BIGINT NOT NULL REFERENCES validators(validator_index),
    epoch BIGINT NOT NULL,
    PRIMARY KEY (validator_index, epoch)
);

-- Raw endpoint responses retain dependent_root and safety metadata. The anchor
-- is checked through canonical block headers before any reuse, including after
-- a restart. Keeping these small inputs avoids requiring pruned beacon states.
CREATE TABLE beacon_inputs (
    input_key TEXT PRIMARY KEY,
    epoch BIGINT NOT NULL,
    anchor_slot BIGINT NOT NULL,
    anchor_root TEXT NOT NULL,
    response TEXT NOT NULL
);
CREATE INDEX beacon_inputs_epoch_idx ON beacon_inputs (epoch);

-- Recent canonical evidence is separate from finalized archive scan completion.
ALTER TABLE attestation_duties ADD COLUMN inclusion_known BOOLEAN NOT NULL DEFAULT FALSE;
UPDATE attestation_duties a SET inclusion_known=TRUE WHERE EXISTS
    (SELECT 1 FROM completed_scans c WHERE c.validator_index=a.validator_index AND c.epoch=a.epoch);

CREATE TABLE live_blocks (
    slot BIGINT PRIMARY KEY,
    root TEXT NOT NULL UNIQUE,
    parent_root TEXT NOT NULL
);

CREATE TABLE live_coverage (
    validator_index BIGINT NOT NULL REFERENCES validators(validator_index),
    slot BIGINT NOT NULL,
    PRIMARY KEY (validator_index, slot)
);

CREATE TABLE live_tracking_start (
    validator_index BIGINT PRIMARY KEY REFERENCES validators(validator_index),
    started_epoch BIGINT NOT NULL
);

CREATE TABLE live_gaps (
    validator_index BIGINT NOT NULL REFERENCES validators(validator_index),
    epoch BIGINT NOT NULL,
    reason TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (validator_index, epoch)
);

CREATE TABLE live_jobs (
    validator_index BIGINT NOT NULL REFERENCES validators(validator_index),
    epoch BIGINT NOT NULL,
    -- Epoch jobs use -1; block jobs use the actual slot.
    slot BIGINT NOT NULL DEFAULT -1,
    component TEXT NOT NULL CHECK (component IN
        ('attestation_rewards', 'proposal_rewards', 'sync_rewards')),
    root TEXT,
    dependency TEXT,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN
        ('pending', 'complete', 'needs_backfill')),
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_error TEXT,
    payload JSONB,
    PRIMARY KEY (validator_index, epoch, slot, component)
);
CREATE INDEX live_jobs_pending ON live_jobs(next_attempt, epoch) WHERE status = 'pending';
