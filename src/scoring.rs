//! Dice scoring for Bones (5 dice) and Farkle (6 dice).
//!
//! Shared singles:
//! - 1 → 100, 5 → 50
//!
//! Bones:
//! - three 1s → 1000; five 1s → 2000
//! - three of a kind → face × 100
//! - four of a kind → face × 200 (four 1s stay 1000)
//! - five of a kind (faces 2–6) → automatic win
//!
//! Farkle:
//! - 3× 1s = 1000; other 3× = face × 100
//! - 4× / 5× / 6× = face × 200 / 300 / 400 (1s: 2000 / 3000 / 4000)
//! - three pairs / straight 1–6 = 1500; two triplets = 2500

use crate::protocol::GameMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScoreOutcome {
    pub points: u32,
    pub auto_win: bool,
}

fn face_counts(dice: &[u8]) -> Option<[u8; 7]> {
    if dice.iter().any(|&d| !(1..=6).contains(&d)) {
        return None;
    }
    let mut counts = [0u8; 7];
    for &d in dice {
        counts[d as usize] += 1;
    }
    Some(counts)
}

fn is_straight(counts: &[u8; 7]) -> bool {
    (1..=6).all(|face| counts[face] == 1)
}

fn is_three_pairs(counts: &[u8; 7]) -> bool {
    (1..=6).filter(|&face| counts[face] == 2).count() == 3
        && (1..=6).all(|face| counts[face] == 0 || counts[face] == 2)
}

fn is_two_triplets(counts: &[u8; 7]) -> bool {
    (1..=6).filter(|&face| counts[face] == 3).count() == 2
        && (1..=6).all(|face| counts[face] == 0 || counts[face] == 3)
}

fn farkle_special(counts: &[u8; 7], len: usize) -> Option<ScoreOutcome> {
    if len != 6 {
        return None;
    }
    if is_straight(counts) || is_three_pairs(counts) {
        return Some(ScoreOutcome {
            points: 1500,
            auto_win: false,
        });
    }
    if is_two_triplets(counts) {
        return Some(ScoreOutcome {
            points: 2500,
            auto_win: false,
        });
    }
    None
}

fn three_of_a_kind_value(face: u8) -> u32 {
    if face == 1 {
        1000
    } else {
        (face as u32) * 100
    }
}

/// Points for n-of-a-kind (n ≥ 3). Farkle: face×100×(n-2), with 1s from a 1000 base.
/// Bones: only used for 3× (4× / 5× handled separately).
fn kind_points(face: u8, count: u8) -> u32 {
    let n = count.min(6);
    if n < 3 {
        return 0;
    }
    if face == 1 {
        1000 * u32::from(n - 2)
    } else {
        (face as u32) * 100 * u32::from(n - 2)
    }
}

fn score_n_of_a_kind(mode: GameMode, counts: &[u8; 7]) -> Option<ScoreOutcome> {
    if mode == GameMode::Bones {
        for face in 1..=6 {
            if counts[face] == 5 {
                if face == 1 {
                    return Some(ScoreOutcome {
                        points: 2000,
                        auto_win: false,
                    });
                }
                return Some(ScoreOutcome {
                    points: 0,
                    auto_win: true,
                });
            }
        }
    }

    let mut points = 0u32;
    for face in 1..=6 {
        let mut c = counts[face];
        if c == 0 {
            continue;
        }

        if mode == GameMode::Farkle {
            if c >= 3 {
                let taken = c.min(6);
                points += kind_points(face as u8, taken);
                c -= taken;
            }
        } else if c >= 4 {
            // Bones: 4× = face × 200; four 1s keep the 1000 three-1s floor.
            points += if face == 1 {
                1000
            } else {
                (face as u32) * 200
            };
            c -= 4;
        } else if c >= 3 {
            points += three_of_a_kind_value(face as u8);
            c -= 3;
        }

        match face {
            1 => points += (c as u32) * 100,
            5 => points += (c as u32) * 50,
            _ if c > 0 => return None,
            _ => {}
        }
    }

    if points == 0 {
        None
    } else {
        Some(ScoreOutcome {
            points,
            auto_win: false,
        })
    }
}

/// Score a multiset of die faces. Returns `None` if any die cannot be used
/// in a scoring combination.
pub fn score_dice(mode: GameMode, dice: &[u8]) -> Option<ScoreOutcome> {
    if dice.is_empty() {
        return None;
    }
    let counts = face_counts(dice)?;
    if mode == GameMode::Farkle {
        if let Some(special) = farkle_special(&counts, dice.len()) {
            return Some(special);
        }
    }
    score_n_of_a_kind(mode, &counts)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldScore {
    pub points: u32,
    pub auto_win: bool,
    pub used: Vec<usize>,
}

/// Score only complete combinations in a selection. Dead leftover faces
/// (e.g. a lone 6, or a 2 next to three 6s) are ignored, not rejected.
pub fn score_held(mode: GameMode, dice: &[u8], selected: &[usize]) -> Option<HeldScore> {
    if selected.is_empty() {
        return None;
    }
    let mut seen = vec![false; dice.len()];
    let mut by_face: [Vec<usize>; 7] = Default::default();
    for &i in selected {
        if i >= dice.len() || seen[i] {
            return None;
        }
        let face = dice[i];
        if !(1..=6).contains(&face) {
            return None;
        }
        seen[i] = true;
        by_face[face as usize].push(i);
    }

    let mut counts = [0u8; 7];
    let mut used_all = Vec::with_capacity(selected.len());
    for face in 1..=6 {
        counts[face] = by_face[face].len() as u8;
        used_all.extend(by_face[face].iter().copied());
    }

    if mode == GameMode::Farkle {
        if let Some(special) = farkle_special(&counts, selected.len()) {
            return Some(HeldScore {
                points: special.points,
                auto_win: false,
                used: used_all,
            });
        }
    }

    if mode == GameMode::Bones {
        for face in 1..=6 {
            if by_face[face].len() == 5 {
                if face == 1 {
                    return Some(HeldScore {
                        points: 2000,
                        auto_win: false,
                        used: by_face[1].clone(),
                    });
                }
                return Some(HeldScore {
                    points: 0,
                    auto_win: true,
                    used: by_face[face].clone(),
                });
            }
        }
    }

    let mut points = 0u32;
    let mut used = Vec::new();
    for face in 1..=6 {
        let idxs = &by_face[face];
        let mut c = idxs.len();
        if c == 0 {
            continue;
        }
        let mut take = 0usize;
        if mode == GameMode::Farkle {
            if c >= 3 {
                let taken = c.min(6);
                points += kind_points(face as u8, taken as u8);
                take += taken;
                c -= taken;
            }
        } else if c >= 4 {
            points += if face == 1 {
                1000
            } else {
                (face as u32) * 200
            };
            take += 4;
            c -= 4;
        } else if c >= 3 {
            points += three_of_a_kind_value(face as u8);
            take += 3;
            c -= 3;
        }
        match face {
            1 => {
                points += (c as u32) * 100;
                take += c;
            }
            5 => {
                points += (c as u32) * 50;
                take += c;
            }
            _ => {}
        }
        used.extend(idxs.iter().copied().take(take));
    }

    if points == 0 {
        None
    } else {
        Some(HeldScore {
            points,
            auto_win: false,
            used,
        })
    }
}

pub fn has_any_score(mode: GameMode, dice: &[u8]) -> bool {
    if dice.is_empty() {
        return false;
    }
    let Some(counts) = face_counts(dice) else {
        return false;
    };
    if mode == GameMode::Farkle && farkle_special(&counts, dice.len()).is_some() {
        return true;
    }
    if counts[1] > 0 || counts[5] > 0 {
        return true;
    }
    (2..=6).any(|face| counts[face] >= 3)
}

fn full_hand_special(mode: GameMode, dice: &[u8]) -> bool {
    if mode != GameMode::Farkle {
        return false;
    }
    face_counts(dice).is_some_and(|counts| farkle_special(&counts, dice.len()).is_some())
}

/// A die can be kept if it is a 1 or 5, its face has at least three showing,
/// or (Farkle) the full roll is a special combination that includes it.
pub fn can_keep_die(mode: GameMode, dice: &[u8], index: usize) -> bool {
    let Some(&face) = dice.get(index) else {
        return false;
    };
    if face == 1 || face == 5 {
        return true;
    }
    if !(2..=6).contains(&face) {
        return false;
    }
    if dice.iter().filter(|&&d| d == face).count() >= 3 {
        return true;
    }
    full_hand_special(mode, dice)
}

/// True if some non-empty selection scores and, when `on_board`, keeps
/// `score + turn_points + kept` at or under `win_score` (auto-wins always count).
pub fn has_playable_keep(
    mode: GameMode,
    dice: &[u8],
    score: u32,
    turn_points: u32,
    on_board: bool,
    win_score: u32,
) -> bool {
    let n = dice.len();
    if n == 0 || n > 16 {
        return false;
    }
    for mask in 1usize..(1usize << n) {
        let indices: Vec<usize> = (0..n).filter(|i| (mask & (1 << i)) != 0).collect();
        let Some(held) = score_held(mode, dice, &indices) else {
            continue;
        };
        if held.auto_win {
            return true;
        }
        if held.points == 0 {
            continue;
        }
        if !on_board {
            return true;
        }
        let pending = turn_points.saturating_add(held.points);
        if score.saturating_add(pending) <= win_score {
            return true;
        }
    }
    false
}

#[cfg(test)]
pub fn score_selection(mode: GameMode, dice: &[u8], selected: &[usize]) -> Option<ScoreOutcome> {
    if selected.is_empty() {
        return None;
    }
    let mut seen = vec![false; dice.len()];
    let mut picked = Vec::with_capacity(selected.len());
    for &i in selected {
        if i >= dice.len() || seen[i] {
            return None;
        }
        seen[i] = true;
        picked.push(dice[i]);
    }
    score_dice(mode, &picked)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn singles() {
        assert_eq!(
            score_dice(GameMode::Bones, &[1]).unwrap().points,
            100
        );
        assert_eq!(
            score_dice(GameMode::Bones, &[5]).unwrap().points,
            50
        );
        assert_eq!(
            score_dice(GameMode::Bones, &[1, 5]).unwrap().points,
            150
        );
        assert!(score_dice(GameMode::Bones, &[2]).is_none());
    }

    #[test]
    fn three_ones() {
        assert_eq!(
            score_dice(GameMode::Bones, &[1, 1, 1]).unwrap().points,
            1000
        );
        assert_eq!(
            score_dice(GameMode::Bones, &[1, 1, 1, 1]).unwrap().points,
            1000
        );
        assert_eq!(
            score_dice(GameMode::Bones, &[1, 1, 1, 1, 5])
                .unwrap()
                .points,
            1050
        );
    }

    #[test]
    fn five_ones() {
        let s = score_dice(GameMode::Bones, &[1, 1, 1, 1, 1]).unwrap();
        assert_eq!(s.points, 2000);
        assert!(!s.auto_win);
    }

    #[test]
    fn n_of_a_kind() {
        assert_eq!(
            score_dice(GameMode::Bones, &[2, 2, 2]).unwrap().points,
            200
        );
        assert_eq!(
            score_dice(GameMode::Bones, &[3, 3, 3, 3]).unwrap().points,
            600
        );
        assert_eq!(
            score_dice(GameMode::Bones, &[4, 4, 4, 1]).unwrap().points,
            500
        );
    }

    #[test]
    fn five_of_a_kind_wins_bones() {
        let s = score_dice(GameMode::Bones, &[6, 6, 6, 6, 6]).unwrap();
        assert!(s.auto_win);
    }

    #[test]
    fn five_of_a_kind_scores_farkle() {
        // 5× 6s = face × 300 = 1800
        let s = score_dice(GameMode::Farkle, &[6, 6, 6, 6, 6]).unwrap();
        assert!(!s.auto_win);
        assert_eq!(s.points, 1800);
    }

    #[test]
    fn six_of_a_kind_farkle() {
        // 6× 2s = face × 400 = 800
        let s = score_dice(GameMode::Farkle, &[2, 2, 2, 2, 2, 2]).unwrap();
        assert_eq!(s.points, 800);
        assert!(!s.auto_win);
    }

    #[test]
    fn farkle_ones_scale_from_three_of_a_kind() {
        assert_eq!(
            score_dice(GameMode::Farkle, &[1, 1, 1]).unwrap().points,
            1000
        );
        assert_eq!(
            score_dice(GameMode::Farkle, &[1, 1, 1, 1]).unwrap().points,
            2000
        );
        assert_eq!(
            score_dice(GameMode::Farkle, &[1, 1, 1, 1, 1])
                .unwrap()
                .points,
            3000
        );
        assert_eq!(
            score_dice(GameMode::Farkle, &[1, 1, 1, 1, 1, 1])
                .unwrap()
                .points,
            4000
        );
    }

    #[test]
    fn three_pairs() {
        let s = score_dice(GameMode::Farkle, &[2, 2, 3, 3, 4, 4]).unwrap();
        assert_eq!(s.points, 1500);
        assert!(score_dice(GameMode::Bones, &[2, 2, 3, 3, 4]).is_none());
    }

    #[test]
    fn straight() {
        let s = score_dice(GameMode::Farkle, &[1, 2, 3, 4, 5, 6]).unwrap();
        assert_eq!(s.points, 1500);
    }

    #[test]
    fn two_triplets() {
        let s = score_dice(GameMode::Farkle, &[2, 2, 2, 5, 5, 5]).unwrap();
        assert_eq!(s.points, 2500);
    }

    #[test]
    fn bust_detection() {
        assert!(!has_any_score(GameMode::Bones, &[2, 3, 4, 6, 6]));
        assert!(has_any_score(GameMode::Bones, &[2, 3, 4, 6, 5]));
        assert!(has_any_score(GameMode::Bones, &[2, 2, 2, 3, 4]));
        assert!(has_any_score(
            GameMode::Farkle,
            &[2, 2, 3, 3, 4, 4]
        ));
        assert!(has_any_score(
            GameMode::Farkle,
            &[1, 2, 3, 4, 5, 6]
        ));
    }

    #[test]
    fn selection_rejects_dupes_and_dead() {
        assert!(score_selection(GameMode::Bones, &[1, 2, 5], &[0, 2]).is_some());
        assert!(score_selection(GameMode::Bones, &[1, 2, 5], &[0, 1]).is_none());
        assert!(score_selection(GameMode::Bones, &[1, 2, 5], &[0, 0]).is_none());
    }

    #[test]
    fn keepable_dice_are_ones_fives_or_n_of_a_kind() {
        let dice = [1, 6, 6, 6, 2];
        assert!(can_keep_die(GameMode::Bones, &dice, 0));
        assert!(can_keep_die(GameMode::Bones, &dice, 1));
        assert!(can_keep_die(GameMode::Bones, &dice, 2));
        assert!(can_keep_die(GameMode::Bones, &dice, 3));
        assert!(!can_keep_die(GameMode::Bones, &dice, 4));
        assert!(!can_keep_die(GameMode::Bones, &[2, 3, 4, 6, 6], 0));
        assert!(can_keep_die(GameMode::Bones, &[5, 2, 3, 4, 6], 0));
    }

    #[test]
    fn keepable_farkle_specials() {
        let pairs = [2, 2, 3, 3, 4, 4];
        for i in 0..6 {
            assert!(can_keep_die(GameMode::Farkle, &pairs, i));
        }
        let straight = [1, 2, 3, 4, 5, 6];
        for i in 0..6 {
            assert!(can_keep_die(GameMode::Farkle, &straight, i));
        }
    }

    #[test]
    fn held_ignores_incomplete_and_dead() {
        assert!(score_held(GameMode::Bones, &[6, 6, 2, 3, 4], &[0]).is_none());
        assert!(score_held(GameMode::Bones, &[6, 6, 2, 3, 4], &[0, 1]).is_none());
        assert_eq!(
            score_held(GameMode::Bones, &[6, 6, 6, 2, 3], &[0, 1, 2])
                .unwrap()
                .points,
            600
        );
        assert_eq!(
            score_held(GameMode::Bones, &[6, 6, 6, 2, 3], &[0, 1, 2, 3])
                .unwrap()
                .points,
            600
        );
        assert_eq!(
            score_held(GameMode::Bones, &[6, 6, 6, 2, 3], &[0, 1, 2, 3])
                .unwrap()
                .used
                .len(),
            3
        );
        assert_eq!(
            score_held(GameMode::Bones, &[1, 6, 2], &[0, 1])
                .unwrap()
                .points,
            100
        );
        assert_eq!(
            score_held(GameMode::Bones, &[1, 1, 1, 6], &[0, 1, 2])
                .unwrap()
                .points,
            1000
        );
    }

    #[test]
    fn held_farkle_three_pairs() {
        let dice = [2, 2, 3, 3, 4, 4];
        let held = score_held(GameMode::Farkle, &dice, &[0, 1, 2, 3, 4, 5]).unwrap();
        assert_eq!(held.points, 1500);
        assert_eq!(held.used.len(), 6);
    }

    #[test]
    fn playable_keep_respects_win_cap() {
        let dice = [2, 4, 5, 3, 3, 3];
        // 9,800 + 300 from three 3s would overshoot, but the lone 5 still fits.
        assert!(has_playable_keep(
            GameMode::Farkle,
            &dice,
            9_800,
            0,
            true,
            10_000
        ));
        // Only three 3s score — every keep overshoots.
        assert!(!has_playable_keep(
            GameMode::Farkle,
            &[2, 4, 3, 3, 3, 6],
            9_800,
            0,
            true,
            10_000
        ));
        // Off the board, any scoring keep is playable.
        assert!(has_playable_keep(
            GameMode::Farkle,
            &[2, 4, 3, 3, 3, 6],
            0,
            0,
            false,
            10_000
        ));
    }
}
