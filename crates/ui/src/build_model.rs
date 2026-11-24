//! The library folder and build screens' pure logic: labels, the plan's items by what they do, and which library
//! folders a selection can be built into.

use std::collections::BTreeMap;

use stage_select_api::SelectionRecord;
use stage_select_api::build::BuildSummary;
use stage_select_api::build::{
    Blocker, BlockerKind, BuildDetail, BuildItem, BuildState, Cause, CleanupAction, DriftEntry,
    DriftKind, FileOutcome, FolderDeletionOutcome, FolderLeftover, IncompleteCounts,
    IncompleteReason, ItemKind, ItemState, KeepReason, LibraryFolder, PlaceHow, PlanChoices,
    PlanContent, PlanItem, PlanPlace, PlanRemoval, PlanSummary, RemovalKind, RemoveAction,
    RestoreKind, ReturnHow, UnfinishedBuild,
};

use crate::activity_model::bytes;
use crate::parts::plan_model::{self, Does, PlanBlocker, kind_slug};
use crate::parts::status::{BuildKind, ItemOutcome, Status};
use crate::parts::tabs::tab_link;

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
        ItemKind::KeepAside => "Set aside",
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
             overwritten, so these are skipped unless you choose to set the file aside and rebuild."
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
            "Built files no longer selected: each is set aside, or forgotten if it is gone."
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
        n => format!(", {n} set aside"),
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
        CleanupAction::KeepAside => "Set aside",
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
            ConflictChoice::KeepAside => "Set aside and rebuild",
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

/// What a plan does to its rows of `kind`, for its summary line.
fn does(kind: BuildKind) -> Does {
    match kind {
        BuildKind::Copy => Does::new("builds", "file", "files"),
        BuildKind::Repack => Does::new("repacks", "file", "files"),
        BuildKind::Place => Does::new("places", "game", "games"),
        BuildKind::Return => Does::new("returns", "game", "games"),
        BuildKind::Adopt => Does::new("adopts", "file", "files"),
        BuildKind::Unchanged => Does::new("keeps", "file as it is", "files as they are"),
        BuildKind::NoSource => Does::new("has no source for", "game", "games"),
        BuildKind::Conflict => Does::new("finds", "file in the way", "files in the way"),
        BuildKind::Unresolved => Does::new("waits on", "unresolved group", "unresolved groups"),
        BuildKind::Blocked => Does::new("can't build", "blocked game", "blocked games"),
        BuildKind::Remove => Does::new("removes", "file", "files"),
        BuildKind::SetAside => Does::new("sets aside", "file", "files"),
        BuildKind::Acknowledge => Does::new("acknowledges", "entry", "entries"),
        BuildKind::Restore => Does::new("puts back", "kept file", "kept files"),
    }
}

/// What a build plan that removes games you chose does with them, for you to read before you approve it: each game
/// with how, and, for a deletion, that it is deleted and a placed game isn't returned. None for any other plan.
pub fn removal_notice(choices: &PlanChoices) -> Option<String> {
    if choices.remove.is_empty() {
        return None;
    }
    let games = choices
        .remove
        .iter()
        .map(|r| {
            let how = match r.action {
                RemoveAction::SendBack => "sent back to your source set",
                RemoveAction::SetAside => "set aside",
                RemoveAction::Delete => "deleted",
            };
            format!("{} ({how})", r.game)
        })
        .collect::<Vec<_>>()
        .join(", ");
    let mut said = format!(
        "This plan removes games you chose from the folder: {games}. Approving it adds them to the folder's removed \
         games, which its builds leave out until you restore them."
    );
    if choices
        .remove
        .iter()
        .any(|r| r.action == RemoveAction::Delete)
    {
        said.push_str(
            " A deleted game's files are set aside, then deleted: a game placed here under single copy is lost, not \
             returned to your source set.",
        );
    }
    Some(said)
}

/// "1 file" or "N files".
fn files(n: usize) -> String {
    match n {
        1 => "1 file".to_owned(),
        n => format!("{n} files"),
    }
}

/// What a plan that deletes its folder does, for you to read, and confirm, before you approve it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderDeletionNotice {
    /// What it deletes, in counts and sizes.
    pub deletes: String,
    /// The games it returns to your source set instead, if any.
    pub returns: Option<String>,
    /// Whether the folder itself is removed, or why it stays.
    pub folder: String,
    /// What it leaves in the folder.
    pub left: Vec<FolderLeftover>,
}

/// What approving plan `content` deletes, returns and leaves, when it deletes its folder; none for any other plan.
pub fn folder_deletion_notice(content: &PlanContent) -> Option<FolderDeletionNotice> {
    let deletion = content.folder_deletion.as_ref()?;
    let built: Vec<u64> = content
        .removals
        .iter()
        .filter_map(|r| r.keep.as_ref().map(|k| k.file.size))
        .collect();
    let kept: Vec<u64> = deletion.kept.iter().map(|k| k.file.size).collect();
    let total = bytes(built.iter().chain(&kept).sum());
    let how = match (built.len(), kept.len()) {
        (0, k) => format!("{} you kept aside before", files(k)),
        (b, 0) => format!("{} built, set aside first", files(b)),
        (b, k) => format!(
            "{} built, set aside first, and {} you kept aside before",
            files(b),
            files(k)
        ),
    };
    let deletes = format!(
        "Approving deletes {} Stage Select wrote here ({total}) for good: {how}.",
        files(built.len() + kept.len()),
    );
    let games: usize = content.returns.iter().map(|r| r.games.len()).sum();
    let returns = (games > 0).then(|| match games {
        1 => "1 game placed here under single copy goes back to your source set first. It isn't deleted.".to_owned(),
        n => format!("{n} games placed here under single copy go back to your source set first. They aren't deleted."),
    });
    let folder = match deletion.left.len() {
        0 => "Then the folder itself is removed, and unregistered.".to_owned(),
        n => format!(
            "The folder itself stays, with {} below left in it: Stage Select deletes only what it wrote. It is still \
             unregistered.",
            match n {
                1 => "the file".to_owned(),
                n => format!("the {n} files"),
            }
        ),
    };
    Some(FolderDeletionNotice {
        deletes,
        returns,
        folder,
        left: deletion.left.clone(),
    })
}

/// What you tick to confirm you mean to delete a folder before approving.
pub const DELETE_FOLDER_CONFIRM: &str = "Delete these files for good";

/// What deleting the folder did, once it finished: whether it is an outcome to report as done, and what to say. None
/// while it waits on its build.
pub fn folder_deletion_outcome(outcome: &FolderDeletionOutcome) -> Option<(bool, String)> {
    outcome.finished_at.as_ref()?;
    let detail = outcome
        .detail
        .as_deref()
        .map(|d| {
            format!(
                " {}{}.",
                d[..1].to_uppercase(),
                d[1..].trim_end_matches('.')
            )
        })
        .unwrap_or_default();
    Some(match (outcome.removed, outcome.left.len()) {
        (true, _) => (
            true,
            format!(
                "Deleted the folder and every file Stage Select wrote in it, and unregistered it.{detail}"
            ),
        ),
        (false, 0) => (false, format!("The folder wasn't removed.{detail}")),
        (false, n) => (
            false,
            format!(
                "Deleted every file Stage Select wrote and unregistered the folder. The folder stays, holding {} \
                 Stage Select didn't delete:{detail}",
                files(n)
            ),
        ),
    })
}

/// A build plan's one-line summary in numbers: its rows by what each does, in the order of their kinds.
pub fn plan_line(content: &PlanContent) -> String {
    let mut kinds: Vec<BuildKind> = plan_kinds(content).into_iter().map(|(k, _)| k).collect();
    kinds.sort_by_key(|k| k.rank());
    plan_model::plan_line(kinds.into_iter().map(|k| (does(k), 1)), "Changes nothing")
}

/// What became of a build's item, by its state.
pub fn item_outcome(state: ItemState) -> ItemOutcome {
    match state {
        ItemState::Applied => ItemOutcome::Done,
        ItemState::Failed => ItemOutcome::Failed,
        ItemState::Uncertain => ItemOutcome::Uncertain,
        ItemState::Pending | ItemState::Publishing => ItemOutcome::NotDone,
    }
}

/// The build item a plan's row is carried out as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Built {
    /// A file it writes or skips, by its target.
    Item(String),
    /// A built file it removes, by its target.
    Removal(String),
    /// A placed game it returns, by its release: a return's item is its release's.
    Return(String),
}

/// What became of the row `built` in the build `detail`: none until the build has its item.
pub fn built_outcome(built: &Built, detail: &BuildDetail) -> Option<ItemOutcome> {
    let is = |i: &&BuildItem| match built {
        Built::Item(t) => i.target == *t && !matches!(i.kind, ItemKind::Remove | ItemKind::Return),
        Built::Removal(t) => i.target == *t && i.kind == ItemKind::Remove,
        Built::Return(release) => i.target == *release && i.kind == ItemKind::Return,
    };
    detail.items.iter().find(is).map(|i| item_outcome(i.state))
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
        n => format!("At most {} for the whole game, an upper bound", bytes(n)),
    };
    let mut facts = vec![
        ("Why", game.reason.clone()),
        ("How", return_how_label(game.how).to_string()),
        ("Game", format!("{}, returned whole", game.release)),
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
        KeepReason::Conflict => "conflict set aside",
        KeepReason::Drift => "not built here, set aside",
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
        BlockerKind::Unresolved => "Unresolved game group",
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

/// A build's files as its page shows them: its counts, then its files, failures first and written last.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildFiles {
    /// Failed, uncertain and writing (each only when a file is), not written, and written, with how many files.
    pub counts: Vec<(ItemState, u64)>,
    /// Each file with its place in the build, in the order the page lists them.
    pub rows: Vec<(usize, BuildItem)>,
}

pub fn build_files(detail: &BuildDetail) -> BuildFiles {
    let b = &detail.build;
    let mut counts = vec![(ItemState::Failed, b.failed)];
    if b.uncertain > 0 {
        counts.push((ItemState::Uncertain, b.uncertain));
    }
    let writing = publishing(detail);
    if writing > 0 {
        counts.push((ItemState::Publishing, writing));
    }
    counts.extend([
        (ItemState::Pending, b.pending),
        (ItemState::Applied, b.applied),
    ]);
    let rank = |state: ItemState| ItemState::ALL.iter().position(|s| *s == state);
    let mut rows: Vec<(usize, BuildItem)> = detail.items.iter().cloned().enumerate().collect();
    rows.sort_by_key(|(_, i)| rank(i.state));
    BuildFiles { counts, rows }
}

/// How many of a build's files are still being put in place, which its summary doesn't count.
pub fn publishing(detail: &BuildDetail) -> u64 {
    detail
        .items
        .iter()
        .filter(|i| i.state == ItemState::Publishing)
        .count() as u64
}

/// Whether a build earned the READY mark: it succeeded and every file it set out to write is in place, none failed,
/// pending, uncertain or still publishing. A build job succeeds even when a file's re-hash fails, so the job's state
/// alone can't say.
pub fn ready(detail: &BuildDetail) -> bool {
    let b = &detail.build;
    b.state == BuildState::Succeeded
        && b.failed == 0
        && b.pending == 0
        && b.uncertain == 0
        && publishing(detail) == 0
}

/// A count of a build's files that need a look, and where its build page lists them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trouble {
    pub state: ItemState,
    pub text: String,
    pub href: String,
}

/// The build's failed and uncertain files, each linking to its build page filtered to them; none when it has none.
pub fn troubles(b: &BuildSummary) -> Vec<Trouble> {
    [
        (ItemState::Failed, b.failed, "failed"),
        (ItemState::Uncertain, b.uncertain, "uncertain"),
    ]
    .into_iter()
    .filter(|(_, n, _)| *n > 0)
    .map(|(state, n, word)| Trouble {
        state,
        text: format!("{n} {} {word}", if n == 1 { "file" } else { "files" }),
        href: tab_link(
            &format!("/builds/{}", b.id),
            None,
            &[("state", &kind_slug(state))],
        ),
    })
    .collect()
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

/// What to do about a blocker of the kind `kind`.
fn blocker_fix(kind: BlockerKind) -> &'static str {
    match kind {
        BlockerKind::NoAudit => "Audit the source set, then plan again.",
        BlockerKind::RevisionMismatch => {
            "Audit the source set against the selection's DAT revision, then plan again."
        }
        BlockerKind::Unresolved => {
            "Confirm a choice for the group, or build the resolved game groups only."
        }
        BlockerKind::ReadOnlyRoot => "Make the root writable, then plan again.",
        BlockerKind::FolderMissing => "Put the folder back, then plan again.",
        BlockerKind::Marker | BlockerKind::Recovery => "Do as it says, then plan again.",
        BlockerKind::Cleanup => {
            "Choose something else for it, or check the folder again, then plan again."
        }
        BlockerKind::Space => "Free space in the source set's root, then plan again.",
    }
}

/// A plan's `blockers`, in the plan shape; an unresolved group links to its choice in the selection `selection`.
pub fn plan_blockers(blockers: &[Blocker], selection: i64) -> Vec<PlanBlocker> {
    blockers
        .iter()
        .map(|b| PlanBlocker {
            what: blocker_label(b.kind).to_string(),
            item: b.target.clone().or_else(|| b.group.clone()),
            why: b.message.clone(),
            fix: blocker_fix(b.kind).to_string(),
            link: b
                .group
                .as_ref()
                .map(|g| (group_href(selection, g), "Confirm a choice".to_string())),
        })
        .collect()
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
    use stage_select_api::build::GameRemoval;
    use stage_select_api::build::{BuildSummary, Cleanup, KeepAside};
    use stage_select_api::selection::{SelectionPins, SelectionSummary};

    fn built(state: BuildState, applied: u64, failed: u64, items: &[ItemState]) -> BuildDetail {
        BuildDetail {
            build: BuildSummary {
                id: 4,
                folder: 7,
                plan: 9,
                job: 12,
                state,
                generation: 1,
                selection: 2,
                audit: 3,
                dat_revision: "abc".into(),
                started_at: "t".into(),
                finished_at: Some("t".into()),
                applied,
                failed,
                pending: 0,
                uncertain: 0,
                planned: PlanSummary::default(),
            },
            items: items
                .iter()
                .enumerate()
                .map(|(n, &state)| BuildItem {
                    target: format!("{n}.zip"),
                    game: format!("{n}"),
                    kind: ItemKind::Copy,
                    state,
                    error: None,
                })
                .collect(),
        }
    }

    /// The store's `a_source_that_fails_its_rehash_fails_only_its_item`: the job succeeds, yet one file failed.
    #[test]
    fn a_build_is_ready_only_when_every_file_it_set_out_to_write_is_in_place() {
        use ItemState::*;
        let clean = built(BuildState::Succeeded, 5, 0, &[Applied; 5]);
        assert!(ready(&clean));
        let rehash = built(
            BuildState::Succeeded,
            4,
            1,
            &[Applied, Failed, Applied, Applied, Applied],
        );
        assert!(!ready(&rehash), "a failed file keeps it from ready");
        let mut pending = clean.clone();
        pending.build.pending = 1;
        assert!(!ready(&pending));
        let mut uncertain = clean.clone();
        uncertain.build.uncertain = 1;
        assert!(!ready(&uncertain));
        let publishing = built(BuildState::Succeeded, 4, 0, &[Applied, Publishing]);
        assert!(!ready(&publishing));
        assert!(!ready(&built(BuildState::Failed, 5, 0, &[Applied; 5])));
    }

    #[test]
    fn a_last_build_names_its_failed_and_uncertain_files_and_links_to_them() {
        let mut detail = built(BuildState::Failed, 2, 1, &[]);
        detail.build.uncertain = 2;
        let lines = troubles(&detail.build);
        assert_eq!(
            lines,
            vec![
                Trouble {
                    state: ItemState::Failed,
                    text: "1 file failed".into(),
                    href: "/builds/4?state=failed".into(),
                },
                Trouble {
                    state: ItemState::Uncertain,
                    text: "2 files uncertain".into(),
                    href: "/builds/4?state=uncertain".into(),
                },
            ]
        );
        assert!(troubles(&built(BuildState::Succeeded, 5, 0, &[]).build).is_empty());
    }

    #[test]
    fn a_build_leads_with_its_counts_and_its_failures() {
        let item = |target: &str, state: ItemState| BuildItem {
            target: target.into(),
            game: target.into(),
            kind: ItemKind::Copy,
            state,
            error: None,
        };
        let mut detail = BuildDetail {
            build: BuildSummary {
                id: 4,
                folder: 7,
                plan: 9,
                job: 12,
                state: BuildState::Failed,
                generation: 0,
                selection: 2,
                audit: 3,
                dat_revision: "abc".into(),
                started_at: "t".into(),
                finished_at: Some("t".into()),
                applied: 2,
                failed: 1,
                pending: 1,
                uncertain: 0,
                planned: PlanSummary::default(),
            },
            items: vec![
                item("a.zip", ItemState::Applied),
                item("b.zip", ItemState::Pending),
                item("c.zip", ItemState::Failed),
                item("d.zip", ItemState::Applied),
            ],
        };
        let files = build_files(&detail);
        assert_eq!(
            files.counts,
            vec![
                (ItemState::Failed, 1),
                (ItemState::Pending, 1),
                (ItemState::Applied, 2)
            ],
            "uncertain and writing show only when a file is"
        );
        let order = |files: &BuildFiles| {
            files
                .rows
                .iter()
                .map(|(i, f)| (*i, f.target.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            order(&files),
            [
                (2, "c.zip".to_string()),
                (1, "b.zip".to_string()),
                (0, "a.zip".to_string()),
                (3, "d.zip".to_string())
            ],
            "failures first, written last, each keeping its place in the build"
        );

        detail.build.uncertain = 1;
        detail.items.push(item("e.zip", ItemState::Uncertain));
        detail.items.push(item("f.zip", ItemState::Publishing));
        let files = build_files(&detail);
        assert_eq!(
            files.counts,
            vec![
                (ItemState::Failed, 1),
                (ItemState::Uncertain, 1),
                (ItemState::Publishing, 1),
                (ItemState::Pending, 1),
                (ItemState::Applied, 2)
            ],
            "a file still being put in place is counted, not dropped"
        );
        assert_eq!(
            order(&files)
                .into_iter()
                .map(|(_, t)| t)
                .collect::<Vec<_>>(),
            ["c.zip", "e.zip", "f.zip", "b.zip", "a.zip", "d.zip"]
        );
    }

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
            removed: Vec::new(),
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
            remove: vec![],
            delete_folder: false,
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

    #[test]
    fn each_blocker_names_what_it_blocks_on_and_what_to_do() {
        let blockers: Vec<Blocker> = serde_json::from_value(serde_json::json!([
            { "kind": "cleanup", "message": "it changed since the check", "target": "notes.txt" },
            { "kind": "unresolved", "message": "Pong has two releases", "group": "Pong & co" },
            { "kind": "read_only_root", "message": "the root is read-only" },
        ]))
        .unwrap();
        let shown = plan_blockers(&blockers, 7);
        let items: Vec<_> = shown.iter().map(|b| b.item.as_deref()).collect();
        assert_eq!(items, [Some("notes.txt"), Some("Pong & co"), None]);
        assert_eq!(shown[0].what, "Cleanup");
        assert_eq!(shown[0].why, "it changed since the check");
        assert!(shown.iter().all(|b| !b.fix.is_empty()), "{shown:?}");
        assert_eq!(
            shown[1].link,
            Some((
                "/selections/7?group=Pong%20%26%20co".to_string(),
                "Confirm a choice".to_string()
            ))
        );
        assert_eq!(shown[2].link, None);
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
        assert_eq!(facts["Game"], "Orbit, returned whole");
        assert_eq!(
            facts["Needs in your source set"],
            "At most 64 KiB for the whole game, an upper bound"
        );
        assert_eq!(
            summary_of(&plan, BuildKind::Return) as u64,
            plan.summary().returned,
            "one row per game, as the summary counts them"
        );
    }

    #[test]
    fn a_return_takes_the_outcome_of_its_releases_item_and_never_a_files() {
        let detail: BuildDetail = serde_json::from_value(serde_json::json!({
            "build": {
                "id": 1, "folder": 1, "plan": 1, "job": 1, "state": "succeeded", "generation": 2,
                "selection": 1, "audit": 1, "dat_revision": "r", "started_at": "now", "finished_at": null,
                "applied": 0, "failed": 0, "pending": 0, "uncertain": 0,
                "planned": serde_json::to_value(stage_select_api::build::PlanSummary::default()).unwrap(),
            },
            "items": [
                { "target": "Combat (USA).zip", "game": "Combat", "kind": "copy", "state": "failed" },
                { "target": "Combat (USA).zip", "game": "Combat", "kind": "remove", "state": "uncertain" },
                { "target": "Combat (USA)", "game": "Combat", "kind": "return", "state": "applied" },
            ],
        }))
        .unwrap();
        let of = |built: Built| built_outcome(&built, &detail);
        assert_eq!(
            of(Built::Return("Combat (USA)".into())),
            Some(ItemOutcome::Done)
        );
        assert_eq!(
            of(Built::Item("Combat (USA).zip".into())),
            Some(ItemOutcome::Failed)
        );
        assert_eq!(
            of(Built::Removal("Combat (USA).zip".into())),
            Some(ItemOutcome::Uncertain)
        );
        assert_eq!(
            of(Built::Return("Combat (USA).zip".into())),
            None,
            "a placed file's name is not its release"
        );
    }

    #[test]
    fn the_summary_line_counts_a_build_plans_rows_by_what_they_do() {
        let plan = content(
            vec![
                item("Pong", ItemKind::Unchanged),
                item("Combat", ItemKind::Copy),
                item("Outlaw", ItemKind::Conflict),
                item("Air", ItemKind::Copy),
            ],
            Vec::new(),
        );
        assert_eq!(
            plan_line(&plan),
            "Builds 2 files, keeps 1 file as it is, finds 1 file in the way"
        );
        assert_eq!(
            plan_line(&content(Vec::new(), Vec::new())),
            "Changes nothing"
        );
    }

    #[test]
    fn deleting_a_folder_says_what_it_deletes_returns_and_leaves() {
        let mut plan = content(Vec::new(), Vec::new());
        assert_eq!(folder_deletion_notice(&plan), None);
        let kept: KeepAside = serde_json::from_value(serde_json::json!({
            "reason": "removal", "name": "Pong.zip",
            "file": { "size": 1024, "mtime_ns": 0 }, "identity": { "dev": 1, "ino": 2 },
        }))
        .unwrap();
        plan.removals = vec![PlanRemoval {
            target: "Pong.zip".into(),
            game: "Pong".into(),
            kind: RemovalKind::Keep,
            reason: String::new(),
            keep: Some(kept),
            converts_to: None,
        }];
        plan.folder_deletion = Some(serde_json::from_value(serde_json::json!({
            "kept": [{
                "id": 1, "folder": 1, "generation": 1, "name": "Combat.zip", "kept_name": "Combat.zip.kept",
                "reason": "removal", "build": 1, "file": { "size": 2048, "mtime_ns": 0 },
                "identity": { "dev": 1, "ino": 3 }, "kept_at": "t",
            }],
        })).unwrap());
        let said = folder_deletion_notice(&plan).unwrap();
        assert_eq!(
            said.deletes,
            "Approving deletes 2 files Stage Select wrote here (3.0 KiB) for good: 1 file built, set aside first, and \
             1 file you kept aside before."
        );
        assert_eq!(said.returns, None);
        assert!(said.folder.contains("removed"), "{}", said.folder);

        plan.folder_deletion.as_mut().unwrap().left = vec![FolderLeftover {
            name: "saves/pong.sav".into(),
            why: "Stage Select didn't write it".into(),
        }];
        let said = folder_deletion_notice(&plan).unwrap();
        assert!(
            said.folder
                .starts_with("The folder itself stays, with the file below"),
            "{}",
            said.folder
        );
        assert_eq!(said.left.len(), 1);
    }

    #[test]
    fn a_finished_folder_deletion_says_whether_the_folder_is_gone() {
        let mut outcome = FolderDeletionOutcome {
            finished_at: None,
            removed: true,
            left: vec![],
            detail: None,
        };
        assert_eq!(
            folder_deletion_outcome(&outcome),
            None,
            "waits on its build"
        );
        outcome.finished_at = Some("t".into());
        let (done, said) = folder_deletion_outcome(&outcome).unwrap();
        assert!(done && said.starts_with("Deleted the folder"), "{said}");
        outcome.removed = false;
        outcome.left = vec![FolderLeftover {
            name: "notes.txt".into(),
            why: "Stage Select didn't write it".into(),
        }];
        let (done, said) = folder_deletion_outcome(&outcome).unwrap();
        assert!(!done && said.contains("holding 1 file"), "{said}");
        outcome.left.clear();
        outcome.detail =
            Some("its build didn't finish, so nothing of the folder was deleted".into());
        let (_, said) = folder_deletion_outcome(&outcome).unwrap();
        assert_eq!(
            said,
            "The folder wasn't removed. Its build didn't finish, so nothing of the folder was deleted."
        );
    }

    #[test]
    fn a_plan_that_removes_games_says_how_and_warns_of_a_deletion() {
        assert_eq!(removal_notice(&PlanChoices::default()), None);
        let mut choices = PlanChoices {
            remove: vec![GameRemoval {
                game: "Pong (USA)".into(),
                action: RemoveAction::SetAside,
            }],
            ..PlanChoices::default()
        };
        let said = removal_notice(&choices).unwrap();
        assert!(said.contains("Pong (USA) (set aside)"), "{said}");
        assert!(!said.contains("lost"), "{said}");
        choices.remove.push(GameRemoval {
            game: "Joust (USA)".into(),
            action: RemoveAction::Delete,
        });
        let said = removal_notice(&choices).unwrap();
        assert!(said.contains("Joust (USA) (deleted)"), "{said}");
        assert!(
            said.contains("is lost, not returned to your source set"),
            "{said}"
        );
    }
}
