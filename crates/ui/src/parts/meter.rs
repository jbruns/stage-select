//! The segmented meter: "how much of a whole" as a row of equal segments, for a Job's progress and a Source Set's
//! Games by Game Status. It describes and never sets a goal: it sits beside the exact counts, and the whole row is
//! one named image whose label carries them.

use std::collections::BTreeMap;

use leptos::prelude::*;
use stage_select_api::report::GameStatus;

use super::status::{Status, Tone};

/// Segments in a Job's progress meter.
pub const PROGRESS_SEGMENTS: usize = 10;
/// Segments in a Source Set's Games meter.
pub const GAME_SEGMENTS: usize = 16;

/// How a segment is drawn, so that parts differ by pattern as well as colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fill {
    Solid,
    Hatched,
    Outline,
}

/// One drawn segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub fill: Fill,
    pub tone: Tone,
}

/// The segment left over when nothing is counted: outlined, in the neutral tone.
pub const EMPTY: Segment = Segment {
    fill: Fill::Outline,
    tone: Tone::Neutral,
};

/// One part of the whole: how many it counts, and how its segments look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Part {
    pub count: usize,
    pub segment: Segment,
}

/// `n` segments shared among `parts` in proportion to their counts, in order. A part counting above zero always gets
/// at least one segment, while `n` allows. With nothing counted, every segment is `EMPTY`.
pub fn segments(parts: &[Part], n: usize) -> Vec<Segment> {
    // Widened before any sum or product: `usize` is 32 bits on wasm32, and counts can pass u32::MAX between them.
    let wide = |x: usize| x as u128;
    let total: u128 = parts.iter().map(|p| wide(p.count)).sum();
    if total == 0 {
        return vec![EMPTY; n];
    }
    // Each part's share is count × n / total segments. Compared scaled by `total`, so the arithmetic stays exact.
    let share = |i: usize, given: usize| {
        (wide(parts[i].count) * wide(n)) as i128 - (wide(given) * total) as i128
    };
    let mut given: Vec<usize> = parts
        .iter()
        .map(|p| ((wide(p.count) * wide(n) / total) as usize).max(usize::from(p.count > 0)))
        .collect();
    let mut sum: usize = given.iter().sum();
    // The rule of at least one can give out more than `n`: take back from the part most over its share that keeps one.
    while sum > n {
        let Some(i) = (0..parts.len())
            .filter(|&i| given[i] > 1)
            .min_by_key(|&i| (share(i, given[i]), i))
            .or_else(|| (0..parts.len()).rev().find(|&i| given[i] > 0))
        else {
            break;
        };
        given[i] -= 1;
        sum -= 1;
    }
    // Rounding down leaves segments over: they go to the parts furthest under their share, earlier parts first.
    while sum < n {
        let i = (0..parts.len())
            .filter(|&i| parts[i].count > 0)
            .max_by_key(|&i| (share(i, given[i]), std::cmp::Reverse(i)))
            .expect("a part counts above zero");
        given[i] += 1;
        sum += 1;
    }
    parts
        .iter()
        .zip(given)
        .flat_map(|(p, k)| std::iter::repeat_n(p.segment, k))
        .collect()
}

/// A Job's progress: what is done, filled in the active tone, then what is left, outlined.
pub fn progress_parts(done: usize, total: usize) -> Vec<Part> {
    let done = done.min(total);
    vec![
        Part {
            count: done,
            segment: Segment {
                fill: Fill::Solid,
                tone: Tone::Active,
            },
        },
        Part {
            count: total - done,
            segment: EMPTY,
        },
    ]
}

/// The progress meter's name: "12 of 40 files done".
pub fn progress_label(done: usize, total: usize, unit: &str) -> String {
    format!("{done} of {total} {unit} done")
}

/// The Games the meter counts, in its order: complete filled, incomplete hatched, missing outlined. A Game with
/// nothing to check is in none of them.
const GAME_FILLS: [(GameStatus, Fill); 3] = [
    (GameStatus::Complete, Fill::Solid),
    (GameStatus::Incomplete, Fill::Hatched),
    (GameStatus::Missing, Fill::Outline),
];

/// A Source Set's Games by Game Status, each in its status's tone.
pub fn game_parts(games: &BTreeMap<GameStatus, usize>) -> Vec<Part> {
    GAME_FILLS
        .into_iter()
        .map(|(status, fill)| Part {
            count: games.get(&status).copied().unwrap_or(0),
            segment: Segment {
                fill,
                tone: status.look().tone,
            },
        })
        .collect()
}

/// The Games meter's name: "Games: 12 complete, 3 incomplete, 900 missing".
pub fn games_label(games: &BTreeMap<GameStatus, usize>) -> String {
    let counts: Vec<String> = GAME_FILLS
        .into_iter()
        .map(|(status, _)| {
            let n = games.get(&status).copied().unwrap_or(0);
            format!("{n} {}", status.look().label.to_lowercase())
        })
        .collect();
    format!("Games: {}", counts.join(", "))
}

impl Fill {
    fn class(self) -> &'static str {
        match self {
            Fill::Solid => "meter-solid",
            Fill::Hatched => "meter-hatched",
            Fill::Outline => "meter-outline",
        }
    }
}

/// The meter itself: `segments` in a row, one image named by `label`.
#[component]
fn Meter(segments: Vec<Segment>, label: String) -> impl IntoView {
    view! {
        <span class="meter" role="img" aria-label=label>
            {segments
                .into_iter()
                .map(|s| {
                    view! { <span class=format!("meter-segment {} {}", s.fill.class(), s.tone.class())></span> }
                })
                .collect_view()}
        </span>
    }
}

/// A Job's progress, `done` of `total` `unit` ("files", "steps"), beside its exact counts.
#[component]
pub fn ProgressMeter(done: usize, total: usize, unit: &'static str) -> impl IntoView {
    let segments = segments(&progress_parts(done, total), PROGRESS_SEGMENTS);
    view! { <Meter segments label=progress_label(done, total, unit) /> }
}

/// A Source Set's Games by Game Status, for beside their exact counts.
#[component]
pub fn GamesMeter(games: BTreeMap<GameStatus, usize>) -> impl IntoView {
    let segments = segments(&game_parts(&games), GAME_SEGMENTS);
    view! { <Meter segments label=games_label(&games) /> }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DONE: Segment = Segment {
        fill: Fill::Solid,
        tone: Tone::Active,
    };

    fn part(count: usize, segment: Segment) -> Part {
        Part { count, segment }
    }

    fn counted(drawn: &[Segment], segment: Segment) -> usize {
        drawn.iter().filter(|s| **s == segment).count()
    }

    #[test]
    fn a_count_above_zero_shows_at_least_one_segment() {
        let drawn = segments(&[part(3, DONE), part(2_997, EMPTY)], 10);
        assert_eq!(drawn.len(), 10);
        assert_eq!(drawn[0], DONE);
        assert_eq!(counted(&drawn, DONE), 1);
    }

    // `usize` is 32 bits on wasm32, so totals past u32::MAX are real there; on the host they reach usize::MAX.
    #[test]
    fn counts_past_u32_max_share_in_proportion() {
        let drawn = segments(&[part(644_245_094, DONE), part(429_496_729, EMPTY)], 10);
        assert_eq!(drawn.len(), 10);
        assert_eq!(counted(&drawn, DONE), 6);

        let half = 1 << 31;
        let drawn = segments(&[part(half, DONE), part(half, EMPTY)], 10);
        assert_eq!(counted(&drawn, DONE), 5);
        assert_eq!(counted(&drawn, EMPTY), 5);
    }

    #[test]
    fn counts_near_usize_max_keep_the_total_and_at_least_one() {
        let drawn = segments(&[part(usize::MAX, DONE), part(usize::MAX, EMPTY)], 10);
        assert_eq!(counted(&drawn, DONE), 5);
        assert_eq!(counted(&drawn, EMPTY), 5);

        let drawn = segments(&[part(1, DONE), part(usize::MAX, EMPTY)], 10);
        assert_eq!(drawn.len(), 10);
        assert_eq!(counted(&drawn, DONE), 1);

        let drawn = segments(&[part(usize::MAX - 1, DONE), part(1, EMPTY)], 10);
        assert_eq!(drawn.len(), 10);
        assert_eq!(counted(&drawn, EMPTY), 1);
    }

    #[test]
    fn what_is_left_of_a_nearly_done_job_still_shows() {
        let drawn = segments(&[part(2_999, DONE), part(1, EMPTY)], 10);
        assert_eq!(counted(&drawn, DONE), 9);
        assert_eq!(drawn[9], EMPTY);
    }

    #[test]
    fn segments_are_shared_in_proportion_and_in_order() {
        let hatched = Segment {
            fill: Fill::Hatched,
            tone: Tone::Review,
        };
        let drawn = segments(&[part(50, DONE), part(30, hatched), part(20, EMPTY)], 10);
        assert_eq!(
            drawn,
            [[DONE; 5].as_slice(), &[hatched; 3], &[EMPTY; 2]].concat()
        );
        // 7 and 3 of 4 segments are 2.8 and 1.2: the one left over goes to the part furthest under its share.
        assert_eq!(
            segments(&[part(7, DONE), part(3, EMPTY)], 4),
            [DONE, DONE, DONE, EMPTY]
        );
    }

    #[test]
    fn small_parts_take_their_one_segment_from_the_largest() {
        let hatched = Segment {
            fill: Fill::Hatched,
            tone: Tone::Review,
        };
        let drawn = segments(&[part(12, DONE), part(3, hatched), part(900, EMPTY)], 16);
        assert_eq!(drawn, [[DONE, hatched].as_slice(), &[EMPTY; 14]].concat());
    }

    #[test]
    fn nothing_counted_is_all_outlined() {
        assert_eq!(segments(&[part(0, DONE), part(0, EMPTY)], 4), [EMPTY; 4]);
        assert_eq!(segments(&[], 3), [EMPTY; 3]);
    }

    #[test]
    fn a_small_whole_fills_the_meter_like_a_large_one() {
        assert_eq!(segments(&[part(5, DONE)], 16), [DONE; 16]);
        assert_eq!(segments(&[part(5_000, DONE)], 16), [DONE; 16]);
    }

    #[test]
    fn games_are_complete_then_incomplete_then_missing() {
        let games = BTreeMap::from([
            (GameStatus::Missing, 900),
            (GameStatus::Complete, 12),
            (GameStatus::Incomplete, 3),
            (GameStatus::NothingToCheck, 40),
        ]);
        let drawn = segments(&game_parts(&games), GAME_SEGMENTS);
        assert_eq!(
            drawn[..2],
            [
                Segment {
                    fill: Fill::Solid,
                    tone: Tone::Good
                },
                Segment {
                    fill: Fill::Hatched,
                    tone: Tone::Review
                },
            ]
        );
        assert!(drawn[2..].iter().all(|s| *s == EMPTY));
        assert_eq!(
            games_label(&games),
            "Games: 12 complete, 3 incomplete, 900 missing"
        );
        assert_eq!(
            games_label(&BTreeMap::new()),
            "Games: 0 complete, 0 incomplete, 0 missing"
        );
    }

    #[test]
    fn progress_is_done_then_left() {
        assert_eq!(
            segments(&progress_parts(3, 4), PROGRESS_SEGMENTS),
            [[DONE; 8].as_slice(), &[EMPTY; 2]].concat()
        );
        // Done past the total, as a stale total can make it, fills the meter and no more.
        assert_eq!(
            segments(&progress_parts(5, 4), PROGRESS_SEGMENTS),
            [DONE; PROGRESS_SEGMENTS]
        );
        assert_eq!(progress_label(12, 40, "files"), "12 of 40 files done");
    }

    #[test]
    fn more_counted_parts_than_segments_keeps_the_first() {
        let hatched = Segment {
            fill: Fill::Hatched,
            tone: Tone::Review,
        };
        assert_eq!(
            segments(&[part(1, DONE), part(1, hatched), part(1, EMPTY)], 2),
            [DONE, hatched]
        );
    }
}
