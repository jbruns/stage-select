//! The library folder and build screens' pure logic: labels, the plan's items by what they do, and which library
//! folders a selection can be built into.

use std::collections::BTreeMap;

use stage_select_api::SelectionRecord;
use stage_select_api::build::{
    BlockerKind, BuildState, Cause, CleanupAction, DriftEntry, DriftKind, FileOutcome,
    IncompleteCounts, IncompleteReason, ItemKind, ItemState, KeepReason, LibraryFolder, PlaceHow,
    PlanChoices, PlanContent, PlanItem, PlanPlace, PlanRemoval, PlanSummary, RemovalKind,
    RestoreKind, ReturnHow, UnfinishedBuild,
};

use crate::activity_model::bytes;
use crate::parts::status::BuildKind;

pub fn item_kind_label(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Copy => "Copy",
        ItemKind::Repack => "Repack",
        ItemKind::Unchanged => "Unchanged",
        ItemKind::NoSource => "No source",
        ItemKind::Conflict => "Conflict",
        ItemKind::Blocked => "Blocked",
        ItemKind::Adopt => "Adopt",
        ItemKind::Unresolved => "Unresolved",
        ItemKind::Remove => "Remove",
        ItemKind::KeepAside => "Keep aside",
        ItemKind::Acknowledge => "Acknowledge",
        ItemKind::Restore => "Restore",
        ItemKind::Place => "Place",
        ItemKind::Return => "Return",
    }
}

/// What a group of items means, for the heading over them.
pub fn item_kind_note(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Copy => "Copied as they are: the source is already this game's TorrentZip.",
        ItemKind::Repack => "Written as a TorrentZip from the verified ROMs.",
        ItemKind::Unchanged => "Already in the folder, built from the same ROMs.",
        ItemKind::NoSource => "No audited file holds every ROM, so these are skipped.",
        ItemKind::Conflict => {
            "A file the store did not build, or one changed since it was built, is at the target. It is never \
             overwritten, so these are skipped unless you choose to keep the file aside and rebuild."
        }
        ItemKind::Blocked => "These cannot be built as planned, so they are skipped.",
        ItemKind::Adopt => {
            "Files already in the folder that hold the ROMs the DAT expects. You chose to adopt them: they are \
             recorded as built, and nothing is written."
        }
        ItemKind::Unresolved => {
            "Their groups are unresolved, and you build the resolved groups only. Whatever is at their targets is \
             left untouched."
        }
        ItemKind::Remove => {
            "Built files no longer selected: each is kept aside, or forgotten if it is gone."
        }
        ItemKind::KeepAside => "Moved, whole, into the retention area as kept files.",
        ItemKind::Acknowledge => "Left as they are, and no longer reported until they change.",
        ItemKind::Restore => "Kept files put back into the folder.",
        ItemKind::Place => {
            "Single copy: moved or copied here from the source set, as its only copy, and taken out of the source set."
        }
        ItemKind::Return => {
            "Single copy: games placed here, moved or copied back into the source set, and verified there."
        }
    }
}

pub fn incomplete_reason_label(reason: IncompleteReason) -> &'static str {
    match reason {
        IncompleteReason::Unresolved => "unresolved",
        IncompleteReason::NoSource => "no source",
        IncompleteReason::Failed => "failed",
        IncompleteReason::Conflict => "conflict",
        IncompleteReason::Blocked => "blocked",
        IncompleteReason::Missing => "missing",
        IncompleteReason::MissingDisc => "missing disc",
        IncompleteReason::MissingDependency => "missing dependency",
    }
}

/// The counts behind an incomplete badge, such as `2 conflict, 1 failed`, leaving out reasons with none.
pub fn incomplete_counts(counts: &IncompleteCounts) -> String {
    IncompleteReason::ALL
        .into_iter()
        .filter(|r| counts.get(*r) > 0)
        .map(|r| format!("{} {}", counts.get(r), incomplete_reason_label(r)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The interrupted-build badge's text, from the store's record of the build.
pub fn unfinished_label(unfinished: &UnfinishedBuild) -> String {
    let what = match unfinished.state {
        BuildState::Failed => "Failed build",
        _ => "Interrupted build",
    };
    let kept = match unfinished.kept {
        0 => String::new(),
        n => format!(", {n} kept aside"),
    };
    format!(
        "{what}: {} applied, {} uncertain{kept}",
        unfinished.applied, unfinished.uncertain
    )
}

pub fn file_outcome_label(outcome: FileOutcome) -> &'static str {
    match outcome {
        FileOutcome::Unchanged => "Unchanged",
        FileOutcome::Refreshed => "Unchanged, record refreshed",
        FileOutcome::Repacked => "Same ROMs, another ZIP",
        FileOutcome::Conflict => "Conflict",
        FileOutcome::Missing => "Missing",
        FileOutcome::Unreadable => "Unreadable",
    }
}

pub fn drift_kind_label(kind: DriftKind) -> &'static str {
    match kind {
        DriftKind::Unselected => "Known title, not selected",
        DriftKind::Selected => "Selected title, not built here",
        DriftKind::Renamed => "Known title under another name",
        DriftKind::Unknown => "Unknown file",
        DriftKind::Directory => "Directory, never opened",
    }
}

pub fn cleanup_action_label(action: CleanupAction) -> &'static str {
    match action {
        CleanupAction::KeepAside => "Keep aside",
        CleanupAction::Adopt => "Adopt",
        CleanupAction::Acknowledge => "Acknowledge",
        CleanupAction::AddAddition => "Add to additions",
    }
}

pub fn restore_kind_label(kind: RestoreKind) -> &'static str {
    match kind {
        RestoreKind::Restore => "Put back and record as built",
        RestoreKind::Adopt => "Adopt under its game's file name",
        RestoreKind::PutBack => "Put back as it was",
    }
}

/// What a cleanup can do with drift entry `d`: keep it aside, always; adopt it, only a file that names a selected
/// title or holds a known one under another name; add it as an addition, only a file no DAT knows;
/// acknowledge it, unless it is at a selected game's file name.
pub fn cleanup_actions(d: &DriftEntry) -> Vec<CleanupAction> {
    let mut actions = vec![CleanupAction::KeepAside];
    if d.kind == DriftKind::Unknown {
        actions.push(CleanupAction::AddAddition);
    }
    if d.game.is_some() && matches!(d.kind, DriftKind::Selected | DriftKind::Renamed) {
        actions.push(CleanupAction::Adopt);
    }
    if !d.conflict {
        actions.push(CleanupAction::Acknowledge);
    }
    actions
}

/// What you choose for a build plan's item in conflict: leave it, adopt the file there, or keep it aside and build
/// the game in its place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictChoice {
    Leave,
    Adopt,
    KeepAside,
}

impl ConflictChoice {
    pub fn label(self) -> &'static str {
        match self {
            ConflictChoice::Leave => "Leave it",
            ConflictChoice::Adopt => "Adopt it",
            ConflictChoice::KeepAside => "Keep aside and rebuild",
        }
    }

    /// The choice `choices` makes for `item`.
    pub fn of(item: &PlanItem, choices: &PlanChoices) -> Self {
        if choices.adopt.contains(&item.target) {
            ConflictChoice::Adopt
        } else if choices.keep_aside.contains(&item.target) {
            ConflictChoice::KeepAside
        } else {
            ConflictChoice::Leave
        }
    }

    /// The choices `item` offers: none unless it is in conflict, or was and you chose for it.
    pub fn offered(item: &PlanItem) -> Vec<Self> {
        let adopt = item.adoptable || item.kind == ItemKind::Adopt;
        let keep = item.keepable
            || item
                .keep
                .as_ref()
                .is_some_and(|k| k.reason == KeepReason::Conflict);
        let leave = (adopt || keep).then_some(ConflictChoice::Leave);
        let adopt = adopt.then_some(ConflictChoice::Adopt);
        leave
            .into_iter()
            .chain(adopt)
            .chain(keep.then_some(ConflictChoice::KeepAside))
            .collect()
    }
}

/// The choices of a plan made again with `picked`, a choice by target, keeping the rest.
pub fn with_conflict_choices<'a>(
    choices: &PlanChoices,
    picked: impl IntoIterator<Item = (&'a String, &'a ConflictChoice)>,
) -> PlanChoices {
    let mut next = choices.clone();
    for (target, choice) in picked {
        next.adopt.retain(|t| t != target);
        next.keep_aside.retain(|t| t != target);
        match choice {
            ConflictChoice::Leave => {}
            ConflictChoice::Adopt => next.adopt.push(target.clone()),
            ConflictChoice::KeepAside => next.keep_aside.push(target.clone()),
        }
    }
    next.adopt.sort();
    next.keep_aside.sort();
    next
}

/// An item's row in its plan's review, as current → proposed: where its file comes from and its name, or, for an
/// item that writes nothing, its name and what becomes of it.
pub fn item_change(item: &PlanItem) -> (String, String) {
    let target = item.target.clone();
    match item.kind {
        ItemKind::Copy | ItemKind::Repack => {
            let mut from: Vec<String> = item
                .sources
                .iter()
                .map(|s| match &s.member {
                    Some(member) => format!("{} ({member})", s.path),
                    None => s.path.clone(),
                })
                .collect();
            from.dedup();
            (from.join(", "), target)
        }
        ItemKind::Unchanged => (target, "as it is".into()),
        ItemKind::Adopt => (target, "recorded as built".into()),
        ItemKind::Place => match &item.place {
            Some(place) => {
                let from: Vec<&str> = place.originals.iter().map(|o| o.path.as_str()).collect();
                (from.join(", "), target)
            }
            None => (target, "placed".into()),
        },
        _ => (target, "skipped".into()),
    }
}

/// The kind an item's row counts under, and what else it does: an item that keeps a file aside first (a replacement,
/// or a conflict you chose to keep aside) also counts as set aside, as the plan's summary counts it.
pub fn item_kinds(item: &PlanItem) -> (BuildKind, Option<BuildKind>) {
    (
        item.kind.into(),
        item.keep.as_ref().map(|_| BuildKind::SetAside),
    )
}

/// The kind a removal's row counts under, and what else it does: one that keeps the file also counts as set aside.
pub fn removal_kinds(removal: &PlanRemoval) -> (BuildKind, Option<BuildKind>) {
    let kept = removal.kind == RemovalKind::Keep;
    (BuildKind::Remove, kept.then_some(BuildKind::SetAside))
}

/// The kind a cleanup's entry counts under, by what it does with it.
pub fn entry_kind(action: CleanupAction) -> BuildKind {
    match action {
        CleanupAction::KeepAside | CleanupAction::AddAddition => BuildKind::SetAside,
        CleanupAction::Adopt => BuildKind::Adopt,
        CleanupAction::Acknowledge => BuildKind::Acknowledge,
    }
}

/// The kind a cleanup's kept file put back counts under: one adopted under its game's name is an adoption.
pub fn restore_kind(kind: RestoreKind) -> BuildKind {
    match kind {
        RestoreKind::Adopt => BuildKind::Adopt,
        RestoreKind::Restore | RestoreKind::PutBack => BuildKind::Restore,
    }
}

/// The kinds a plan's rows count under, each with what else it does, in the order its review lists them.
pub fn plan_kinds(content: &PlanContent) -> Vec<(BuildKind, Option<BuildKind>)> {
    let Some(cleanup) = &content.cleanup else {
        let returns = return_games(content)
            .into_iter()
            .map(|_| (BuildKind::Return, None));
        let items = content.items.iter().map(item_kinds);
        return returns
            .chain(items)
            .chain(content.removals.iter().map(removal_kinds))
            .collect();
    };
    let entries = cleanup
        .entries
        .iter()
        .filter_map(|e| e.action)
        .map(|a| (entry_kind(a), None));
    let restores = cleanup
        .restores
        .iter()
        .map(|r| (restore_kind(r.kind), None));
    let repacks = cleanup
        .repacks
        .iter()
        .map(|_| (BuildKind::Repack, Some(BuildKind::SetAside)));
    entries.chain(restores).chain(repacks).collect()
}

/// How a plan places a game, for its detail.
pub fn place_how_label(how: PlaceHow) -> &'static str {
    match how {
        PlaceHow::Rename => {
            "Renamed: your source set and this folder share a filesystem, so nothing is copied"
        }
        PlaceHow::Copy => {
            "Copied: this folder is on another filesystem, so the file is copied here and read back, then the original deleted"
        }
        PlaceHow::Convert => {
            "Converted: written here in this folder's format and checked, then the originals deleted"
        }
        PlaceHow::Built => {
            "Already built here: your source set's copy is deleted once the built file is checked again"
        }
    }
}

/// How a plan returns a release to its source set, for its detail.
pub fn return_how_label(how: ReturnHow) -> &'static str {
    match how {
        ReturnHow::Rename => {
            "Renamed home: your source set and this folder share a filesystem, so nothing is copied"
        }
        ReturnHow::Copy => {
            "Copied home and verified against the DAT, then deleted here unless this folder keeps it"
        }
        ReturnHow::Convert => {
            "Written home in your source set's format and verified against the DAT, then deleted here unless this folder keeps it"
        }
    }
}

/// What placing a game does, for its detail: how, the source set files it takes, and the space it frees and writes,
/// labelled as estimates where they are.
pub fn place_facts(place: &PlanPlace) -> Vec<(&'static str, String)> {
    let originals: Vec<&str> = place.originals.iter().map(|o| o.path.as_str()).collect();
    let written = match place.how {
        PlaceHow::Rename | PlaceHow::Built => "Nothing".to_string(),
        PlaceHow::Copy => bytes(place.written),
        PlaceHow::Convert => estimate(place.written),
    };
    vec![
        ("How", place_how_label(place.how).to_string()),
        ("Takes from your source set", originals.join(", ")),
        ("Frees in your source set", estimate(place.freed)),
        ("Writes here", written),
    ]
}

/// "About 3.0 MiB, an estimate".
fn estimate(n: u64) -> String {
    format!("About {}, an estimate", bytes(n))
}

/// "1 game", "3 games".
fn games(n: u64) -> String {
    match n {
        1 => "1 game".to_string(),
        n => format!("{n} games"),
    }
}

/// One game a plan returns to its source set's folder: its release goes whole, with every other game of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReturnGame {
    pub game: String,
    pub release: String,
    /// Its placed files, in the folder.
    pub current: Vec<String>,
    /// The files it becomes in the source set's folder.
    pub proposed: Vec<String>,
    pub how: ReturnHow,
    pub reason: String,
    /// The most its release writes in the source set's root.
    pub needs: u64,
    /// Whether the folder keeps its files as built copies once it is home.
    pub keeps: bool,
}

/// Each game a plan returns, release by release.
pub fn return_games(content: &PlanContent) -> Vec<ReturnGame> {
    content
        .returns
        .iter()
        .flat_map(|r| {
            r.games.iter().map(move |game| {
                let placed = r.placed.iter().filter(|p| &p.game == game);
                ReturnGame {
                    game: game.clone(),
                    release: r.release.clone(),
                    current: placed.clone().map(|p| p.path.clone()).collect(),
                    proposed: r
                        .files
                        .iter()
                        .filter(|f| &f.game == game)
                        .map(|f| f.path.clone())
                        .collect(),
                    how: r.how,
                    reason: r.reason.clone(),
                    needs: r.needs,
                    keeps: placed.clone().any(|p| p.keeps),
                }
            })
        })
        .collect()
}

/// What returning a game does, for its detail.
pub fn return_facts(game: &ReturnGame) -> Vec<(&'static str, String)> {
    let needs = match game.needs {
        0 => "Nothing".to_string(),
        n => format!("At most {} for its release, an upper bound", bytes(n)),
    };
    let mut facts = vec![
        ("Why", game.reason.clone()),
        ("How", return_how_label(game.how).to_string()),
        ("Release", format!("{}, returned whole", game.release)),
        ("Needs in your source set", needs),
    ];
    if game.keeps {
        facts.push((
            "Kept here",
            "Still selected, so this folder keeps its file as an ordinary built copy".to_string(),
        ));
    }
    facts
}

/// The games a plan places, each counted once however many files it has, and of them those placed by a rename.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Placing {
    pub games: u64,
    pub renamed: u64,
}

impl Placing {
    /// Counts the plan's placement items by game: the summary counts them by file, so a disc's cue and tracks would
    /// read as three games. A game counts as renamed only when every one of its files is: one file copied, converted
    /// or already built here makes it a copied game, since then placing it writes here and reads its source set.
    pub fn of(content: &PlanContent) -> Placing {
        let mut renamed: BTreeMap<&str, bool> = BTreeMap::new();
        for item in &content.items {
            if let Some(place) = &item.place {
                let all = renamed.entry(item.game.as_str()).or_insert(true);
                *all &= place.how == PlaceHow::Rename;
            }
        }
        Placing {
            games: renamed.len() as u64,
            renamed: renamed.values().filter(|r| **r).count() as u64,
        }
    }
}

/// What the plan's placements and returns do to space, as estimates where they are: one line each, none without
/// either. `placing` counts the games it places, which the summary counts by file.
pub fn space_lines(summary: &PlanSummary, placing: Placing) -> Vec<String> {
    let s = summary;
    let mut lines = Vec::new();
    if placing.games > 0 {
        let copy = if placing.games == 1 {
            "its only copy"
        } else {
            "their only copy"
        };
        let how = match placing.games - placing.renamed {
            0 => "all by a rename".to_string(),
            n if n == placing.games => "copied, converted or already built here".to_string(),
            n => format!(
                "{} by a rename, {n} copied, converted or already built here",
                placing.renamed
            ),
        };
        lines.push(format!(
            "Places {} here as {copy}: {how}.",
            games(placing.games)
        ));
        lines.push(match s.place_written {
            0 => format!(
                "Frees about {} in your source set and writes nothing new here. That's an estimate: block \
                 cloning may already share those bytes.",
                bytes(s.place_freed)
            ),
            written => format!(
                "Frees about {} in your source set and writes about {} here. Both are estimates: block \
                 cloning, compression and archive headers change them.",
                bytes(s.place_freed),
                bytes(written)
            ),
        });
    }
    if s.returned > 0 {
        lines.push(match s.return_needs {
            0 => format!(
                "Returns {} to your source set by a rename, needing no space there.",
                games(s.returned)
            ),
            needs => format!(
                "Returns {} to your source set, needing at most {} free in its root. That's an upper \
                 bound: a rename needs none.",
                games(s.returned),
                bytes(needs)
            ),
        });
    }
    lines
}

/// The approve button of a build plan: what approving it does, counting files where it builds them and games where
/// it places or returns them.
pub fn approve_label(summary: &PlanSummary, placing: Placing) -> String {
    let s = summary;
    let writes = s.copy + s.repack;
    if placing.games + s.returned + s.keep == 0 {
        return format!("Approve and build {writes} files");
    }
    let files = if writes == 1 {
        "1 file".to_string()
    } else {
        format!("{writes} files")
    };
    let parts = [
        (writes > 0).then(|| format!("build {files}")),
        (placing.games > 0).then(|| format!("place {}", games(placing.games))),
        (s.returned > 0).then(|| format!("return {}", games(s.returned))),
        (s.keep > 0).then(|| format!("set {} aside", s.keep)),
    ];
    let parts: Vec<String> = parts.into_iter().flatten().collect();
    format!("Approve: {}", parts.join(", "))
}

pub fn keep_reason_label(reason: KeepReason) -> &'static str {
    match reason {
        KeepReason::Removal => "no longer selected",
        KeepReason::Replacement => "replaced",
        KeepReason::Conflict => "conflict kept aside",
        KeepReason::Drift => "not built here, kept aside",
        KeepReason::Repack => "before repacking",
    }
}

pub fn cause_label(cause: Cause) -> &'static str {
    match cause {
        Cause::Settings => "your settings",
        Cause::Choices => "your choices",
        Cause::DatRevision => "the DAT revision",
    }
}

pub fn blocker_label(kind: BlockerKind) -> &'static str {
    match kind {
        BlockerKind::NoAudit => "No audit",
        BlockerKind::RevisionMismatch => "DAT revision mismatch",
        BlockerKind::Unresolved => "Unresolved group",
        BlockerKind::ReadOnlyRoot => "Read-only root",
        BlockerKind::FolderMissing => "Folder missing",
        BlockerKind::Marker => "Marker",
        BlockerKind::Recovery => "Recovery",
        BlockerKind::Cleanup => "Cleanup",
        BlockerKind::Space => "Not enough space",
    }
}

pub fn build_state_label(state: BuildState) -> &'static str {
    match state {
        BuildState::Running => "Running",
        BuildState::Succeeded => "Succeeded",
        BuildState::Failed => "Failed",
        BuildState::Interrupted => "Interrupted",
    }
}

pub fn item_state_label(state: ItemState) -> &'static str {
    match state {
        ItemState::Pending => "Not written",
        ItemState::Publishing => "Writing",
        ItemState::Applied => "Written",
        ItemState::Failed => "Failed",
        ItemState::Uncertain => "Uncertain",
    }
}

/// The plan's items by kind, the ones a build writes first, leaving out kinds with none.
pub fn by_kind(items: &[PlanItem]) -> Vec<(ItemKind, Vec<&PlanItem>)> {
    ItemKind::ALL
        .into_iter()
        .map(|kind| (kind, items.iter().filter(|i| i.kind == kind).collect()))
        .filter(|(_, items): &(ItemKind, Vec<&PlanItem>)| !items.is_empty())
        .collect()
}

/// The library folders `selection` can be built into: those bound to its source set and selection profile.
pub fn folders_for<'a>(
    folders: &'a [LibraryFolder],
    selection: &SelectionRecord,
) -> Vec<&'a LibraryFolder> {
    folders
        .iter()
        .filter(|f| f.source_set == selection.source_set && f.profile == selection.profile.name)
        .collect()
}

/// Where you confirm an unresolved group: its selection, opened at the group.
pub fn group_href(selection: i64, group: &str) -> String {
    format!("/selections/{selection}?group={}", url_component(group))
}

/// Percent-encodes everything but unreserved characters, as `encodeURIComponent` does.
fn url_component(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use stage_select_api::ProfileVersionRef;
    use stage_select_api::build::{Cleanup, KeepAside};
    use stage_select_api::selection::{SelectionPins, SelectionSummary};

    fn drift(kind: DriftKind, game: Option<&str>, conflict: bool) -> DriftEntry {
        DriftEntry {
            name: "x.zip".into(),
            kind,
            game: game.map(Into::into),
            conflict,
            hashed: false,
            detail: None,
        }
    }

    #[test]
    fn a_cleanup_offers_adopting_only_selected_or_renamed_titles_and_never_acknowledges_a_conflict()
    {
        use CleanupAction::*;
        assert_eq!(
            cleanup_actions(&drift(DriftKind::Selected, Some("Pong"), true)),
            [KeepAside, Adopt]
        );
        assert_eq!(
            cleanup_actions(&drift(DriftKind::Renamed, Some("Pong"), false)),
            [KeepAside, Adopt, Acknowledge]
        );
        assert_eq!(
            cleanup_actions(&drift(DriftKind::Unselected, Some("Pong"), false)),
            [KeepAside, Acknowledge]
        );
        assert_eq!(
            cleanup_actions(&drift(DriftKind::Directory, None, false)),
            [KeepAside, Acknowledge]
        );
    }

    fn item(game: &str, kind: ItemKind) -> PlanItem {
        PlanItem {
            group: game.to_lowercase(),
            game: game.into(),
            target: format!("{game}.zip"),
            kind,
            roms: Vec::new(),
            sources: Vec::new(),
            reason: None,
            existing: None,
            keepable: false,
            keep: None,
            adoptable: false,
            loose: false,
            chd: false,
            converts: None,
            addition: None,
            generated: None,
            place: None,
            changes_source_set: false,
            disk: false,
            missing_dependencies: Vec::new(),
        }
    }

    fn folder(id: i64, source_set: &str, profile: &str) -> LibraryFolder {
        LibraryFolder {
            id,
            root: "play".into(),
            path: "atari2600".into(),
            system: "No-Intro/Atari - Atari 2600".into(),
            source_set: source_set.into(),
            profile: profile.into(),
            generation: 0,
            created_at: String::new(),
            writable: true,
            build: None,
            files: 0,
            active_job: None,
            latest_plan: None,
            incomplete: None,
            unfinished: None,
            check: None,
            plan_open: false,
            stale: Vec::new(),
            built_for: None,
            disk_folders: Vec::new(),
        }
    }

    #[test]
    fn items_are_grouped_by_kind_with_written_ones_first_and_empty_kinds_left_out() {
        let items = [
            item("Pong", ItemKind::NoSource),
            item("Combat", ItemKind::Repack),
            item("Yars", ItemKind::Copy),
            item("Adventure", ItemKind::Repack),
        ];

        let grouped = by_kind(&items);

        let shape: Vec<(ItemKind, Vec<&str>)> = grouped
            .iter()
            .map(|(k, items)| (*k, items.iter().map(|i| i.game.as_str()).collect()))
            .collect();
        assert_eq!(
            shape,
            [
                (ItemKind::Copy, vec!["Yars"]),
                (ItemKind::Repack, vec!["Combat", "Adventure"]),
                (ItemKind::NoSource, vec!["Pong"]),
            ]
        );
    }

    #[test]
    fn a_selection_is_offered_the_folders_bound_to_its_source_set_and_profile() {
        let folders = [
            folder(1, "Atari", "Mine"),
            folder(2, "Atari", "Theirs"),
            folder(3, "Other", "Mine"),
            folder(4, "Atari", "Mine"),
        ];
        let selection = SelectionRecord {
            id: 7,
            source_set: "Atari".into(),
            dat_revision_sha256: String::new(),
            dat_revision_version: String::new(),
            profile: ProfileVersionRef {
                name: "Mine".into(),
                version: 1,
            },
            latest_profile_version: 1,
            job: None,
            system: String::new(),
            system_settings: false,
            pins: SelectionPins::default(),
            summary: SelectionSummary::default(),
            created_at: String::new(),
            pruned: false,
            library_folders: Vec::new(),
        };

        let ids: Vec<i64> = folders_for(&folders, &selection)
            .iter()
            .map(|f| f.id)
            .collect();

        assert_eq!(ids, [1, 4]);
    }

    #[test]
    fn an_incomplete_badge_counts_only_the_reasons_it_has() {
        let counts = IncompleteCounts {
            conflict: 2,
            failed: 1,
            ..IncompleteCounts::default()
        };

        assert_eq!(incomplete_counts(&counts), "1 failed, 2 conflict");
    }

    #[test]
    fn a_mame_folder_lacking_a_dependency_is_incomplete_for_that_reason() {
        let counts = IncompleteCounts {
            missing_dependency: 2,
            missing_disc: 1,
            ..IncompleteCounts::default()
        };

        assert_eq!(
            incomplete_counts(&counts),
            "1 missing disc, 2 missing dependency"
        );
    }

    #[test]
    fn choosing_for_conflicts_keeps_the_other_choices() {
        let choices = PlanChoices {
            resolved_only: true,
            adopt: vec!["Pong.zip".into()],
            keep_aside: vec!["Combat.zip".into()],
            remove_all: false,
        };
        let picked = BTreeMap::from([
            ("Combat.zip".to_string(), ConflictChoice::Adopt),
            ("Pong.zip".to_string(), ConflictChoice::Leave),
            ("Asteroids.zip".to_string(), ConflictChoice::KeepAside),
        ]);

        let next = with_conflict_choices(&choices, &picked);

        assert_eq!(next.adopt, ["Combat.zip"]);
        assert_eq!(next.keep_aside, ["Asteroids.zip"]);
        assert!(next.resolved_only);
    }

    #[test]
    fn only_a_conflict_or_an_item_chosen_for_one_offers_choices() {
        assert!(ConflictChoice::offered(&item("Pong", ItemKind::NoSource)).is_empty());
        let conflict = PlanItem {
            keepable: true,
            ..item("Pong", ItemKind::Conflict)
        };
        assert_eq!(
            ConflictChoice::offered(&conflict),
            [ConflictChoice::Leave, ConflictChoice::KeepAside]
        );
        assert_eq!(
            ConflictChoice::offered(&item("Pong", ItemKind::Adopt)),
            [ConflictChoice::Leave, ConflictChoice::Adopt]
        );
        let choices = PlanChoices {
            adopt: vec!["Pong.zip".into()],
            ..PlanChoices::default()
        };
        assert_eq!(
            ConflictChoice::of(&conflict, &choices),
            ConflictChoice::Adopt
        );
    }

    #[test]
    fn a_written_item_reads_from_its_sources_and_a_skipped_one_from_its_name() {
        let copy = PlanItem {
            sources: ["a.zip", "a.zip", "b.zip"]
                .map(|path| {
                    serde_json::from_value(serde_json::json!({
                        "rom": "r", "path": path, "file": { "size": 1, "mtime_ns": 0 }
                    }))
                    .unwrap()
                })
                .into(),
            ..item("Pong", ItemKind::Copy)
        };
        assert_eq!(
            item_change(&copy),
            ("a.zip, b.zip".into(), "Pong.zip".into())
        );
        assert_eq!(
            item_change(&item("Pong", ItemKind::Conflict)),
            ("Pong.zip".into(), "skipped".into())
        );
    }

    #[test]
    fn an_unresolved_group_links_to_its_selection_with_the_name_escaped() {
        assert_eq!(
            group_href(7, "café & co/1"),
            "/selections/7?group=caf%C3%A9%20%26%20co%2F1"
        );
    }

    /// The kinds counted from a plan's rows, as `kind_counts` counts them for the review's strip.
    fn counted(content: &PlanContent) -> Vec<(BuildKind, usize)> {
        let kinds = plan_kinds(content);
        crate::parts::plan_model::kind_counts(
            kinds
                .into_iter()
                .flat_map(|(k, also)| [Some(k), also])
                .flatten(),
        )
    }

    fn summary_of(content: &PlanContent, kind: BuildKind) -> usize {
        counted(content)
            .into_iter()
            .find(|(k, _)| *k == kind)
            .map_or(0, |(_, n)| n)
    }

    fn content(items: Vec<PlanItem>, removals: Vec<PlanRemoval>) -> PlanContent {
        serde_json::from_value::<PlanContent>(serde_json::json!({
            "folder": 1, "generation": 1, "selection": 1, "audit": null, "dat_revision": "r",
            "items": [], "blockers": [],
        }))
        .map(|c| PlanContent {
            items,
            removals,
            ..c
        })
        .unwrap()
    }

    #[test]
    fn a_replacement_counts_as_set_aside_as_the_summary_does() {
        let kept: KeepAside = serde_json::from_value(serde_json::json!({
            "reason": "replacement", "name": "Pong.zip",
            "file": { "size": 1, "mtime_ns": 0 }, "identity": { "dev": 1, "ino": 2 },
        }))
        .unwrap();
        let removal = |kind| PlanRemoval {
            target: "Combat.zip".into(),
            game: "Combat".into(),
            kind,
            reason: String::new(),
            keep: None,
            converts_to: None,
        };
        let plan = content(
            vec![
                PlanItem {
                    keep: Some(kept),
                    ..item("Pong", ItemKind::Repack)
                },
                item("Combat", ItemKind::Copy),
            ],
            vec![removal(RemovalKind::Keep), removal(RemovalKind::Forget)],
        );
        let summary = plan.summary();

        assert_eq!(summary.keep, 2);
        assert_eq!(summary_of(&plan, BuildKind::SetAside), 2);
        assert_eq!(summary_of(&plan, BuildKind::Repack), 1);
        assert_eq!(summary_of(&plan, BuildKind::Remove), 2);
        assert_eq!(
            plan_kinds(&plan).len(),
            4,
            "one row each, counted twice where it sets a file aside"
        );
    }

    #[test]
    fn a_cleanups_rows_count_as_its_summary_does() {
        let stat = serde_json::json!({ "file": { "size": 1, "mtime_ns": 0 }, "identity": { "dev": 1, "ino": 2 } });
        let entry = |name: &str, action: &str| {
            let mut e = serde_json::json!({ "name": name, "kind": "unselected", "action": action });
            e.as_object_mut()
                .unwrap()
                .extend(stat.as_object().unwrap().clone());
            e
        };
        let mut kept = serde_json::json!({
            "id": 1, "folder": 1, "generation": 1, "name": "Pong.zip", "kept_name": "Pong.zip",
            "reason": "drift", "build": 1, "kept_at": "now",
        });
        kept.as_object_mut()
            .unwrap()
            .extend(stat.as_object().unwrap().clone());
        let mut repack = serde_json::json!({ "target": "Combat.zip", "game": "Combat", "roms": [], "sha256": "s" });
        repack
            .as_object_mut()
            .unwrap()
            .extend(stat.as_object().unwrap().clone());
        let cleanup: Cleanup = serde_json::from_value(serde_json::json!({
            "build": 1, "choices": {},
            "entries": [entry("a", "keep_aside"), entry("b", "add_addition"), entry("c", "adopt"), entry("d", "acknowledge")],
            "restores": [{ "kept": kept, "kind": "adopt", "target": "Pong.zip" }],
            "repacks": [repack],
        }))
        .unwrap();
        let plan = PlanContent {
            cleanup: Some(cleanup),
            ..content(Vec::new(), Vec::new())
        };
        let s = plan.summary();

        for (kind, n) in [
            (BuildKind::SetAside, s.keep),
            (BuildKind::Adopt, s.adopt),
            (BuildKind::Acknowledge, s.acknowledge),
            (BuildKind::Restore, s.restore),
            (BuildKind::Repack, s.repack),
        ] {
            assert_eq!(summary_of(&plan, kind) as u64, n, "{kind:?}");
        }
        assert_eq!(s.keep, 3);
    }

    fn place(how: &str, freed: u64, written: u64) -> PlanPlace {
        serde_json::from_value(serde_json::json!({
            "how": how, "freed": freed, "written": written,
            "originals": [{
                "path": "Pong (USA).zip", "file": { "size": 1, "mtime_ns": 0 },
                "identity": { "dev": 1, "ino": 2 }, "ctime_ns": 0,
            }],
        }))
        .unwrap()
    }

    #[test]
    fn a_placement_reads_from_the_source_set_file_it_takes() {
        let placed = PlanItem {
            place: Some(place("rename", 1024, 0)),
            ..item("Pong (USA)", ItemKind::Place)
        };
        assert_eq!(
            item_change(&placed),
            ("Pong (USA).zip".into(), "Pong (USA).zip".into())
        );
    }

    #[test]
    fn a_placements_space_is_exact_for_a_copy_and_an_estimate_otherwise() {
        let facts =
            |p: PlanPlace| -> BTreeMap<&str, String> { place_facts(&p).into_iter().collect() };
        let renamed = facts(place("rename", 3 * 1024 * 1024, 0));
        assert!(renamed["How"].starts_with("Renamed"), "{renamed:?}");
        assert_eq!(renamed["Takes from your source set"], "Pong (USA).zip");
        assert_eq!(
            renamed["Frees in your source set"],
            "About 3.0 MiB, an estimate"
        );
        assert_eq!(renamed["Writes here"], "Nothing");
        let copied = facts(place("copy", 2048, 2048));
        assert_eq!(copied["Writes here"], "2.0 KiB");
        let converted = facts(place("convert", 2048, 1500));
        assert_eq!(converted["Writes here"], "About 1.5 KiB, an estimate");
    }

    fn summary(json: serde_json::Value) -> PlanSummary {
        let mut s = serde_json::to_value(PlanSummary::default()).unwrap();
        s.as_object_mut()
            .unwrap()
            .extend(json.as_object().unwrap().clone());
        serde_json::from_value(s).unwrap()
    }

    #[test]
    fn the_plans_space_figures_say_which_are_estimates_and_how_many_rename() {
        let none = Placing::default();
        assert!(space_lines(&summary(serde_json::json!({})), none).is_empty());
        let placing = space_lines(
            &summary(serde_json::json!({
                "place": 5, "place_renamed": 3,
                "place_freed": 5 * 1024 * 1024 * 1024u64, "place_written": 300 * 1024 * 1024,
            })),
            Placing {
                games: 3,
                renamed: 2,
            },
        );
        assert_eq!(
            placing,
            [
                "Places 3 games here as their only copy: 2 by a rename, 1 copied, converted or already built here.",
                "Frees about 5.0 GiB in your source set and writes about 300 MiB here. Both are estimates: block \
                 cloning, compression and archive headers change them.",
            ]
        );
        let renamed = space_lines(
            &summary(serde_json::json!({
                "place": 1, "place_renamed": 1, "place_freed": 1024, "place_written": 0,
            })),
            Placing {
                games: 1,
                renamed: 1,
            },
        );
        assert_eq!(
            renamed,
            [
                "Places 1 game here as its only copy: all by a rename.",
                "Frees about 1.0 KiB in your source set and writes nothing new here. That's an estimate: block \
                 cloning may already share those bytes.",
            ]
        );
        let returning = space_lines(
            &summary(serde_json::json!({
                "returned": 2, "return_needs": 64 * 1024,
            })),
            none,
        );
        assert_eq!(
            returning,
            [
                "Returns 2 games to your source set, needing at most 64 KiB free in its root. That's an upper \
              bound: a rename needs none."
            ]
        );
        let renamed_home = space_lines(
            &summary(serde_json::json!({ "returned": 1, "return_needs": 0 })),
            none,
        );
        assert_eq!(
            renamed_home,
            ["Returns 1 game to your source set by a rename, needing no space there."]
        );
    }

    #[test]
    fn approving_says_what_it_builds_places_returns_and_sets_aside() {
        assert_eq!(
            approve_label(
                &summary(serde_json::json!({ "copy": 1, "repack": 2 })),
                Placing::default()
            ),
            "Approve and build 3 files"
        );
        assert_eq!(
            approve_label(
                &summary(serde_json::json!({
                    "repack": 1, "place": 4, "returned": 1, "keep": 1,
                })),
                Placing {
                    games: 2,
                    renamed: 0,
                }
            ),
            "Approve: build 1 file, place 2 games, return 1 game, set 1 aside"
        );
    }

    #[test]
    fn placing_a_disc_counts_it_once_and_as_copied_when_any_of_its_files_copies() {
        let placed = |game: &str, target: &str, how: &str| PlanItem {
            target: target.into(),
            place: Some(place(how, 1, 0)),
            ..item(game, ItemKind::Place)
        };
        let plan = content(
            vec![
                placed("Orbit", "Orbit/Orbit.cue", "rename"),
                placed("Orbit", "Orbit/Orbit (Track 1).bin", "rename"),
                placed("Orbit", "Orbit/Orbit (Track 2).bin", "copy"),
                placed("Pong", "Pong.zip", "rename"),
                item("Combat", ItemKind::Copy),
            ],
            Vec::new(),
        );
        let placing = Placing::of(&plan);
        assert_eq!(plan.summary().place, 4, "the summary counts files");
        assert_eq!(
            placing,
            Placing {
                games: 2,
                renamed: 1
            }
        );
        assert_eq!(
            space_lines(&plan.summary(), placing)[0],
            "Places 2 games here as their only copy: 1 by a rename, 1 copied, converted or already built here."
        );
        assert_eq!(
            approve_label(&plan.summary(), placing),
            "Approve: build 1 file, place 2 games"
        );
    }

    #[test]
    fn a_return_is_listed_by_game_with_its_releases_files() {
        let plan = PlanContent {
            returns: vec![serde_json::from_value(serde_json::json!({
                "release": "Orbit", "games": ["Orbit (Disc 1)", "Orbit (Disc 2)"], "how": "convert",
                "reason": "single copy is off for its system", "needs": 65536,
                "placed": [
                    { "path": "Orbit (Disc 1).chd", "game": "Orbit (Disc 1)", "file": { "size": 1, "mtime_ns": 0 },
                      "identity": { "dev": 1, "ino": 2 }, "ctime_ns": 0, "keeps": true },
                    { "path": "Orbit (Disc 2).chd", "game": "Orbit (Disc 2)", "file": { "size": 1, "mtime_ns": 0 },
                      "identity": { "dev": 1, "ino": 3 }, "ctime_ns": 0 },
                ],
                "files": [
                    { "path": "Orbit (Disc 1)/Orbit (Disc 1).cue", "game": "Orbit (Disc 1)", "roms": [], "sources": [] },
                    { "path": "Orbit (Disc 1)/Orbit (Disc 1).bin", "game": "Orbit (Disc 1)", "roms": [], "sources": [] },
                    { "path": "Orbit (Disc 2)/Orbit (Disc 2).cue", "game": "Orbit (Disc 2)", "roms": [], "sources": [] },
                ],
            }))
            .unwrap()],
            ..content(Vec::new(), Vec::new())
        };
        let games = return_games(&plan);
        assert_eq!(games.len(), 2);
        assert_eq!(games[0].current, ["Orbit (Disc 1).chd"]);
        assert_eq!(
            games[0].proposed,
            [
                "Orbit (Disc 1)/Orbit (Disc 1).cue",
                "Orbit (Disc 1)/Orbit (Disc 1).bin"
            ]
        );
        assert!(games[0].keeps && !games[1].keeps);
        let facts: BTreeMap<&str, String> = return_facts(&games[1]).into_iter().collect();
        assert_eq!(facts["Why"], "single copy is off for its system");
        assert_eq!(facts["Release"], "Orbit, returned whole");
        assert_eq!(
            facts["Needs in your source set"],
            "At most 64 KiB for its release, an upper bound"
        );
        assert_eq!(
            summary_of(&plan, BuildKind::Return) as u64,
            plan.summary().returned,
            "one row per game, as the summary counts them"
        );
    }
}
