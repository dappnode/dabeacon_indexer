#!/usr/bin/env bash
#
# Spin up the test postgres in docker, run the dense-vs-sparse
# integration tests against it (plus a beacon node you supply), then
# tear the container back down.
#
# Required env (caller supplies):
#   BEACON_URL        e.g. https://your.beacon.node/
#
# Optional — randomised on each run if unset:
#   TEST_EPOCH        if set, used verbatim. Otherwise a random epoch
#                     is picked from the last ~100 finalized epochs.
#   TEST_VALIDATORS   if set, used verbatim. Otherwise a random subset
#                     of NUM_VALIDATORS proposer indices for that epoch
#                     is picked (proposers are guaranteed-active).
#
# Optional — run config:
#   ITERATIONS        how many times to run the test, default 1. Each
#                     iteration re-randomises TEST_EPOCH /
#                     TEST_VALIDATORS unless those are pinned in env.
#   NUM_VALIDATORS    size of the random validator sample, default 4.
#   TEST_FILTER       which #[ignore] tests to run, default
#                     "integration_tests" (runs both: equivalence + bench).
#                     Use e.g. "dense_sparse_attestation_rows_match" or
#                     "dense_vs_sparse_perf_bench" to run one.
#   KEEP_DB           if "1", don't tear down the test container on exit.
#
# Source the .env.test file (if present) so the caller can drop their
# values there once and forget. See .env.test.example.

set -euo pipefail

cd "$(dirname "$0")/.."

if [[ -f .env.test ]]; then
    # shellcheck disable=SC1091
    set -a
    . .env.test
    set +a
fi

: "${BEACON_URL:?Set BEACON_URL (or put it in .env.test)}"

ITERATIONS="${ITERATIONS:-1}"
# 32 = the upper bound on what /eth/v1/validator/duties/proposer/{epoch}
# returns in a single call (one proposer per slot in the epoch). The
# bench sweeps within this pool; the equivalence test uses the full set.
NUM_VALIDATORS="${NUM_VALIDATORS:-32}"
TEST_FILTER="${TEST_FILTER:-integration_tests}"
DATABASE_URL="postgres://dabeacon:dabeacon@localhost:5433/dabeacon_test"

# Capture user overrides BEFORE any randomisation. If they pinned a
# value in env, every iteration uses it; if they didn't, each iteration
# fetches a fresh random one.
USER_TEST_EPOCH="${TEST_EPOCH:-}"
USER_TEST_VALIDATORS="${TEST_VALIDATORS:-}"

cleanup() {
    if [[ "${KEEP_DB:-0}" != "1" ]]; then
        echo "==> tearing down test db"
        docker compose -f docker-compose.test.yml down --volumes >/dev/null 2>&1 || true
    else
        echo "==> KEEP_DB=1, leaving test db running on localhost:5433"
    fi
}
trap cleanup EXIT

echo "==> bringing up test postgres on localhost:5433"
docker compose -f docker-compose.test.yml up -d --wait

# Ask the beacon node for the slot of the latest finalized header. Used
# as the upper bound when randomising an epoch.
fetch_finalized_epoch() {
    local slot
    slot=$(curl -sf --max-time 5 "$BEACON_URL/eth/v1/beacon/headers/finalized" \
        | jq -r '.data.header.message.slot')
    echo "$((slot / 32))"
}

# Pick a random epoch in [finalized - 100, finalized - 5]. Excluding
# the very latest 5 leaves headroom in case the rewards/state APIs lag
# the head a touch on the chosen node.
random_epoch() {
    local finalized_epoch range_start range_size
    finalized_epoch=$(fetch_finalized_epoch)
    range_size=95
    range_start=$((finalized_epoch - 100))
    echo "$((range_start + RANDOM % range_size))"
}

# Pick NUM_VALIDATORS random proposers for the given epoch. Proposers
# for an epoch are 32 validators chosen uniformly at random from the
# full active set, so this gives us a fresh, guaranteed-active sample
# every run with one cheap API call.
random_validators_for_epoch() {
    local epoch="$1"
    curl -sf --max-time 5 "$BEACON_URL/eth/v1/validator/duties/proposer/$epoch" \
        | jq -r '.data[].validator_index' \
        | sort -R \
        | head -"$NUM_VALIDATORS" \
        | paste -sd ',' -
}

run_one() {
    local iter="$1"
    local epoch validators

    if [[ -n "$USER_TEST_EPOCH" ]]; then
        epoch="$USER_TEST_EPOCH"
    else
        epoch=$(random_epoch)
    fi

    if [[ -n "$USER_TEST_VALIDATORS" ]]; then
        validators="$USER_TEST_VALIDATORS"
    else
        validators=$(random_validators_for_epoch "$epoch")
    fi

    echo
    echo "================================================================"
    echo " iteration $iter / $ITERATIONS"
    echo "    BEACON_URL=$BEACON_URL"
    echo "    TEST_EPOCH=$epoch"
    echo "    TEST_VALIDATORS=$validators"
    echo "    DATABASE_URL=$DATABASE_URL"
    echo "    filter: $TEST_FILTER"
    echo "================================================================"

    BEACON_URL="$BEACON_URL" \
    DATABASE_URL="$DATABASE_URL" \
    TEST_EPOCH="$epoch" \
    TEST_VALIDATORS="$validators" \
    RUST_LOG="${RUST_LOG:-dabeacon_indexer=info}" \
    cargo test --bin dabeacon_indexer "$TEST_FILTER" -- --ignored --nocapture --test-threads=1
}

for i in $(seq 1 "$ITERATIONS"); do
    run_one "$i"
done
