use axum::{Json, extract::State};
use serde::Serialize;

use crate::chain;
use crate::db::api::rewards as db_rewards;
use crate::web::AppState;

#[derive(Serialize)]
struct RewardWindow {
    attestation_reward: i64,
    sync_reward: i64,
    proposal_reward: i64,
    total: i64,
    epochs: i64,
}

#[derive(Serialize)]
struct ValidatorRewards {
    validator_index: i64,
    day_1: RewardWindow,
    day_7: RewardWindow,
    day_30: RewardWindow,
    all_time: RewardWindow,
}

#[derive(Serialize)]
pub(super) struct RewardsResponse {
    validators: Vec<ValidatorRewards>,
    totals: TotalRewards,
    latest_epoch: i64,
}

#[derive(Serialize)]
struct TotalRewards {
    day_1: RewardWindow,
    day_7: RewardWindow,
    day_30: RewardWindow,
    all_time: RewardWindow,
}

fn empty_window() -> RewardWindow {
    RewardWindow {
        attestation_reward: 0,
        sync_reward: 0,
        proposal_reward: 0,
        total: 0,
        epochs: 0,
    }
}

pub(super) async fn get_rewards(State(state): State<AppState>) -> Json<RewardsResponse> {
    let pool = &state.pool;

    let latest_epoch = db_rewards::latest_scanned_epoch(pool).await.unwrap_or(0);
    let validators = db_rewards::list_validator_indices(pool)
        .await
        .unwrap_or_default();

    let epochs_per_day = chain::epochs_per_day() as i64;
    let spe = chain::slots_per_epoch() as i64;
    let cutoff_1d = (latest_epoch - epochs_per_day).max(0);
    let cutoff_7d = (latest_epoch - epochs_per_day * 7).max(0);
    let cutoff_30d = (latest_epoch - epochs_per_day * 30).max(0);

    let att = db_rewards::attestation_reward_windows(pool, cutoff_1d, cutoff_7d, cutoff_30d)
        .await
        .unwrap_or_default();
    let sync =
        db_rewards::sync_reward_windows(pool, cutoff_1d * spe, cutoff_7d * spe, cutoff_30d * spe)
            .await
            .unwrap_or_default();
    let proposal = db_rewards::proposal_reward_windows(
        pool,
        cutoff_1d * spe,
        cutoff_7d * spe,
        cutoff_30d * spe,
    )
    .await
    .unwrap_or_default();

    let mut result_validators = Vec::with_capacity(validators.len());
    let mut t1 = empty_window();
    let mut t7 = empty_window();
    let mut t30 = empty_window();
    let mut tall = empty_window();

    for vi in validators {
        let a = att.get(&vi).copied().unwrap_or_default();
        let s = sync.get(&vi).copied().unwrap_or_default();
        let p = proposal.get(&vi).copied().unwrap_or_default();

        let d1 = RewardWindow {
            attestation_reward: a.d1,
            sync_reward: s.d1,
            proposal_reward: p.d1,
            total: a.d1 + s.d1 + p.d1,
            epochs: a.epochs_d1,
        };
        let d7 = RewardWindow {
            attestation_reward: a.d7,
            sync_reward: s.d7,
            proposal_reward: p.d7,
            total: a.d7 + s.d7 + p.d7,
            epochs: a.epochs_d7,
        };
        let d30 = RewardWindow {
            attestation_reward: a.d30,
            sync_reward: s.d30,
            proposal_reward: p.d30,
            total: a.d30 + s.d30 + p.d30,
            epochs: a.epochs_d30,
        };
        let all = RewardWindow {
            attestation_reward: a.all,
            sync_reward: s.all,
            proposal_reward: p.all,
            total: a.all + s.all + p.all,
            epochs: a.epochs_all,
        };

        sum_windows(&mut t1, &d1);
        sum_windows(&mut t7, &d7);
        sum_windows(&mut t30, &d30);
        sum_windows(&mut tall, &all);

        result_validators.push(ValidatorRewards {
            validator_index: vi,
            day_1: d1,
            day_7: d7,
            day_30: d30,
            all_time: all,
        });
    }

    Json(RewardsResponse {
        validators: result_validators,
        totals: TotalRewards {
            day_1: t1,
            day_7: t7,
            day_30: t30,
            all_time: tall,
        },
        latest_epoch,
    })
}

fn sum_windows(a: &mut RewardWindow, b: &RewardWindow) {
    a.attestation_reward += b.attestation_reward;
    a.sync_reward += b.sync_reward;
    a.proposal_reward += b.proposal_reward;
    a.total += b.total;
    a.epochs = a.epochs.max(b.epochs);
}
