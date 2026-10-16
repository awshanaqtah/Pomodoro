//! Something to do with a break, so it's a real rest and not more scrolling.

use serde::Serialize;

#[derive(Clone, Copy, PartialEq, Serialize)]
pub struct Idea {
    pub text: &'static str,
    /// Picks the little icon: eyes, stretch, water, breathe, walk or rest.
    pub kind: &'static str,
}

const SHORT: &[Idea] = &[
    Idea { text: "Look at something far away for 20 seconds", kind: "eyes" },
    Idea { text: "Roll your shoulders back five times", kind: "stretch" },
    Idea { text: "Drink a glass of water", kind: "water" },
    Idea { text: "Stand up and reach your arms overhead", kind: "stretch" },
    Idea { text: "Five slow breaths: in for 4, out for 6", kind: "breathe" },
    Idea { text: "Unclench your jaw and drop your shoulders", kind: "breathe" },
    Idea { text: "Walk to a window and look outside", kind: "eyes" },
    Idea { text: "Tilt your ear to each shoulder, slowly", kind: "stretch" },
    Idea { text: "Shake out your hands and wrists", kind: "stretch" },
    Idea { text: "Close your eyes for a minute", kind: "rest" },
];

const LONG: &[Idea] = &[
    Idea { text: "Take a short walk, outside if you can", kind: "walk" },
    Idea { text: "Eat something away from the screen", kind: "rest" },
    Idea { text: "A few squats, then a full-body stretch", kind: "stretch" },
    Idea { text: "Make tea or coffee, and leave the phone", kind: "water" },
    Idea { text: "Step outside for some fresh air", kind: "walk" },
    Idea { text: "Lie down and rest your eyes", kind: "rest" },
    Idea { text: "Tidy your desk for a few minutes", kind: "walk" },
];

/// A random idea for the break, never the same one twice in a row.
pub fn pick(long: bool, previous: Option<Idea>) -> Idea {
    use std::hash::{BuildHasher, Hasher};
    let list = if long { LONG } else { SHORT };
    // Not the clock: Windows time moves in 100 ns steps, so `nanos % 10` is always 0.
    let seed = std::collections::hash_map::RandomState::new().build_hasher().finish() as usize;
    let mut i = seed % list.len();
    if Some(list[i]) == previous {
        i = (i + 1) % list.len();
    }
    list[i]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varied_and_never_back_to_back() {
        let mut last = None;
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            let idea = pick(false, last);
            assert!(Some(idea) != last);
            seen.insert(idea.text);
            last = Some(idea);
        }
        assert!(seen.len() >= 8, "only {} different ideas came up", seen.len());
        assert!(LONG.contains(&pick(true, None)));
    }
}
