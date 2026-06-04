//! # Weighted-Entropy Selector
//!
//! The HIP dispatch decision, as a pure function over the ready set, the number
//! of execution contexts, and the kernel CSPRNG.
//!
//! The rule has two regimes:
//!
//! * **No competition (`ready <= contexts`).** Every ready lane dispatches.
//!   There is nothing to choose between; all ready pathways progress together,
//!   which is the source of the "parallel pathways" / quantum-like behavior.
//!   Weights are irrelevant here.
//! * **Competition (`ready > contexts`).** Exactly `contexts` lanes are chosen
//!   by weighted random sampling *without replacement*: a lane's chance of being
//!   picked is proportional to its weight. Higher-weight classes (System) are
//!   favored, but no lane is starved deterministically, and the choice is
//!   unpredictable because it is driven by the CSPRNG.
//!
//! When the active profile forces equal weights (Maximum Isolation), every lane
//! is passed in with weight 1, making selection uniform and removing scheduling
//! decisions as an observable timing side channel. That policy lives in the
//! scheduler; this module simply honors whatever weights it is given.

use crate::entropy::Csprng;
use alloc::vec::Vec;
use shared::LaneId;

/// A ready lane and the weight to give it during competition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WeightedLane {
    /// The lane.
    pub lane: LaneId,
    /// Selection weight (already resolved from the lane's class and the active
    /// profile). Must be at least 1.
    pub weight: u32,
}

/// Select which ready lanes to dispatch this pass.
///
/// Returns all lanes when `ready.len() <= contexts`; otherwise returns exactly
/// `contexts` lanes chosen by weighted sampling without replacement using `rng`.
///
/// `contexts` of zero yields an empty selection (no execution contexts to run
/// on). Any lane with weight zero is treated as weight 1 so it can never become
/// permanently unselectable.
#[must_use]
pub fn select(ready: &[WeightedLane], contexts: usize, rng: &mut Csprng) -> Vec<LaneId> {
    if contexts == 0 || ready.is_empty() {
        return Vec::new();
    }

    // No competition: dispatch everything.
    if ready.len() <= contexts {
        return ready.iter().map(|w| w.lane).collect();
    }

    // Competition: weighted sampling without replacement.
    // Work on a local pool of (lane, weight) we can shrink as we pick.
    let mut pool: Vec<(LaneId, u64)> = ready
        .iter()
        .map(|w| (w.lane, u64::from(w.weight.max(1))))
        .collect();

    let mut chosen = Vec::with_capacity(contexts);
    for _ in 0..contexts {
        if pool.is_empty() {
            break;
        }
        let total: u64 = pool.iter().map(|(_, w)| *w).sum();
        // total is always >= pool.len() >= 1 here because every weight >= 1.
        let mut ticket = rng.next_bounded(total);
        let mut picked = 0usize;
        for (i, (_, w)) in pool.iter().enumerate() {
            if ticket < *w {
                picked = i;
                break;
            }
            ticket -= *w;
        }
        let (lane, _) = pool.swap_remove(picked);
        chosen.push(lane);
    }
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn lanes(ids_weights: &[(u64, u32)]) -> Vec<WeightedLane> {
        ids_weights
            .iter()
            .map(|(id, w)| WeightedLane {
                lane: LaneId::new(*id),
                weight: *w,
            })
            .collect()
    }

    #[test]
    fn no_competition_dispatches_all() {
        let mut rng = Csprng::from_seed([0u8; 32]);
        let ready = lanes(&[(1, 1), (2, 5), (3, 3)]);
        let out = select(&ready, 4, &mut rng);
        assert_eq!(out.len(), 3);
        // All present, order may follow input here (no sampling path).
        assert!(out.contains(&LaneId::new(1)));
        assert!(out.contains(&LaneId::new(2)));
        assert!(out.contains(&LaneId::new(3)));
    }

    #[test]
    fn competition_selects_exactly_contexts_without_dupes() {
        let mut rng = Csprng::from_seed([9u8; 32]);
        let ready = lanes(&[(1, 1), (2, 1), (3, 1), (4, 1), (5, 1)]);
        let out = select(&ready, 2, &mut rng);
        assert_eq!(out.len(), 2);
        assert_ne!(out[0], out[1], "no lane selected twice");
    }

    #[test]
    fn zero_contexts_selects_none() {
        let mut rng = Csprng::from_seed([0u8; 32]);
        let ready = lanes(&[(1, 1)]);
        assert!(select(&ready, 0, &mut rng).is_empty());
    }

    #[test]
    fn higher_weight_is_favored_under_competition() {
        // One heavy lane (weight 20) against many light lanes (weight 1).
        // Picking 1 of N each pass, the heavy lane should win far more often
        // than any individual light lane.
        let mut rng = Csprng::from_seed([123u8; 32]);
        let mut ready = vec![WeightedLane {
            lane: LaneId::new(100),
            weight: 20,
        }];
        for id in 0..9 {
            ready.push(WeightedLane {
                lane: LaneId::new(id),
                weight: 1,
            });
        }

        let mut heavy_wins = 0u32;
        let trials = 5000;
        for _ in 0..trials {
            let out = select(&ready, 1, &mut rng);
            if out[0] == LaneId::new(100) {
                heavy_wins += 1;
            }
        }
        // Heavy weight 20 of total 29 => ~69% expected. Light lanes ~3.4% each.
        // Assert the heavy lane dominates well beyond any light lane's share.
        assert!(
            heavy_wins > trials * 55 / 100 && heavy_wins < trials * 80 / 100,
            "heavy lane won {heavy_wins}/{trials}, outside expected band"
        );
    }

    #[test]
    fn weight_zero_treated_as_one() {
        // A weight-0 lane must still be selectable (never permanently starved).
        let mut rng = Csprng::from_seed([5u8; 32]);
        let ready = lanes(&[(1, 0), (2, 0), (3, 0)]);
        let out = select(&ready, 2, &mut rng);
        assert_eq!(out.len(), 2);
    }
}
