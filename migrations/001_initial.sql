-- Tracked validators with per-validator scan progress
CREATE TABLE validators (
    validator_index    BIGINT PRIMARY KEY,
    pubkey             BYTEA NOT NULL UNIQUE,
    activation_epoch   BIGINT NOT NULL,
    exit_epoch         BIGINT,
    -- Per-validator scan watermark: the last epoch fully scanned and committed for this validator.
    -- NULL means never scanned (needs backfill from activation_epoch).
    last_scanned_epoch BIGINT,
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_validators_activation ON validators (activation_epoch);

-- Per-validator per-epoch attestation duty & performance
CREATE TABLE attestation_duties (
    validator_index           BIGINT NOT NULL REFERENCES validators(validator_index),
    epoch                     BIGINT NOT NULL,
    assigned_slot             BIGINT NOT NULL,
    committee_index           INTEGER NOT NULL,
    committee_position        INTEGER NOT NULL,
    included                  BOOLEAN NOT NULL DEFAULT FALSE,
    inclusion_slot            BIGINT,
    inclusion_delay           INTEGER,
    effective_inclusion_delay INTEGER,
    source_correct            BOOLEAN,
    target_correct            BOOLEAN,
    head_correct              BOOLEAN,
    source_reward             BIGINT,
    target_reward             BIGINT,
    head_reward               BIGINT,
    inactivity_penalty        BIGINT DEFAULT 0,
    finalized                 BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (validator_index, epoch)
);

CREATE INDEX idx_att_epoch ON attestation_duties (epoch);
CREATE INDEX idx_att_not_finalized ON attestation_duties (epoch) WHERE finalized = FALSE;
CREATE INDEX idx_att_missed ON attestation_duties (validator_index, epoch) WHERE included = FALSE;

-- Sync committee participation (per-slot granularity)
CREATE TABLE sync_duties (
    validator_index BIGINT NOT NULL REFERENCES validators(validator_index),
    slot            BIGINT NOT NULL,
    participated    BOOLEAN NOT NULL,
    reward          BIGINT,
    missed_block    BOOLEAN NOT NULL DEFAULT FALSE,
    finalized       BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (validator_index, slot)
);

CREATE INDEX idx_sync_slot ON sync_duties (slot);
CREATE INDEX idx_sync_not_finalized ON sync_duties (slot) WHERE finalized = FALSE;

-- Block proposals (only for slots where tracked validators are proposers)
CREATE TABLE block_proposals (
    slot                BIGINT NOT NULL,
    proposer_index      BIGINT NOT NULL REFERENCES validators(validator_index),
    proposed            BOOLEAN NOT NULL,
    reward_total        BIGINT,
    reward_attestations BIGINT,
    reward_sync         BIGINT,
    reward_slashings    BIGINT,
    finalized           BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (slot)
);

CREATE INDEX idx_proposals_proposer ON block_proposals (proposer_index);
CREATE INDEX idx_proposals_missed ON block_proposals (slot) WHERE proposed = FALSE;

-- Instance heartbeat (lightweight, just for multi-instance awareness)
CREATE TABLE instances (
    instance_id UUID PRIMARY KEY,
    started_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    heartbeat   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
