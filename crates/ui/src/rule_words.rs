//! The selection engine's explanations in the glossary's words. The engine's trace keeps Retool's messages, which
//! say "title", "group", "release" and "excluded"; the workbench shows each known one as the Operator's words: game,
//! game group, kept, clone, left out. `{0}`, `{1}`, `{2}` and `{list}` stand for the values Retool fills in, which
//! are carried across untouched.

/// Each message Retool's trace records, as the engine shows it (its first line, without `ACTION: ` or a trailing
/// colon), the synthetic messages and the reasons it leaves a game out, and the words the workbench shows instead.
const WORDS: &[(&str, &str)] = &[
    (
        "Original unmodified group",
        "The game group as the DAT has it",
    ),
    (
        "Group after taking clone list priorities into account",
        "Game group after clone list priorities",
    ),
    (
        "Group after cleaning up preproduction/bad/pirate/mixed version-revision titles",
        "Game group after cleaning up preproduction, bad, pirate and mixed version-revision games",
    ),
    (
        "Group after handling modern title rips",
        "Game group after handling modern rips",
    ),
    (
        "[First pass] Group after filtering by user language order",
        "Game group after filtering by your language order, first pass",
    ),
    (
        "Group after handling special editions",
        "Game group after handling special editions",
    ),
    (
        "Group after handling versions and revisions",
        "Game group after handling versions and revisions",
    ),
    (
        "Group after choosing dates",
        "Game group after choosing dates",
    ),
    (
        "Group after choosing good, original versions over alternatives",
        "Game group after choosing good, original versions over other versions",
    ),
    (
        "Group after handling promotions and demotions",
        "Game group after handling promotions and demotions",
    ),
    (
        "Group after handling \"Made in\" titles",
        "Game group after handling \"Made in\" games",
    ),
    ("Cross region parents", "Parents across regions"),
    (
        "Group after lower priority region, non-superset clones removed",
        "Game group after removing clones from lower-priority regions that aren't supersets",
    ),
    (
        "Highest language priority titles",
        "Games with the highest language priority",
    ),
    (
        "Top superset title for tag free name {1}: {0}",
        "Top superset game for {1}, without tags: {0}",
    ),
    (
        "Top superset title/s for group",
        "Top superset games of the game group",
    ),
    (
        "Group after production title check",
        "Game group after the production check",
    ),
    (
        "Clone assignments (might be reassigned later)",
        "Clone assignments, which may change later",
    ),
    ("Compare clone list priority", "Compare clone list priority"),
    (
        "Compare clone list priority (both titles are supersets)",
        "Compare clone list priority, both games being supersets",
    ),
    ("Compare dates", "Compare dates"),
    ("Compare languages", "Compare languages"),
    (
        "Favor multiple regions higher up user region order",
        "Favor more regions higher in your region order",
    ),
    ("Choose title with string", "Choose the game with the text"),
    (
        "Choose title without string",
        "Choose the game without the text",
    ),
    ("Choose \"Made in\" title", "Choose the \"Made in\" game"),
    ("Choose higher versions", "Choose higher versions"),
    (
        "Excluded due to categories: {0}",
        "Left out by category: {0}",
    ),
    (
        "[Second pass] Group after filtering by user language order",
        "Game group after filtering by your language order, second pass",
    ),
    (
        "Excluded due to a categories regex: {0}",
        "Left out by a category regular expression: {0}",
    ),
    (
        "Excluded due to a system override",
        "Left out by a system override",
    ),
    (
        "Excluded due to a global override",
        "Left out by a global override",
    ),
    (
        "Included due to a system override",
        "Included by a system override",
    ),
    (
        "Included due to a global override",
        "Included by a global override",
    ),
    (
        "Removed due to user language filters: {0}",
        "Left out by your language filter: {0}",
    ),
    (
        "Would have been a system override exclude, but was cancelled out by a system include",
        "A system override would leave it out, but a system include keeps it",
    ),
    (
        "Would have been a global override exclude, but was cancelled out by a global include",
        "A global override would leave it out, but a global include keeps it",
    ),
    (
        "Would have been a global override exclude, but was cancelled out by a system include",
        "A global override would leave it out, but a system include keeps it",
    ),
    (
        "Changing categories for title found in clone list Categories object",
        "Categories changed by the clone list's Categories object",
    ),
    (
        "Removing title found in clone list Removes object",
        "Left out by the clone list's Removes object",
    ),
    (
        "Moving title found in clone list Variants object to other group",
        "Moved to another game group by the clone list's Variants object",
    ),
    (
        "Overriding default group for title found in clone list overrides object",
        "Game group set by the clone list's overrides object",
    ),
    (
        "Fail safe removal of title with lower string value",
        "Fail-safe: the game whose name sorts lower is removed",
    ),
    (
        "Group after filtering by string comparison",
        "Game group after comparing names as text",
    ),
    (
        "Group after handling unlicensed/aftermarket/pirate versions",
        "Game group after handling unlicensed, aftermarket and pirate versions",
    ),
    (
        "Group after handling Alt versions",
        "Game group after handling Alt versions",
    ),
    ("Excluded due to known MIA", "Left out as a known MIA"),
    (
        "Included due to being related to a system or global override",
        "Included as related to a system or global override",
    ),
    (
        "Group after filtering by user language order",
        "Game group after filtering by your language order",
    ),
    (
        "Group after filtering by region order",
        "Game group after filtering by your region order",
    ),
    (
        "Group with original compilation names",
        "Game group with the original compilation names",
    ),
    (
        "Contenders for crossover groups: {0}",
        "Contenders for crossover game groups: {0}",
    ),
    (
        "Looking for this ideal combination: {list}",
        "Looking for this ideal combination: {list}",
    ),
    (
        "Compilation clone assignments",
        "Compilation clone assignments",
    ),
    (
        "Only one title in this region, skipping filtering",
        "Only one game in this region, so no filtering",
    ),
    (
        "Tie breaker, remove the compilation title",
        "Tie-breaker: the compilation is removed",
    ),
    (
        "Group after choosing superset titles",
        "Game group after choosing supersets",
    ),
    ("Choose supersets", "Choose supersets"),
    (
        "Group after favoring multiple regions higher up user region order",
        "Game group after favoring more regions higher in your region order",
    ),
    (
        "Group after filtering by clone list priority",
        "Game group after filtering by clone list priority",
    ),
    ("Choose more languages", "Choose more languages"),
    (
        "Fallback language comparison. Compare languages based on default region order.",
        "Fallback: compare languages by the default region order",
    ),
    (
        "ROM tagged as MIA due to name match in {0} (name: {1})",
        "ROM marked MIA by a name match in {0} (name: {1})",
    ),
    (
        "User has region bias enabled, selecting a higher region title than the superset",
        "You prefer regions, so a game from a higher region is chosen over the superset",
    ),
    ("Keeping superset title", "Keeping the superset"),
    (
        "Keeping compilation that contains another",
        "Keeping the compilation that contains another",
    ),
    (
        "Group after removing preproduction",
        "Game group after removing preproduction games",
    ),
    (
        "Favor primary region higher up user region order (individual title vs compilation)",
        "Favor the primary region higher in your region order, individual game or compilation",
    ),
    (
        "Group after choosing video standard",
        "Game group after choosing the video standard",
    ),
    (
        "Fallback language comparison. Compare languages based on region order.",
        "Fallback: compare languages by your region order",
    ),
    (
        "Kept due to system post filter",
        "Kept by a system post filter",
    ),
    (
        "Kept due to global post filter",
        "Kept by a global post filter",
    ),
    (
        "Group after fallback user language priority check",
        "Game group after the fallback language priority check",
    ),
    (
        "Group after fallback higher priority region check",
        "Game group after the fallback region priority check",
    ),
    (
        "Group after non-superset clones removed",
        "Game group after removing clones that aren't supersets",
    ),
    (
        "Tie breaker, keep the first compilation title",
        "Tie-breaker: the first compilation is kept",
    ),
    (
        "Changing categories for title found in clone list Variants object",
        "Categories changed by the clone list's Variants object",
    ),
    (
        "Moving title found in clone list Variants object to other group due to a condition being true",
        "Moved to another game group by a condition in the clone list's Variants object",
    ),
    (
        "Changing a title's priority due to a condition in the Variants object being true",
        "Priority changed by a condition in the clone list's Variants object",
    ),
    (
        "Setting a title's local name due to a condition in the Variants object being true",
        "Local name set by a condition in the clone list's Variants object",
    ),
    (
        "Marking a title as English-friendly due to an entry in the Variants object",
        "Marked English-friendly by the clone list's Variants object",
    ),
    (
        "Marking a title as English-friendly due to a condition in the Variants object being true",
        "Marked English-friendly by a condition in the clone list's Variants object",
    ),
    (
        "Found a superset assigned as a both a parent and clone. Setting title with the superset as a parent to the \
         superset's parent to resolve the conflict",
        "A superset was both a parent and a clone, so the game whose parent it was now has the superset's parent",
    ),
    (
        "Setting a title's local name as defined in the Variants object",
        "Local name set by the clone list's Variants object",
    ),
    (
        "Setting a title as a superset due to a condition in the Variants object being true",
        "Made a superset by a condition in the clone list's Variants object",
    ),
    (
        "Group after good dump/production/retail check",
        "Game group after the good dump, production and retail check",
    ),
    (
        "INFO: Here are the title groups in contention",
        "The game groups in contention",
    ),
    (
        "INFO: Here are the regions required for each group in contention",
        "The regions each game group in contention needs",
    ),
    (
        "INFO: Here are the combinations in groups of {0}",
        "The combinations in sets of {0}",
    ),
    (
        "INFO: Here are the results of the candidate selection stages",
        "The results of the candidate selection stages",
    ),
    (
        "INFO: Testing these candidates for consideration",
        "Testing these candidates",
    ),
    (
        "INFO: Test details for candidate",
        "Test details for a candidate",
    ),
    (
        "Group after choosing individual title due to user preference",
        "Game group after choosing individual games, as you prefer",
    ),
    (
        "Group after choosing superset",
        "Game group after choosing the superset",
    ),
    (
        "Group after comparing compilations",
        "Game group after comparing compilations",
    ),
    (
        "Marking a title as the oldest due to an entry in the Variants object",
        "Marked oldest by the clone list's Variants object",
    ),
    (
        "Compare clone list priority, oldest title",
        "Compare clone list priority, oldest game",
    ),
    (
        "Marking a title as the oldest due to a condition in the Variants object being true",
        "Marked oldest by a condition in the clone list's Variants object",
    ),
    (
        "Group after handling budget editions",
        "Game group after handling budget editions",
    ),
    (
        "ROM tagged as MIA due to CRC match in {0} (CRC: {1})",
        "ROM marked MIA by a CRC match in {0} (CRC: {1})",
    ),
    (
        "ROM tagged as MIA due to CRC and name match in {0} (CRC: {1}, name: {2})",
        "ROM marked MIA by a CRC and name match in {0} (CRC: {1}, name: {2})",
    ),
    (
        "Title tagged as supporting RetroAchievements {0}",
        "Marked as supporting RetroAchievements {0}",
    ),
    ("Choose RetroAchievements", "Choose RetroAchievements"),
    (
        "Group after choosing RetroAchievements titles due to user preference",
        "Game group after choosing games that support RetroAchievements, as you prefer",
    ),
    (
        "Group after comparing World titles to USA, Europe, or Japan",
        "Game group after comparing World games with USA, Europe or Japan ones",
    ),
    // The engine's own messages, for a game no trace record names.
    ("No other release competed", "No other version competed"),
    ("Assigned as a clone", "Assigned as a clone"),
    // Why Retool left a game out.
    ("Clone list remove", "Removed by the clone list"),
    ("Language remove", "Left out by your language filter"),
    ("Region remove", "Not in your region order"),
    ("Category exclude, {0}", "Category, {0}"),
    ("Known MIA title", "Known MIA"),
    ("User system exclude", "Your system exclude overrides"),
    ("User global exclude", "Your global exclude overrides"),
    (
        "Related to user system excludes",
        "Related to a game your system exclude overrides leave out",
    ),
    (
        "Related to user global excludes",
        "Related to a game your global exclude overrides leave out",
    ),
    (
        "System post filter exclude",
        "Not kept by your system post filters",
    ),
    (
        "Global post filter exclude",
        "Not kept by your global post filters",
    ),
];

/// An arcade election's fixed words that name a machine a release; the rest of its messages are its own.
const ARCADE_FRAGMENTS: &[(&str, &str)] = &[("an original release", "an original version")];

/// `message`, one of the engine's explanations or reasons, in the workbench's words; one it doesn't know, as it is,
/// but for the fixed fragments of an arcade election.
pub fn explanation(message: &str) -> String {
    if let Some(words) = WORDS
        .iter()
        .find_map(|(theirs, ours)| translated(message, theirs, ours))
    {
        return words;
    }
    ARCADE_FRAGMENTS
        .iter()
        .fold(message.to_owned(), |text, (theirs, ours)| {
            text.replace(theirs, ours)
        })
}

/// A template's literal text and the placeholders between it: `"a {0} b"` is `["a ", " b"]` and `["{0}"]`.
fn parts(template: &str) -> (Vec<&str>, Vec<&str>) {
    let mut literals = Vec::new();
    let mut holes = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let Some(len) = rest[start..].find('}') else {
            break;
        };
        literals.push(&rest[..start]);
        holes.push(&rest[start..start + len + 1]);
        rest = &rest[start + len + 1..];
    }
    literals.push(rest);
    (literals, holes)
}

/// `ours` with the values `message` fills into `theirs`, when `message` is `theirs` filled in.
fn translated(message: &str, theirs: &str, ours: &str) -> Option<String> {
    let (literals, holes) = parts(theirs);
    if holes.is_empty() {
        return (message == theirs).then(|| ours.to_owned());
    }
    let mut rest = message.strip_prefix(literals[0])?;
    let mut values = Vec::new();
    for (i, literal) in literals[1..].iter().enumerate() {
        let last = i + 2 == literals.len();
        let end = if last {
            rest.strip_suffix(literal).map(str::len)?
        } else {
            rest.find(literal)?
        };
        values.push(&rest[..end]);
        rest = &rest[end + literal.len()..];
    }
    Some(
        holes
            .iter()
            .zip(values)
            .fold(ours.to_owned(), |text, (hole, value)| {
                text.replace(hole, value)
            }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every trace message the engine can record, as `refs.rs` holds them, reduced as the engine shows them.
    fn engine_messages() -> Vec<(String, String)> {
        let source = include_str!("../../selection/src/native/refs.rs");
        let mut out = Vec::new();
        let mut rest = source;
        while let Some(at) = rest.find("\"REF") {
            let reference = &rest[at + 1..at + 8];
            rest = &rest[at + 9..];
            let open = rest.find('"').expect("a template follows its reference");
            let mut template = String::new();
            let mut chars = rest[open + 1..].char_indices();
            let mut end = 0;
            while let Some((i, c)) = chars.next() {
                match c {
                    '\\' => match chars.next().map(|(_, c)| c) {
                        Some('n') => template.push('\n'),
                        Some(other) => template.push(other),
                        None => {}
                    },
                    '"' => {
                        end = open + 1 + i + 1;
                        break;
                    }
                    c => template.push(c),
                }
            }
            rest = &rest[end..];
            let first = template.lines().next().unwrap_or_default();
            let first = first.trim().trim_end_matches(':').trim_end();
            let shown = first.strip_prefix("ACTION: ").unwrap_or(first);
            out.push((reference.to_owned(), shown.to_owned()));
        }
        out
    }

    fn filled(template: &str) -> String {
        template
            .replace("{0}", "Demos")
            .replace("{1}", "Pong")
            .replace("{2}", "Pong (USA)")
            .replace("{list}", "['USA', 'Europe']")
    }

    fn assert_plain(text: &str, of: &str) {
        let lower = text.to_lowercase();
        for banned in ["release", "alternative", "winner", "exclusion"] {
            assert!(!lower.contains(banned), "{of:?} reads {text:?}");
        }
    }

    #[test]
    fn every_engine_message_reads_in_the_glossarys_words() {
        let messages = engine_messages();
        assert!(messages.len() > 100, "read {} templates", messages.len());
        for (reference, template) in messages {
            let message = filled(&template);
            let known = WORDS
                .iter()
                .find_map(|(theirs, ours)| translated(&message, theirs, ours));
            assert!(known.is_some(), "{reference} {template:?} has no words");
            let shown = explanation(&message);
            assert_plain(&shown, &template);
            assert!(!shown.contains("title"), "{reference} reads {shown:?}");
            assert!(!shown.starts_with("Group"), "{reference} reads {shown:?}");
        }
    }

    #[test]
    fn every_known_message_and_reason_reads_without_the_old_words() {
        for (theirs, _) in WORDS {
            assert_plain(&explanation(&filled(theirs)), theirs);
        }
    }

    #[test]
    fn the_values_retool_fills_in_are_carried_across_untouched() {
        assert_eq!(
            explanation("Top superset title for tag free name pong: Pong (USA) (Rerelease)"),
            "Top superset game for pong, without tags: Pong (USA) (Rerelease)"
        );
        assert_eq!(
            explanation(
                "ROM tagged as MIA due to CRC and name match in a.dat (CRC: 1234abcd, name: Pong)"
            ),
            "ROM marked MIA by a CRC and name match in a.dat (CRC: 1234abcd, name: Pong)"
        );
        assert_eq!(
            explanation("Excluded due to categories: Demos, kiosks, and samples"),
            "Left out by category: Demos, kiosks, and samples"
        );
        assert_eq!(
            explanation("No other release competed"),
            "No other version competed"
        );
    }

    #[test]
    fn an_arcade_election_says_version_for_an_original_machine() {
        assert_eq!(
            explanation("a bootleg, which ranks below an original release"),
            "a bootleg, which ranks below an original version"
        );
        assert_eq!(
            explanation("its shortname sorts first"),
            "its shortname sorts first"
        );
    }
}
