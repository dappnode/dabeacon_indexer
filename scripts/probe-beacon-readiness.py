# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Read-only readiness probe; outputs JSONL without URLs or response error bodies.

Use BEACON_URL or --config. No database access, subscriptions, or node changes.
The default validator is the initial head's proposer; an empty validator request
is never sent. This measures one endpoint, not every instance behind a gateway.
"""

import argparse
import hashlib
import json
import os
import sys
import time
import tomllib
import urllib.error
import urllib.request
from datetime import datetime, timezone
from pathlib import Path


def emit(record):
    record["observed_at"] = datetime.now(timezone.utc).isoformat()
    print(json.dumps(record, sort_keys=True), flush=True)


def request(base, path, body=None):
    started = time.monotonic()
    req = urllib.request.Request(
        base.rstrip("/") + path,
        data=None if body is None else json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=10) as response:
            data = json.load(response)
            status = response.status
    except urllib.error.HTTPError as exc:
        status, data = exc.code, {}
    except (urllib.error.URLError, TimeoutError, OSError, ValueError) as exc:
        status, data = type(exc).__name__, {}
    return {
        "status": status,
        "seconds": round(time.monotonic() - started, 3),
        "execution_optimistic": data.get("execution_optimistic"),
        "finalized": data.get("finalized"),
    }, data


def root_at(base, slot):
    meta, response = request(base, f"/eth/v1/beacon/states/{slot}/root")
    return {**meta, "root": response.get("data", {}).get("root")}


def reward_sample(base, epoch, validator, slots_per_epoch):
    state_slot = (epoch + 2) * slots_per_epoch - 1
    before = root_at(base, state_slot)
    meta, response = request(
        base, f"/eth/v1/beacon/rewards/attestations/{epoch}", [str(validator)]
    )
    after = root_at(base, state_slot)
    rewards = response.get("data", {}).get("total_rewards", [])
    digest = hashlib.sha256(json.dumps(rewards, sort_keys=True).encode()).hexdigest()
    return {
        "kind": "attestation_rewards",
        "epoch": epoch,
        **meta,
        "reward_rows": rewards,
        "reward_digest": digest if meta["status"] == 200 else None,
        "state_slot": state_slot,
        "state_before": before,
        "state_after": after,
        "stable_state_anchor": bool(before["root"])
        and before["root"] == after["root"],
    }


def block_samples(base, context, validator):
    """Compare fixed root calculations at several distances from imported head."""
    for offset in (0, 1, 2, 4):
        slot = context["head_slot"] - offset
        if slot < 0:
            continue
        meta, header = request(base, f"/eth/v1/beacon/headers/{slot}")
        reference = {**context, "offset": offset, "slot": slot}
        if meta["status"] != 200:
            emit({"kind": "block_header", **reference, **meta})
            continue
        root = header["data"]["root"]
        for kind, path, body in (
            ("block_rewards", f"/eth/v1/beacon/rewards/blocks/{root}", None),
            ("sync_rewards", f"/eth/v1/beacon/rewards/sync_committee/{root}", [str(validator)]),
        ):
            meta, response = request(base, path, body)
            emit({"kind": kind, **reference, "block_root": root,
                  **meta, "data": response.get("data")})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, help="TOML file supplying beacon_url")
    parser.add_argument("--validator", type=int, help="Otherwise use the initial head proposer")
    parser.add_argument("--samples", type=int, default=1)
    parser.add_argument("--interval", type=float, default=24)
    args = parser.parse_args()
    if (
        args.samples < 1
        or args.interval < 1
        or (args.validator is not None and args.validator < 0)
    ):
        parser.error("samples and interval must be positive; validator must be nonnegative")
    base = os.environ.get("BEACON_URL")
    if args.config:
        base = base or tomllib.loads(args.config.read_text()).get("beacon_url")
    if not base:
        parser.error("set BEACON_URL or pass --config")

    for name, path in [("version", "/eth/v1/node/version"), ("syncing", "/eth/v1/node/syncing")]:
        meta, data = request(base, path)
        emit({"kind": name, **meta, "data": data.get("data")})
    meta, spec = request(base, "/eth/v1/config/spec")
    if meta["status"] != 200:
        emit({"kind": "spec", **meta})
        return 1
    slots_per_epoch = int(spec["data"]["SLOTS_PER_EPOCH"])
    watched_epoch = None
    validator = args.validator

    for sample in range(args.samples):
        if sample:
            time.sleep(args.interval)
        meta, response = request(base, "/eth/v1/beacon/headers/head")
        if meta["status"] != 200:
            emit({"kind": "head", "sample": sample, **meta})
            continue
        head = response["data"]
        message = head["header"]["message"]
        slot = int(message["slot"])
        epoch = slot // slots_per_epoch
        if validator is None:
            validator = int(message["proposer_index"])
        context = {"sample": sample, "head_slot": slot, "head_root": head["root"]}
        emit({"kind": "head", **context, **meta, "slot_in_epoch": slot % slots_per_epoch})
        meta, finality = request(base, "/eth/v1/beacon/states/head/finality_checkpoints")
        emit({"kind": "finality", **context, **meta, "data": finality.get("data")})
        block_samples(base, context, validator)

        if watched_epoch is None:
            watched_epoch = max(epoch - 1, 0)
            # Compare endpoint capabilities once; do not repeatedly scan old history.
            for lag in range(min(epoch, 4) + 1):
                target = epoch - lag
                for kind, path, body in [
                    (
                        "attester_duties",
                        f"/eth/v1/validator/duties/attester/{target}",
                        [str(validator)],
                    ),
                    (
                        "committees",
                        f"/eth/v1/beacon/states/{target * slots_per_epoch}/committees"
                        f"?epoch={target}&slot={target * slots_per_epoch}",
                        None,
                    ),
                ]:
                    meta, data = request(base, path, body)
                    emit({"kind": kind, **context, "epoch": target, **meta,
                          "rows": len(data.get("data", [])),
                          "dependent_root": data.get("dependent_root")})
                emit({**context, **reward_sample(base, target, validator, slots_per_epoch)})
        else:
            emit({**context, **reward_sample(base, watched_epoch, validator, slots_per_epoch)})
    return 0


if __name__ == "__main__":
    sys.exit(main())
