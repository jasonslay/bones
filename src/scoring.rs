//! Dice scoring for Bones (5 dice, not 6-dice Farkle).
//!
//! - 1 → 100, 5 → 50
//! - three 1s → 1000; five 1s → 2000
//! - three of a kind → face × 100
//! - four of a kind → face × 1000
//! - five of a kind (faces 2–6) → automatic win

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScoreOutcome {
    pub points: u32,
    pub auto_win: bool,
}

/// Score a multiset of die faces. Returns `None` if any die cannot be used
/// in a scoring combination.
pub fn score_dice(dice: &[u8]) -> Option<ScoreOutcome> {
    if dice.is_empty() {
        return None;
    }
    if dice.iter().any(|&d| !(1..=6).contains(&d)) {
        return None;
    }

    let mut counts = [0u8; 7];
    for &d in dice {
        counts[d as usize] += 1;
    }

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

    let mut points = 0u32;
    for face in 1..=6 {
        let mut c = counts[face];
        if c == 0 {
            continue;
        }

        if c >= 4 {
            points += (face as u32) * 1000;
            c -= 4;
        } else if c >= 3 {
            if face == 1 {
                points += 1000;
            } else {
                points += (face as u32) * 100;
            }
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

pub fn has_any_score(dice: &[u8]) -> bool {
    if dice.is_empty() {
        return false;
    }
    let mut counts = [0u8; 7];
    for &d in dice {
        if (1..=6).contains(&d) {
            counts[d as usize] += 1;
        }
    }
    if counts[1] > 0 || counts[5] > 0 {
        return true;
    }
    (2..=6).any(|face| counts[face] >= 3)
}

pub fn score_selection(dice: &[u8], selected: &[usize]) -> Option<ScoreOutcome> {
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
    score_dice(&picked)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn singles() {
        assert_eq!(score_dice(&[1]).unwrap().points, 100);
        assert_eq!(score_dice(&[5]).unwrap().points, 50);
        assert_eq!(score_dice(&[1, 5]).unwrap().points, 150);
        assert!(score_dice(&[2]).is_none());
    }

    #[test]
    fn three_ones() {
        assert_eq!(score_dice(&[1, 1, 1]).unwrap().points, 1000);
        assert_eq!(score_dice(&[1, 1, 1, 1]).unwrap().points, 1000);
        assert_eq!(score_dice(&[1, 1, 1, 1, 5]).unwrap().points, 1050);
    }

    #[test]
    fn five_ones() {
        let s = score_dice(&[1, 1, 1, 1, 1]).unwrap();
        assert_eq!(s.points, 2000);
        assert!(!s.auto_win);
    }

    #[test]
    fn n_of_a_kind() {
        assert_eq!(score_dice(&[2, 2, 2]).unwrap().points, 200);
        assert_eq!(score_dice(&[3, 3, 3, 3]).unwrap().points, 3000);
        assert_eq!(score_dice(&[4, 4, 4, 1]).unwrap().points, 500);
    }

    #[test]
    fn five_of_a_kind_wins() {
        let s = score_dice(&[6, 6, 6, 6, 6]).unwrap();
        assert!(s.auto_win);
    }

    #[test]
    fn bust_detection() {
        assert!(!has_any_score(&[2, 3, 4, 6, 6]));
        assert!(has_any_score(&[2, 3, 4, 6, 5]));
        assert!(has_any_score(&[2, 2, 2, 3, 4]));
    }

    #[test]
    fn selection_rejects_dupes_and_dead() {
        assert!(score_selection(&[1, 2, 5], &[0, 2]).is_some());
        assert!(score_selection(&[1, 2, 5], &[0, 1]).is_none());
        assert!(score_selection(&[1, 2, 5], &[0, 0]).is_none());
    }
}
