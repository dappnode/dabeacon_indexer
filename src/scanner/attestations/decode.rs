//! Pure, fork-specific attestation decoding. No I/O, no DB writes.
//!
//! Both the dense epoch pipeline and the live inclusion scan enter through
//! [`extract_attestation_inclusions`]. Vote correctness is computed only when
//! a `VoteContext` is supplied; live paths pass `None` and accept default-false
//! `VoteMarks`.

use std::collections::{HashMap, HashSet};

use super::super::bits::{decode_bitlist, decode_bitvector};
use super::{AssignedCommittee, AttestationInclusion, VoteContext, VoteMarks, inclusion_delay};
use crate::beacon_client::types::{
    Attestation, AttestationData, Attestations, BlockRoot, Committee, PreElectraAttestation,
    SignedBeaconBlock,
};
use crate::chain;
use crate::error::{Error, Result};

/// Vote correctness relative to the canonical chain in `ctx`. `ctx.block_roots` must
/// contain the attestation's slot — a missing entry is a scanner-internal bug.
pub(super) fn compute_vote_correctness<'a>(
    data: &AttestationData,
    ctx: &'a VoteContext,
) -> Result<(&'a BlockRoot, VoteMarks)> {
    let Some(canonical_head_root) = ctx.block_roots.get(&data.slot) else {
        return Err(Error::InconsistentBeaconData(format!(
            "no canonical block root for attestation slot {} (target epoch {})",
            data.slot, ctx.target_epoch
        )));
    };
    let marks = VoteMarks {
        head_correct: &data.beacon_block_root == canonical_head_root,
        target_correct: data.target.epoch == ctx.target_epoch
            && data.target.root == ctx.target_root,
        source_correct: data.source.epoch == ctx.source_epoch
            && data.source.root == ctx.source_root,
    };
    Ok((canonical_head_root, marks))
}

/// `(slot, committee_index) -> Vec<validator_index>`.
pub(super) fn build_committee_map(committees: &[Committee]) -> HashMap<(u64, u64), Vec<u64>> {
    let mut map = HashMap::new();
    for c in committees {
        let validators: Vec<u64> = c.validators.iter().map(|v| v.0).collect();
        map.insert((c.slot, c.index), validators);
    }
    tracing::trace!(committee_entries = map.len(), "Built committee map");
    map
}

/// Collect attestation inclusions across every block in an epoch into a single map
/// keyed by validator index. Each validator's *earliest* inclusion wins.
pub(super) fn collect_inclusions_from_blocks(
    blocks: &[(u64, Option<SignedBeaconBlock>)],
    committee_map: &HashMap<(u64, u64), Vec<u64>>,
    validators: &HashSet<u64>,
    ctx: &VoteContext,
) -> Result<HashMap<u64, AttestationInclusion>> {
    let mut inclusions: HashMap<u64, AttestationInclusion> = HashMap::new();
    for (slot, block_opt) in blocks {
        if let Some(block) = block_opt {
            extract_attestation_inclusions(
                block,
                *slot,
                ctx.target_epoch,
                committee_map,
                validators,
                Some(ctx),
                &mut inclusions,
            )?;
        }
    }
    Ok(inclusions)
}

/// Extract inclusions from a single block, filtered to `target_epoch`.
/// Dispatches on the block's fork-specific attestation encoding.
pub(super) fn extract_attestation_inclusions(
    block: &SignedBeaconBlock,
    inclusion_slot: u64,
    target_epoch: u64,
    committee_map: &HashMap<(u64, u64), Vec<u64>>,
    validators: &HashSet<u64>,
    ctx: Option<&VoteContext>,
    inclusions: &mut HashMap<u64, AttestationInclusion>,
) -> Result<()> {
    tracing::trace!(
        inclusion_slot,
        attestation_count = block.attestations_len(),
        "Processing attestations from block"
    );

    match block.attestations() {
        Attestations::PreElectra(atts) => {
            for att in atts {
                process_pre_electra_attestation(
                    att,
                    inclusion_slot,
                    target_epoch,
                    committee_map,
                    validators,
                    ctx,
                    inclusions,
                )?;
            }
        }
        Attestations::Electra(atts) => {
            for att in atts {
                process_electra_attestation(
                    att,
                    inclusion_slot,
                    target_epoch,
                    committee_map,
                    validators,
                    ctx,
                    inclusions,
                )?;
            }
        }
    }
    Ok(())
}

fn resolve_pre_electra_committees<'a>(
    att: &PreElectraAttestation,
    committee_map: &'a HashMap<(u64, u64), Vec<u64>>,
    agg_bits_len: usize,
) -> Result<Vec<AssignedCommittee<'a>>> {
    let committee = committee_map
        .get(&(att.data.slot, att.data.index))
        .ok_or_else(|| {
            Error::InconsistentBeaconData(format!(
                "pre-Electra attestation references committee (slot={}, index={}) not present in \
                 the epoch's committee list",
                att.data.slot, att.data.index
            ))
        })?;

    if agg_bits_len != committee.len() {
        return Err(Error::InconsistentBeaconData(format!(
            "pre-Electra aggregation_bits length {} != committee size {} (slot={}, index={})",
            agg_bits_len,
            committee.len(),
            att.data.slot,
            att.data.index,
        )));
    }

    Ok(vec![AssignedCommittee {
        index: att.data.index,
        validators: committee,
        bit_offset: 0,
    }])
}

/// Resolve an Electra attestation into the ordered committees whose concatenated
/// bits occupy `aggregation_bits`. Validates `data.index == 0`, `committee_bits`
/// width, at least one committee selected, and Σ committee sizes == agg_bits.
fn resolve_electra_committees<'a>(
    att: &Attestation,
    committee_map: &'a HashMap<(u64, u64), Vec<u64>>,
    agg_bits_len: usize,
) -> Result<Vec<AssignedCommittee<'a>>> {
    if att.data.index != 0 {
        return Err(Error::InconsistentBeaconData(format!(
            "Electra attestation has data.index={} (must be 0 per EIP-7549), slot={}",
            att.data.index, att.data.slot
        )));
    }

    let committee_bits = decode_bitvector(&att.committee_bits)?;
    let max_committees = chain::max_committees_per_slot() as usize;
    if committee_bits.len() != max_committees {
        return Err(Error::InconsistentBeaconData(format!(
            "Electra committee_bits length {} != MAX_COMMITTEES_PER_SLOT ({max_committees}) at slot {}",
            committee_bits.len(),
            att.data.slot,
        )));
    }

    let mut assigned = Vec::new();
    let mut bit_offset = 0usize;
    for (idx, &set) in committee_bits.iter().enumerate() {
        if !set {
            continue;
        }
        let committee_idx = idx as u64;
        let committee = committee_map
            .get(&(att.data.slot, committee_idx))
            .ok_or_else(|| {
                Error::InconsistentBeaconData(format!(
                    "Electra attestation references committee (slot={}, index={}) not in \
                     epoch's committee list",
                    att.data.slot, committee_idx,
                ))
            })?;
        assigned.push(AssignedCommittee {
            index: committee_idx,
            validators: committee,
            bit_offset,
        });
        bit_offset += committee.len();
    }

    if assigned.is_empty() {
        return Err(Error::InconsistentBeaconData(format!(
            "Electra attestation has empty committee_bits at slot {}",
            att.data.slot,
        )));
    }
    if agg_bits_len != bit_offset {
        return Err(Error::InconsistentBeaconData(format!(
            "Electra aggregation_bits length {} != Σ included committee sizes {} at slot {}",
            agg_bits_len, bit_offset, att.data.slot,
        )));
    }

    Ok(assigned)
}

#[allow(clippy::too_many_arguments)]
fn record_committee_inclusions(
    assigned: &AssignedCommittee<'_>,
    agg_bits: &[bool],
    scan_set: &HashSet<u64>,
    inclusion_slot: u64,
    att_slot: u64,
    delay: u64,
    marks: VoteMarks,
    inclusions: &mut HashMap<u64, AttestationInclusion>,
) {
    for (pos, &validator_index) in assigned.validators.iter().enumerate() {
        let bit_pos = assigned.bit_offset + pos;
        if !scan_set.contains(&validator_index) || !agg_bits[bit_pos] {
            continue;
        }
        if inclusions.contains_key(&validator_index) {
            tracing::trace!(
                validator = validator_index,
                inclusion_slot,
                "Skipping duplicate inclusion (already found earlier)"
            );
            continue;
        }
        tracing::trace!(
            validator = validator_index,
            att_slot,
            committee_idx = assigned.index,
            pos_in_committee = pos,
            bit_pos,
            inclusion_slot,
            delay,
            head_correct = marks.head_correct,
            target_correct = marks.target_correct,
            source_correct = marks.source_correct,
            "Found attestation inclusion"
        );
        inclusions.insert(
            validator_index,
            AttestationInclusion {
                inclusion_slot,
                inclusion_delay: delay,
                head_correct: marks.head_correct,
                target_correct: marks.target_correct,
                source_correct: marks.source_correct,
            },
        );
    }
}

fn process_resolved_attestation(
    data: &AttestationData,
    agg_bits: &[bool],
    committees: &[AssignedCommittee<'_>],
    inclusion_slot: u64,
    scan_set: &HashSet<u64>,
    ctx: Option<&VoteContext>,
    inclusions: &mut HashMap<u64, AttestationInclusion>,
) -> Result<()> {
    let delay = inclusion_delay(inclusion_slot, data.slot)?;
    let (canonical_head_root, marks) = match ctx {
        Some(ctx) => {
            let (root, marks) = compute_vote_correctness(data, ctx)?;
            (Some(root), marks)
        }
        None => (None, VoteMarks::default()),
    };

    tracing::trace!(
        att_slot = data.slot,
        committees = committees.len(),
        agg_bits_len = agg_bits.len(),
        bits_set = agg_bits.iter().filter(|&&b| b).count(),
        head_correct = marks.head_correct,
        target_correct = marks.target_correct,
        source_correct = marks.source_correct,
        att_head_root = %data.beacon_block_root,
        canonical_head_root = %canonical_head_root.map(|r| r.as_str()).unwrap_or("NONE"),
        "Processing attestation"
    );

    for assigned in committees {
        record_committee_inclusions(
            assigned,
            agg_bits,
            scan_set,
            inclusion_slot,
            data.slot,
            delay,
            marks,
            inclusions,
        );
    }
    Ok(())
}

fn process_pre_electra_attestation(
    att: &PreElectraAttestation,
    inclusion_slot: u64,
    target_epoch: u64,
    committee_map: &HashMap<(u64, u64), Vec<u64>>,
    validators: &HashSet<u64>,
    ctx: Option<&VoteContext>,
    inclusions: &mut HashMap<u64, AttestationInclusion>,
) -> Result<()> {
    if att.data.slot / chain::slots_per_epoch() != target_epoch {
        return Ok(());
    }
    let agg_bits = decode_bitlist(&att.aggregation_bits)?;
    let committees = resolve_pre_electra_committees(att, committee_map, agg_bits.len())?;
    process_resolved_attestation(
        &att.data,
        &agg_bits,
        &committees,
        inclusion_slot,
        validators,
        ctx,
        inclusions,
    )
}

fn process_electra_attestation(
    att: &Attestation,
    inclusion_slot: u64,
    target_epoch: u64,
    committee_map: &HashMap<(u64, u64), Vec<u64>>,
    validators: &HashSet<u64>,
    ctx: Option<&VoteContext>,
    inclusions: &mut HashMap<u64, AttestationInclusion>,
) -> Result<()> {
    if att.data.slot / chain::slots_per_epoch() != target_epoch {
        return Ok(());
    }
    let agg_bits = decode_bitlist(&att.aggregation_bits)?;
    let committees = resolve_electra_committees(att, committee_map, agg_bits.len())?;
    process_resolved_attestation(
        &att.data,
        &agg_bits,
        &committees,
        inclusion_slot,
        validators,
        ctx,
        inclusions,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::beacon_client::types::{Checkpoint, Committee, Root, StringU64};

    fn zero_root() -> Root {
        Root::parse(&format!("0x{}", "0".repeat(64))).unwrap()
    }

    fn ones_root() -> Root {
        Root::parse(&format!("0x{}", "1".repeat(64))).unwrap()
    }

    fn twos_root() -> Root {
        Root::parse(&format!("0x{}", "2".repeat(64))).unwrap()
    }

    fn make_att_data(slot: u64, head: Root, target: Root, source: Root) -> AttestationData {
        AttestationData {
            slot,
            index: 0,
            beacon_block_root: head,
            source: Checkpoint {
                epoch: 2,
                root: source,
            },
            target: Checkpoint {
                epoch: 3,
                root: target,
            },
        }
    }

    fn make_ctx(target: Root, source: Root, canonical_head_at_100: Root) -> VoteContext {
        let mut block_roots = HashMap::new();
        block_roots.insert(100, canonical_head_at_100);
        VoteContext {
            block_roots,
            target_root: target,
            target_epoch: 3,
            source_epoch: 2,
            source_root: source,
        }
    }

    #[test]
    fn vote_correctness_all_correct() {
        let head_val = zero_root();
        let target_val = ones_root();
        let source_val = twos_root();
        let data = make_att_data(
            100,
            head_val.clone(),
            target_val.clone(),
            source_val.clone(),
        );
        let ctx = make_ctx(target_val, source_val, head_val);
        let (_, marks) = compute_vote_correctness(&data, &ctx).unwrap();
        assert!(marks.head_correct);
        assert!(marks.target_correct);
        assert!(marks.source_correct);
    }

    #[test]
    fn vote_correctness_wrong_head_only() {
        let data = make_att_data(100, zero_root(), ones_root(), twos_root());
        let ctx = make_ctx(ones_root(), twos_root(), ones_root());
        let (_, marks) = compute_vote_correctness(&data, &ctx).unwrap();
        assert!(!marks.head_correct);
        assert!(marks.target_correct);
        assert!(marks.source_correct);
    }

    #[test]
    fn vote_correctness_missing_canonical_head_slot_errors() {
        let data = make_att_data(42, zero_root(), ones_root(), twos_root());
        let ctx = make_ctx(ones_root(), twos_root(), zero_root());
        assert!(matches!(
            compute_vote_correctness(&data, &ctx),
            Err(Error::InconsistentBeaconData(_))
        ));
    }

    fn committee(slot: u64, index: u64, vals: &[u64]) -> Committee {
        Committee {
            slot,
            index,
            validators: vals.iter().copied().map(StringU64).collect(),
        }
    }

    #[test]
    fn committee_map_keys_by_slot_and_index() {
        let committees = vec![
            committee(100, 0, &[10, 20]),
            committee(100, 1, &[30, 40, 50]),
            committee(101, 0, &[60]),
        ];
        let map = build_committee_map(&committees);
        assert_eq!(map.len(), 3);
        assert_eq!(map[&(100, 0)], vec![10, 20]);
        assert_eq!(map[&(100, 1)], vec![30, 40, 50]);
        assert_eq!(map[&(101, 0)], vec![60]);
    }

    fn make_pre_electra(slot: u64, index: u64, aggregation_bits: &str) -> PreElectraAttestation {
        PreElectraAttestation {
            aggregation_bits: aggregation_bits.into(),
            data: AttestationData {
                index,
                ..make_att_data(slot, zero_root(), zero_root(), zero_root())
            },
            signature: "0x00".into(),
        }
    }

    #[test]
    fn pre_electra_includes_validators_whose_bits_are_set() {
        let mut committee_map = HashMap::new();
        committee_map.insert((100, 0), vec![10u64, 20, 30]);
        let validators: HashSet<u64> = [10, 20, 30].into_iter().collect();

        let att = make_pre_electra(100, 0, "0x0D"); // [true, false, true]
        let mut inclusions = HashMap::new();

        process_pre_electra_attestation(
            &att,
            101,
            100 / chain::slots_per_epoch(),
            &committee_map,
            &validators,
            None,
            &mut inclusions,
        )
        .unwrap();

        assert_eq!(inclusions.len(), 2);
        assert!(inclusions.contains_key(&10));
        assert!(inclusions.contains_key(&30));
        assert_eq!(inclusions[&10].inclusion_slot, 101);
        assert_eq!(inclusions[&10].inclusion_delay, 1);
    }

    #[test]
    fn pre_electra_skips_validators_not_in_scan_set() {
        let mut committee_map = HashMap::new();
        committee_map.insert((100, 0), vec![10u64, 20, 30]);
        let validators: HashSet<u64> = [10].into_iter().collect();

        let att = make_pre_electra(100, 0, "0x0F"); // [true, true, true]
        let mut inclusions = HashMap::new();

        process_pre_electra_attestation(
            &att,
            101,
            100 / chain::slots_per_epoch(),
            &committee_map,
            &validators,
            None,
            &mut inclusions,
        )
        .unwrap();

        assert_eq!(inclusions.len(), 1);
        assert!(inclusions.contains_key(&10));
    }

    #[test]
    fn pre_electra_wrong_epoch_is_skipped_without_error() {
        // Malformed bits intentionally: the early epoch-mismatch skip must return
        // before decoding them.
        let committee_map: HashMap<(u64, u64), Vec<u64>> = HashMap::new();
        let validators: HashSet<u64> = [10].into_iter().collect();
        let att = make_pre_electra(100, 0, "0xzz"); // malformed, but not decoded
        let mut inclusions = HashMap::new();

        process_pre_electra_attestation(
            &att,
            101,
            /*target_epoch*/ 999,
            &committee_map,
            &validators,
            None,
            &mut inclusions,
        )
        .unwrap();
        assert!(inclusions.is_empty());
    }

    #[test]
    fn pre_electra_unknown_committee_errors() {
        let committee_map: HashMap<(u64, u64), Vec<u64>> = HashMap::new();
        let validators: HashSet<u64> = [10].into_iter().collect();
        let att = make_pre_electra(100, 0, "0x03");
        let mut inclusions = HashMap::new();

        let err = process_pre_electra_attestation(
            &att,
            101,
            100 / chain::slots_per_epoch(),
            &committee_map,
            &validators,
            None,
            &mut inclusions,
        )
        .unwrap_err();
        assert!(matches!(err, Error::InconsistentBeaconData(_)));
    }

    #[test]
    fn pre_electra_agg_bits_size_mismatch_errors() {
        // Committee has 3 validators but bits declare length 1.
        let mut committee_map = HashMap::new();
        committee_map.insert((100, 0), vec![10u64, 20, 30]);
        let validators: HashSet<u64> = [10].into_iter().collect();
        let att = make_pre_electra(100, 0, "0x03"); // length 1
        let mut inclusions = HashMap::new();

        let err = process_pre_electra_attestation(
            &att,
            101,
            100 / chain::slots_per_epoch(),
            &committee_map,
            &validators,
            None,
            &mut inclusions,
        )
        .unwrap_err();
        assert!(matches!(err, Error::InconsistentBeaconData(_)));
    }

    #[test]
    fn pre_electra_inclusion_before_att_slot_errors() {
        let mut committee_map = HashMap::new();
        committee_map.insert((100, 0), vec![10u64]);
        let validators: HashSet<u64> = [10].into_iter().collect();
        let att = make_pre_electra(100, 0, "0x03");
        let mut inclusions = HashMap::new();

        let err = process_pre_electra_attestation(
            &att,
            /*inclusion_slot*/ 99, // before data.slot=100
            100 / chain::slots_per_epoch(),
            &committee_map,
            &validators,
            None,
            &mut inclusions,
        )
        .unwrap_err();
        assert!(matches!(err, Error::InconsistentBeaconData(_)));
    }

    #[test]
    fn pre_electra_duplicate_inclusion_keeps_earliest() {
        let mut committee_map = HashMap::new();
        committee_map.insert((100, 0), vec![10u64]);
        let validators: HashSet<u64> = [10].into_iter().collect();
        let att = make_pre_electra(100, 0, "0x03");
        let mut inclusions = HashMap::new();

        process_pre_electra_attestation(
            &att,
            101,
            100 / chain::slots_per_epoch(),
            &committee_map,
            &validators,
            None,
            &mut inclusions,
        )
        .unwrap();
        process_pre_electra_attestation(
            &att,
            110,
            100 / chain::slots_per_epoch(),
            &committee_map,
            &validators,
            None,
            &mut inclusions,
        )
        .unwrap();

        assert_eq!(inclusions.len(), 1);
        assert_eq!(inclusions[&10].inclusion_slot, 101);
    }

    fn make_electra(slot: u64, aggregation_bits: &str, committee_bits: &str) -> Attestation {
        Attestation {
            aggregation_bits: aggregation_bits.into(),
            committee_bits: committee_bits.into(),
            data: make_att_data(slot, zero_root(), zero_root(), zero_root()),
            signature: "0x00".into(),
        }
    }

    /// 64-bit committee-bits vector with bits `set` flipped.
    fn committee_bits_hex(set: &[u64]) -> String {
        let mut bytes = [0u8; 8];
        for &b in set {
            let byte = (b / 8) as usize;
            let bit = (b % 8) as u8;
            bytes[byte] |= 1 << bit;
        }
        format!("0x{}", hex::encode(bytes))
    }

    #[test]
    fn electra_spans_multiple_committees_by_offset() {
        let mut committee_map = HashMap::new();
        committee_map.insert((100, 1), vec![10u64, 20, 30]);
        committee_map.insert((100, 3), vec![40u64, 50, 60]);
        let validators: HashSet<u64> = [10, 30, 40, 60].into_iter().collect();

        // [T, F, T, F, F, T] + sentinel at bit 6 → 0x65. Σ committee sizes = 6.
        let att = make_electra(100, "0x65", &committee_bits_hex(&[1, 3]));
        let mut inclusions = HashMap::new();

        process_electra_attestation(
            &att,
            101,
            100 / chain::slots_per_epoch(),
            &committee_map,
            &validators,
            None,
            &mut inclusions,
        )
        .unwrap();

        assert_eq!(inclusions.len(), 3);
        assert!(inclusions.contains_key(&10));
        assert!(inclusions.contains_key(&30));
        assert!(inclusions.contains_key(&60));
    }

    #[test]
    fn electra_unknown_committee_errors() {
        let mut committee_map = HashMap::new();
        committee_map.insert((100, 1), vec![10u64, 20, 30]);
        // committee 3 missing
        let validators: HashSet<u64> = [10].into_iter().collect();

        let att = make_electra(100, "0x65", &committee_bits_hex(&[1, 3]));
        let mut inclusions = HashMap::new();

        let err = process_electra_attestation(
            &att,
            101,
            100 / chain::slots_per_epoch(),
            &committee_map,
            &validators,
            None,
            &mut inclusions,
        )
        .unwrap_err();
        assert!(matches!(err, Error::InconsistentBeaconData(_)));
    }

    #[test]
    fn electra_non_zero_data_index_errors() {
        let mut committee_map = HashMap::new();
        committee_map.insert((100, 1), vec![10u64]);
        let validators: HashSet<u64> = [10].into_iter().collect();
        let mut att = make_electra(100, "0x03", &committee_bits_hex(&[1]));
        att.data.index = 5; // EIP-7549 requires 0
        let mut inclusions = HashMap::new();

        let err = process_electra_attestation(
            &att,
            101,
            100 / chain::slots_per_epoch(),
            &committee_map,
            &validators,
            None,
            &mut inclusions,
        )
        .unwrap_err();
        assert!(matches!(err, Error::InconsistentBeaconData(_)));
    }

    #[test]
    fn electra_committee_bits_wrong_length_errors() {
        let mut committee_map = HashMap::new();
        committee_map.insert((100, 1), vec![10u64]);
        let validators: HashSet<u64> = [10].into_iter().collect();
        // 4 bytes = 32 bits, not 64.
        let att = make_electra(100, "0x03", "0x02000000");
        let mut inclusions = HashMap::new();

        let err = process_electra_attestation(
            &att,
            101,
            100 / chain::slots_per_epoch(),
            &committee_map,
            &validators,
            None,
            &mut inclusions,
        )
        .unwrap_err();
        assert!(matches!(err, Error::InconsistentBeaconData(_)));
    }

    #[test]
    fn electra_empty_committee_bits_errors() {
        let mut committee_map = HashMap::new();
        committee_map.insert((100, 1), vec![10u64]);
        let validators: HashSet<u64> = [10].into_iter().collect();
        // All-zero committee bits (no committees selected) is a spec violation.
        let att = make_electra(100, "0x03", &committee_bits_hex(&[]));
        let mut inclusions = HashMap::new();

        let err = process_electra_attestation(
            &att,
            101,
            100 / chain::slots_per_epoch(),
            &committee_map,
            &validators,
            None,
            &mut inclusions,
        )
        .unwrap_err();
        assert!(matches!(err, Error::InconsistentBeaconData(_)));
    }

    #[test]
    fn electra_agg_bits_size_mismatch_errors() {
        // Committees 1 and 3 each have 3 validators → expected 6 agg bits.
        // Passing a 1-bit aggregation_bits should trip the sum check.
        let mut committee_map = HashMap::new();
        committee_map.insert((100, 1), vec![10u64, 20, 30]);
        committee_map.insert((100, 3), vec![40u64, 50, 60]);
        let validators: HashSet<u64> = [10].into_iter().collect();

        let att = make_electra(100, "0x03", &committee_bits_hex(&[1, 3]));
        let mut inclusions = HashMap::new();

        let err = process_electra_attestation(
            &att,
            101,
            100 / chain::slots_per_epoch(),
            &committee_map,
            &validators,
            None,
            &mut inclusions,
        )
        .unwrap_err();
        assert!(matches!(err, Error::InconsistentBeaconData(_)));
    }

    #[test]
    fn electra_wrong_epoch_is_skipped_without_error() {
        // Same short-circuit logic as pre-Electra: mismatched epoch returns Ok(()).
        let committee_map: HashMap<(u64, u64), Vec<u64>> = HashMap::new();
        let validators: HashSet<u64> = [10].into_iter().collect();
        let att = make_electra(100, "0xzz", "0xzz"); // malformed — unused in wrong-epoch path
        let mut inclusions = HashMap::new();

        process_electra_attestation(
            &att,
            101,
            /*target_epoch*/ 999,
            &committee_map,
            &validators,
            None,
            &mut inclusions,
        )
        .unwrap();
        assert!(inclusions.is_empty());
    }
}
