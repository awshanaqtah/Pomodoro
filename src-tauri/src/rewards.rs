//! What your forest gains as you keep focusing. Rewards are only ever gained: a broken streak or
//! a reset session takes nothing away, because unlocks go by your best streak, not your current one.

use serde::Serialize;

enum Need {
    Sessions(u32),
    Streak(u32),
}

struct Rule {
    id: &'static str,
    name: &'static str,
    need: Need,
}

const RULES: &[Rule] = &[
    Rule { id: "pines", name: "Pine forest", need: Need::Sessions(0) },
    Rule { id: "maples", name: "Maple trees", need: Need::Sessions(10) },
    Rule { id: "blossoms", name: "Blossom trees", need: Need::Streak(3) },
    Rule { id: "fireflies", name: "Fireflies", need: Need::Sessions(25) },
    Rule { id: "moon", name: "The moon", need: Need::Streak(7) },
    Rule { id: "owl", name: "An owl", need: Need::Sessions(50) },
    Rule { id: "campfire", name: "A campfire", need: Need::Streak(14) },
    Rule { id: "aurora", name: "Northern lights", need: Need::Sessions(100) },
];

#[derive(Clone, Serialize)]
pub struct Reward {
    pub id: &'static str,
    pub name: &'static str,
    /// How to earn it, e.g. "10 focus sessions" or "a 3-day streak".
    pub hint: String,
    pub unlocked: bool,
    /// 0..=1 toward unlocking.
    pub progress: f32,
}

pub fn rewards(total_sessions: u32, best_streak: u32) -> Vec<Reward> {
    RULES
        .iter()
        .map(|r| {
            let (have, need, hint) = match r.need {
                Need::Sessions(n) => (total_sessions, n, format!("{n} focus sessions")),
                Need::Streak(n) => (best_streak, n, format!("a {n}-day streak")),
            };
            Reward {
                id: r.id,
                name: r.name,
                hint: if need == 0 { "where it all starts".into() } else { hint },
                unlocked: have >= need,
                progress: if need == 0 { 1.0 } else { (have as f32 / need as f32).min(1.0) },
            }
        })
        .collect()
}

/// Names of rewards that `after` has and `before` didn't.
pub fn newly_unlocked(before: &[Reward], after: &[Reward]) -> Vec<&'static str> {
    after
        .iter()
        .zip(before)
        .filter(|(a, b)| a.unlocked && !b.unlocked)
        .map(|(a, _)| a.name)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlocks_by_sessions_and_best_streak() {
        let fresh = rewards(0, 0);
        assert_eq!(fresh.iter().filter(|r| r.unlocked).count(), 1, "only the pines to start");
        let later = rewards(12, 3);
        let names = newly_unlocked(&fresh, &later);
        assert_eq!(names, ["Maple trees", "Blossom trees"]);
        let maple_progress = rewards(5, 0).into_iter().find(|r| r.id == "maples").unwrap().progress;
        assert!((maple_progress - 0.5).abs() < 1e-6);
    }
}
