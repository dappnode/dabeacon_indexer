#!/bin/bash
# Probes a beacon node to find the earliest slot for which it still serves
# state-derived endpoints — i.e. how far back the indexer's backfill can
# reach against this node.
#
# Different proxies in front of beacon nodes selectively gate endpoints
# (some return 403 on `/headers/head`, others on `/states/.../fork`, etc.),
# so this script falls back through several alternatives when looking up
# the head slot and when probing per-slot state availability.
#
# Binary-searches the boundary in O(log N) requests rather than walking back
# slot by slot.
#
# Usage:
#   scripts/probe-state-retention.sh <beacon-url>
#   BEACON_URL=https://beacon.example/ scripts/probe-state-retention.sh
#
# URL may include basic auth (`https://user:pass@host`).

set -euo pipefail

BEACON_URL="${1:-${BEACON_URL:-}}"
if [ -z "$BEACON_URL" ]; then
    echo "Usage: $0 <beacon-url>" >&2
    echo "  or:  BEACON_URL=... $0" >&2
    exit 1
fi
BEACON_URL="${BEACON_URL%/}"

command -v curl >/dev/null || { echo "curl required" >&2; exit 1; }
command -v jq   >/dev/null || { echo "jq required"   >&2; exit 1; }

# State-derived endpoints used to probe per-slot availability. We try each in
# order and accept whichever returns a clean 200/404 — that's the answer.
# Other status codes (403, 5xx) indicate proxy gating or upstream churn and
# we fall through to the next candidate.
STATE_ENDPOINTS=(
    "/eth/v1/beacon/states/%s/finality_checkpoints"
    "/eth/v1/beacon/states/%s/root"
    "/eth/v1/beacon/states/%s/fork"
)

# Head-slot discovery candidates: each is "<path>|<jq selector>". Tried in
# order; first endpoint that returns a usable slot wins.
HEAD_ENDPOINTS=(
    "/eth/v1/beacon/headers/head|.data.header.message.slot"
    "/eth/v1/node/syncing|.data.head_slot"
    "/eth/v2/beacon/blocks/head|.data.message.slot"
)

probe_state() {
    # Echoes 200 / 404 on a clean answer; "unknown" if every state endpoint
    # the proxy exposes refused to give a definitive verdict.
    local slot=$1
    local path code
    for tmpl in "${STATE_ENDPOINTS[@]}"; do
        # shellcheck disable=SC2059
        path=$(printf "$tmpl" "$slot")
        code=$(curl -ksS -m 10 -o /dev/null -w "%{http_code}" \
            "${BEACON_URL}${path}" 2>/dev/null || echo "000")
        case "$code" in
            200|404) echo "$code"; return 0 ;;
        esac
    done
    echo "unknown"
}

discover_head_slot() {
    # Echoes the head slot via whichever endpoint succeeds, or empty on total
    # failure (caller's responsibility to error out).
    local entry path selector body slot
    for entry in "${HEAD_ENDPOINTS[@]}"; do
        path="${entry%|*}"
        selector="${entry##*|}"
        body=$(curl -ksS -m 10 "${BEACON_URL}${path}" 2>/dev/null) || continue
        slot=$(echo "$body" | jq -r "$selector // empty | tonumber" 2>/dev/null || true)
        if [ -n "$slot" ] && [ "$slot" != "null" ]; then
            echo "via ${path}: slot ${slot}" >&2
            echo "$slot"
            return 0
        fi
    done
    return 1
}

echo "Probing ${BEACON_URL%@*}@…"

spec=$(curl -fsS -m 10 "${BEACON_URL}/eth/v1/config/spec") || {
    echo "ERROR: /eth/v1/config/spec is unreachable on this node — can't determine slot/epoch math" >&2
    exit 1
}
slots_per_epoch=$(echo "$spec" | jq -r '.data.SLOTS_PER_EPOCH | tonumber')
seconds_per_slot=$(echo "$spec" | jq -r '.data.SECONDS_PER_SLOT | tonumber')

head_slot=$(discover_head_slot) || {
    echo "ERROR: every head-slot endpoint refused — proxy is locking out:" >&2
    for entry in "${HEAD_ENDPOINTS[@]}"; do
        echo "         ${entry%|*}" >&2
    done
    exit 1
}
head_epoch=$((head_slot / slots_per_epoch))
echo "Head: slot ${head_slot} (epoch ${head_epoch})"

# Head must be served via at least one state endpoint — otherwise the binary
# search has no anchor.
head_code=$(probe_state "$head_slot")
if [ "$head_code" != "200" ]; then
    echo "ERROR: head state returned ${head_code} on every probe endpoint — node or proxy is broken" >&2
    exit 1
fi

# Fast path: does it serve genesis? Then it's fully archive-capable.
if [ "$(probe_state 0)" = "200" ]; then
    total_seconds=$((head_slot * seconds_per_slot))
    total_days=$((total_seconds / 86400))
    echo
    echo "Earliest available slot: 0 (genesis) — fully archive-capable"
    echo "Window: $((head_slot + 1)) slots ≈ ${total_days}d since genesis"
    exit 0
fi

echo "Binary searching for retention boundary…"
lo=0
hi=$head_slot
while [ $lo -lt $hi ]; do
    mid=$(( (lo + hi) / 2 ))
    code=$(probe_state "$mid")
    case "$code" in
        200) hi=$mid ;;
        404) lo=$((mid + 1)) ;;
        *)
            echo "  ! all state endpoints refused at slot ${mid}; treating as unavailable" >&2
            lo=$((mid + 1))
            ;;
    esac
    printf "  probe slot=%-9d -> %s  (range [%d, %d])\n" "$mid" "$code" "$lo" "$hi"
done

earliest=$lo
back_slots=$((head_slot - earliest))
back_seconds=$((back_slots * seconds_per_slot))
back_days=$((back_seconds / 86400))
back_hours=$(( (back_seconds % 86400) / 3600 ))
back_minutes=$(( (back_seconds % 3600) / 60 ))

echo
echo "Earliest available slot: ${earliest} (epoch $((earliest / slots_per_epoch)))"
echo "Retention window:        ${back_slots} slots"
echo "                       ≈ ${back_days}d ${back_hours}h ${back_minutes}m back from head"
