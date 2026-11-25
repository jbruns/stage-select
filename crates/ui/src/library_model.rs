//! What the Library Folder screens decide without rendering: a folder's status, the selection a new plan builds,
//! the page's tabs, and the words for why a folder is stale.

use stage_select_api::build::{BuiltFile, IncompleteCounts, LibraryFolder, RemoveAction, Stale};
use stage_select_api::selection::ARCADE_COVERAGE_REFUSAL;
use stage_select_api::storage::release_of;
use stage_select_api::{Job, JobKind, JobState, SelectionRecord, SourceSetSystem};

use crate::coverage_model::label_in_sentence;
use crate::model::earlier;
use crate::parts::status::{LibraryFacts, LibraryStatus, Placement, PlanState, by_attention};

/// Whether the folder's latest check, made since its current build, found it differs from that build.
pub fn drifted(folder: &LibraryFolder) -> bool {
    let (Some(check), Some(build)) = (&folder.check, &folder.build) else {
        return false;
    };
    let built = build.finished_at.as_deref().unwrap_or(&build.started_at);
    !earlier(&check.at, built)
        && (check.marker.is_some()
            || check.repacked + check.conflict + check.missing + check.unreadable + check.drift > 0)
}

/// Where the folder's plan is, for its Plan tab's icon: being made while you wait (`planning`), carried out by a build
/// (`building`), or waiting on your approval (`open`).
pub fn plan_state(planning: bool, building: bool, open: bool) -> Option<PlanState> {
    if planning {
        Some(PlanState::Proposing)
    } else if building {
        Some(PlanState::Running)
    } else if open {
        Some(PlanState::Open)
    } else {
        None
    }
}

/// What is true of the folder.
pub fn facts(folder: &LibraryFolder) -> LibraryFacts {
    LibraryFacts {
        running: folder.active_job.is_some(),
        plan_open: folder.plan_open,
        drift: drifted(folder),
        incomplete: folder
            .incomplete
            .as_ref()
            .is_some_and(IncompleteCounts::makes_incomplete)
            || folder.unfinished.is_some(),
        stale: !folder.stale.is_empty(),
        // A first build that failed or was interrupted wrote some files: the folder is incomplete, not never built.
        built: folder.build.is_some() || folder.unfinished.is_some(),
    }
}

/// Whether `job` is a build that finished: the one message READY may stand beside. A build you cancelled, or one that
/// failed or was interrupted, didn't finish, and a check isn't a build.
pub fn build_finished(job: &Job) -> bool {
    job.kind == JobKind::Build
        && job.state == JobState::Succeeded
        && job.cancel_requested_at.is_none()
}

/// The folder's row status.
pub fn status(folder: &LibraryFolder) -> LibraryStatus {
    LibraryStatus::of(facts(folder))
}

/// The folder's row status, Running too while any of `jobs` not yet ended works on it: the live job book knows of a
/// job started anywhere before the folder is fetched again.
pub fn live_status(folder: &LibraryFolder, jobs: &[Job]) -> LibraryStatus {
    let working = jobs
        .iter()
        .any(|j| j.library_folder == Some(folder.id) && !j.state.is_final());
    LibraryStatus::of(LibraryFacts {
        running: working || folder.active_job.is_some(),
        ..facts(folder)
    })
}

/// Where the folder is: its root and its path in it.
pub fn location(folder: &LibraryFolder) -> String {
    format!("{}: {}", folder.root, folder.path)
}

/// Every folder with its live row status, in the list's default order: what needs attention first, then by where it
/// is. The Library folders badge counts the same statuses.
pub fn checked_in(folders: &[LibraryFolder], jobs: &[Job]) -> Vec<(LibraryStatus, LibraryFolder)> {
    let mut rows: Vec<(LibraryStatus, String, LibraryFolder)> = folders
        .iter()
        .map(|f| (live_status(f, jobs), location(f), f.clone()))
        .collect();
    rows.sort_by(|(a, x, _), (b, y, _)| by_attention((*a, x), (*b, y)));
    rows.into_iter().map(|(s, _, f)| (s, f)).collect()
}

/// The oldest selection job not yet ended for the folder's source set and profile that runs the profile's `newest`
/// version on the `verified` DAT revision (the one the source set was last audited against), which a new plan waits
/// for rather than starting another. A job pinned to anything older makes an older selection, so it isn't followed.
pub fn selection_under_way(
    jobs: &[Job],
    folder: &LibraryFolder,
    newest: Option<u32>,
    verified: Option<&str>,
) -> Option<i64> {
    jobs.iter()
        .filter(|j| {
            j.kind == JobKind::Selection
                && !j.state.is_final()
                && j.source_set == folder.source_set
                && j.profile.as_ref().is_some_and(|p| {
                    p.name == folder.profile && newest.is_some_and(|v| p.version == v)
                })
                && verified.is_none_or(|v| j.dat_revision == v)
        })
        .map(|j| j.id)
        .min()
}

/// The selection a new build plan of a folder builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    /// Made with the profile's newest version from the DAT revision the source set was last audited against.
    Fresh(i64),
    /// Only an older one: make a fresh one, or offer this as an explicit choice when that fails.
    Older(i64),
    None,
}

/// Whether a new build plan of the folder may build `selection` without asking: made for the folder's source set
/// with its profile's newest version, from the DAT revision the source set was last audited against.
pub fn fresh(selection: &SelectionRecord, folder: &LibraryFolder, verified: Option<&str>) -> bool {
    !selection.pruned
        && selection.source_set == folder.source_set
        && selection.profile.name == folder.profile
        && selection.profile.version == selection.latest_profile_version
        && verified.is_none_or(|v| selection.dat_revision_sha256 == v)
}

pub fn pick(
    selections: &[SelectionRecord],
    folder: &LibraryFolder,
    verified: Option<&str>,
) -> Pick {
    let usable = || {
        selections.iter().filter(|s| {
            !s.pruned && s.source_set == folder.source_set && s.profile.name == folder.profile
        })
    };
    let newest = usable()
        .filter(|s| fresh(s, folder, verified))
        .map(|s| s.id)
        .max();
    match (newest, usable().map(|s| s.id).max()) {
        (Some(id), _) => Pick::Fresh(id),
        (None, Some(id)) => Pick::Older(id),
        (None, None) => Pick::None,
    }
}

/// Why a folder is stale, in a sentence.
pub fn stale_label(stale: &Stale) -> String {
    match stale {
        Stale::SourceSetRebound { built } => {
            format!("You bound it to another source set; its build is of {built}.")
        }
        Stale::SourceSetChanged { built, latest } => format!(
            "Its source set changed: audit #{latest} found something other than audit #{built}, which its build used."
        ),
        Stale::ProfileRebound { built } => {
            format!("You bound it to another selection profile; its build used {built}.")
        }
        Stale::ProfileSaved { built, latest } => {
            format!("Its selection profile is at version {latest}; its build used version {built}.")
        }
        Stale::CoverageChanged { built, latest } => format!(
            "Coverage changed: its source set covers {} now; its build was selected under {}.",
            label_in_sentence(latest.as_deref()),
            label_in_sentence(built.as_deref()),
        ),
        Stale::DatChanged { built } => format!(
            "You bound its source set to another DAT; its build is of {built}. Build a selection of the new DAT."
        ),
    }
}

/// Why a Library Folder bound to `source_set` has no Coverage to inherit: an Arcade DAT's Source Set covers every Game
/// (ADR 0027), as `systems` (the service's `/systems`) says of it. `None` for any other, a MAME Logiqx DAT's included,
/// and for a Source Set `systems` doesn't list.
pub fn no_coverage(systems: &[SourceSetSystem], source_set: &str) -> Option<&'static str> {
    systems
        .iter()
        .any(|s| s.source_set == source_set && s.arcade)
        .then_some(ARCADE_COVERAGE_REFUSAL)
}

/// What a MAME Library Folder was built for, "MAME 0.289", from its Selection's ListXML build, such as `0.289
/// (mame0289)`.
pub fn built_for(build: &str) -> String {
    format!("MAME {}", build.split_whitespace().next().unwrap_or(build))
}

/// Which emulators run a MAME Library Folder (ADR 0026).
pub const BUILT_FOR_NOTE: &str = "Only an emulator of that MAME version runs what this folder holds, such as \
RetroArch's current MAME core or standalone MAME.";

/// The sentence on RomM's exclusions up to the setting's name, `exclude.roms.multi_file.names`: why RomM needs a MAME
/// Library Folder's Disk folders left out.
pub const ROMM_BEFORE_SETTING: &str = "RomM takes every folder in a platform for a game of its own, so without this it shows \
each game with disks twice: once for its ZIP, and once for its disk folder, which doesn't run. Add these names to";

/// The rest of that sentence after the setting's name: what to do then, and that RomM's list holds for every platform.
pub const ROMM_AFTER_SETTING: &str = "in your RomM configuration, then scan the platform again. RomM keeps that setting \
as one list for every platform, so it leaves out a folder of these names on every platform, not only this one. Stage \
Select doesn't change RomM's configuration for you.";

/// The upstream request for RomM to take exclusions per platform.
pub const ROMM_PER_PLATFORM: &str = "https://github.com/rommapp/romm/issues/5696";

/// What to merge into RomM's `config.yml` so that it leaves out `folders`, a MAME library folder's disk folders, which
/// it would otherwise list as games of their own: each under `exclude.roms.multi_file.names`, quoted.
pub fn romm_exclusions(folders: &[String]) -> String {
    let mut out = String::from("exclude:\n  roms:\n    multi_file:\n      names:\n");
    for folder in folders {
        out += &format!("        - '{}'\n", folder.replace('\'', "''"));
    }
    out
}

/// A tab of a Library Folder's page, named in its URL's `tab`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderTab {
    Overview,
    Selection,
    Plan,
    Builds,
    Drift,
    Kept,
    Romm,
}

impl FolderTab {
    pub const ALL: [FolderTab; 7] = [
        FolderTab::Overview,
        FolderTab::Selection,
        FolderTab::Plan,
        FolderTab::Builds,
        FolderTab::Drift,
        FolderTab::Kept,
        FolderTab::Romm,
    ];

    /// The tab a URL's `tab` names; Overview for none, or for one it doesn't know.
    pub fn parse(tab: Option<&str>) -> Self {
        Self::ALL
            .into_iter()
            .find(|t| Some(t.key()) == tab)
            .unwrap_or(FolderTab::Overview)
    }

    pub fn key(self) -> &'static str {
        match self {
            FolderTab::Overview => "overview",
            FolderTab::Selection => "selection",
            FolderTab::Plan => "plan",
            FolderTab::Builds => "builds",
            FolderTab::Drift => "drift",
            FolderTab::Kept => "kept",
            FolderTab::Romm => "romm",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            FolderTab::Overview => "Overview",
            FolderTab::Selection => "Selection",
            FolderTab::Plan => "Plan",
            FolderTab::Builds => "Builds",
            FolderTab::Drift => "Drift",
            FolderTab::Kept => "Kept files",
            FolderTab::Romm => "RomM",
        }
    }
}

/// The plans asked for in turn, to tell the answer to the latest from those it superseded.
#[derive(Debug, Default)]
pub struct Replans {
    seq: u64,
}

/// One plan asked for, to check its answer against.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Replan(u64);

impl Replans {
    /// Asks for a plan, superseding every one asked for before.
    pub fn ask(&mut self) -> Replan {
        self.seq += 1;
        Replan(self.seq)
    }

    /// Whether `replan` is the latest asked for, so its answer is the one to show.
    pub fn is_latest(&self, replan: Replan) -> bool {
        replan.0 == self.seq
    }
}

/// The games of the built files you checked, to remove from the folder: each once, by name, and which of them are
/// placed there as their source set's only copy. `unknown` while that isn't known for one of them yet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChosenGames {
    pub games: Vec<String>,
    pub placed: Vec<String>,
    pub unknown: bool,
}

impl ChosenGames {
    /// From each checked file's game and whether it is placed: a game is placed when any of its files is.
    pub fn of<'a>(files: impl IntoIterator<Item = (&'a str, Option<Placement>)>) -> ChosenGames {
        let mut chosen = ChosenGames::default();
        for (game, placement) in files {
            if !chosen.games.iter().any(|g| g == game) {
                chosen.games.push(game.to_owned());
            }
            match placement {
                Some(Placement::Placed) if !chosen.placed.iter().any(|g| g == game) => {
                    chosen.placed.push(game.to_owned())
                }
                Some(Placement::Unknown) => chosen.unknown = true,
                _ => {}
            }
        }
        chosen.games.sort();
        chosen.placed.sort();
        chosen
    }

    /// As [`ChosenGames::of`], from each checked file's game, whether it is placed, and whether it is a generated
    /// playlist ([`is_generated_playlist`]): a playlist, whose game is its release's name, stands for its discs, the
    /// games among `all` the folder's built files whose release it is. Any other file stands for its own game.
    pub fn of_checked<'a>(
        checked: impl IntoIterator<Item = (&'a str, Option<Placement>, bool)>,
        all: &[(&'a str, Option<Placement>)],
    ) -> ChosenGames {
        ChosenGames::of(checked.into_iter().flat_map(|(game, placement, playlist)| {
            let discs: Vec<(&str, Option<Placement>)> = match playlist {
                true => all
                    .iter()
                    .filter(|(other, _)| *other != game && release_of(other) == game)
                    .copied()
                    .collect(),
                false => Vec::new(),
            };
            match discs.is_empty() {
                true => vec![(game, placement)],
                false => discs,
            }
        }))
    }

    fn built_only(&self) -> Vec<&str> {
        self.games
            .iter()
            .filter(|g| !self.placed.contains(g))
            .map(String::as_str)
            .collect()
    }
}

/// Whether built file `file` is the `.m3u` a build generates to list a multi-disc game's discs: a file of no ROM,
/// named for its release.
pub fn is_generated_playlist(file: &BuiltFile) -> bool {
    file.roms.is_empty() && file.target.to_ascii_lowercase().ends_with(".m3u")
}

/// `games` in a sentence: the name of one, or how many.
fn games_in_sentence(games: &[&str]) -> String {
    match games {
        [one] => one.to_string(),
        many => format!("{} of these games", many.len()),
    }
}

/// What the button that removes games with `action` says.
pub fn removal_label(action: RemoveAction) -> &'static str {
    match action {
        RemoveAction::SendBack => "Send back to source",
        RemoveAction::SetAside => "Set aside",
        RemoveAction::Delete => "Delete",
    }
}

/// How a game was removed, in the folder's list of removed games.
pub fn removed_label(action: RemoveAction) -> &'static str {
    match action {
        RemoveAction::SendBack => "Sent back",
        RemoveAction::SetAside => "Set aside",
        RemoveAction::Delete => "Deleted",
    }
}

/// Why removing `chosen` with `action` can't be planned, or none when it can.
pub fn removal_refusal(action: RemoveAction, chosen: &ChosenGames) -> Option<String> {
    if chosen.games.is_empty() {
        return Some("Check the files of the games you want to remove.".into());
    }
    if chosen.unknown {
        return Some(
            "Whether these games are their only copies is still being read; try again once it is."
                .into(),
        );
    }
    let placed: Vec<&str> = chosen.placed.iter().map(String::as_str).collect();
    let built = chosen.built_only();
    match action {
        RemoveAction::SendBack if !built.is_empty() => Some(format!(
            "{} {} placed here from your source set, so there is nothing to send back. Set {} aside or delete {}.",
            games_in_sentence(&built),
            if built.len() == 1 { "isn't" } else { "aren't" },
            if built.len() == 1 { "it" } else { "them" },
            if built.len() == 1 { "it" } else { "them" },
        )),
        RemoveAction::SetAside if !placed.is_empty() => Some(format!(
            "{} {} placed here as your source set's only copy, so {} can't be set aside. Send {} back to your source \
             set, or delete {}.",
            games_in_sentence(&placed),
            if placed.len() == 1 { "is" } else { "are" },
            if placed.len() == 1 { "it" } else { "they" },
            if placed.len() == 1 { "it" } else { "them" },
            if placed.len() == 1 { "it" } else { "them" },
        )),
        _ => None,
    }
}

/// What removing games with `action` does to their files, for you to read before you plan it.
pub fn removal_result(action: RemoveAction, chosen: &ChosenGames) -> String {
    let placed: Vec<&str> = chosen.placed.iter().map(String::as_str).collect();
    match action {
        RemoveAction::SendBack => {
            "Each game goes back to your source set's folder, as a build returns a placed game."
                .into()
        }
        RemoveAction::SetAside => {
            "Their files are set aside as kept files, which you can restore later.".into()
        }
        RemoveAction::Delete if placed.is_empty() => {
            "Their files are set aside, then deleted once the build has set them aside.".into()
        }
        RemoveAction::Delete => format!(
            "Their files are set aside, then deleted once the build has set them aside. {} {} placed here as your \
             source set's only copy: deleting {} loses {}, and nothing goes back to your source set.",
            games_in_sentence(&placed),
            if placed.len() == 1 { "is" } else { "are" },
            if placed.len() == 1 { "it" } else { "them" },
            if placed.len() == 1 {
                "the game"
            } else {
                "those games"
            },
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stage_select_api::build::{
        BuildState, BuildSummary, CheckSummary, PlanSummary, Stale, UnfinishedBuild,
    };
    use stage_select_api::selection::{SelectionPins, SelectionSummary};
    use stage_select_api::{ProfileVersionRef, Workers};

    use crate::parts::status::needing_a_look;

    fn folder() -> LibraryFolder {
        LibraryFolder {
            id: 1,
            root: "play".into(),
            path: "atari2600".into(),
            system: "No-Intro/Atari - Atari 2600".into(),
            source_set: "Atari".into(),
            profile: "Mine".into(),
            generation: 0,
            created_at: "2026-01-01T00:00:00Z".into(),
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

    fn built(finished: &str) -> LibraryFolder {
        LibraryFolder {
            generation: 1,
            build: Some(BuildSummary {
                id: 3,
                folder: 1,
                plan: 2,
                job: 4,
                state: BuildState::Succeeded,
                generation: 1,
                selection: 5,
                audit: 6,
                dat_revision: "ab".into(),
                started_at: "2026-01-02T00:00:00Z".into(),
                finished_at: Some(finished.into()),
                applied: 1,
                failed: 0,
                pending: 0,
                uncertain: 0,
                planned: PlanSummary::default(),
            }),
            ..folder()
        }
    }

    fn checked(at: &str, missing: u64) -> CheckSummary {
        CheckSummary {
            id: 7,
            at: at.into(),
            job: None,
            marker: None,
            unchanged: 1,
            refreshed: 0,
            repacked: 0,
            conflict: 0,
            missing,
            unreadable: 0,
            hashed: 0,
            drift: 0,
            acknowledged: 0,
        }
    }

    #[test]
    fn a_folder_is_never_built_then_current_then_stale_and_drift_only_from_a_check_since_its_build()
    {
        assert_eq!(status(&folder()), LibraryStatus::NeverBuilt);
        let built = built("2026-01-03T00:00:00Z");
        assert_eq!(status(&built), LibraryStatus::Current);
        let stale = LibraryFolder {
            stale: vec![Stale::ProfileSaved {
                built: 1,
                latest: 2,
            }],
            ..built.clone()
        };
        assert_eq!(status(&stale), LibraryStatus::Stale);
        let before = LibraryFolder {
            check: Some(checked("2026-01-02T12:00:00Z", 1)),
            ..stale.clone()
        };
        assert_eq!(
            status(&before),
            LibraryStatus::Stale,
            "a check before the build says nothing of it"
        );
        let after = LibraryFolder {
            check: Some(checked("2026-01-04T00:00:00Z", 1)),
            ..stale.clone()
        };
        assert_eq!(status(&after), LibraryStatus::Drift);
        let clean = LibraryFolder {
            check: Some(checked("2026-01-04T00:00:00Z", 0)),
            ..built.clone()
        };
        assert_eq!(status(&clean), LibraryStatus::Current);
        let open = LibraryFolder {
            plan_open: true,
            ..after.clone()
        };
        assert_eq!(status(&open), LibraryStatus::PlanOpen);
        let running = LibraryFolder {
            active_job: Some(9),
            ..open
        };
        assert_eq!(status(&running), LibraryStatus::Running);
    }

    #[test]
    fn games_the_source_set_lacks_leave_a_folder_current_and_any_other_gap_makes_it_incomplete() {
        let lacking = |counts: IncompleteCounts| LibraryFolder {
            incomplete: Some(counts),
            ..built("2026-01-03T00:00:00Z")
        };
        let no_source = IncompleteCounts {
            no_source: 23,
            ..IncompleteCounts::default()
        };
        assert_eq!(status(&lacking(no_source.clone())), LibraryStatus::Current);
        let stale = LibraryFolder {
            stale: vec![Stale::ProfileSaved {
                built: 1,
                latest: 2,
            }],
            ..lacking(no_source.clone())
        };
        assert_eq!(status(&stale), LibraryStatus::Stale);
        for counts in [
            IncompleteCounts {
                failed: 1,
                ..no_source.clone()
            },
            IncompleteCounts {
                missing_dependency: 1,
                ..no_source.clone()
            },
            IncompleteCounts {
                missing_disc: 1,
                ..IncompleteCounts::default()
            },
        ] {
            assert_eq!(status(&lacking(counts)), LibraryStatus::Incomplete);
        }
    }

    fn selection(id: i64, set: &str, profile: &str, version: u32, sha: &str) -> SelectionRecord {
        SelectionRecord {
            id,
            source_set: set.into(),
            dat_revision_sha256: sha.into(),
            dat_revision_version: "20260101".into(),
            profile: ProfileVersionRef {
                name: profile.into(),
                version,
            },
            latest_profile_version: 2,
            job: None,
            system: "Atari - Atari 2600 (No-Intro)".into(),
            system_settings: false,
            pins: SelectionPins::default(),
            summary: SelectionSummary::default(),
            created_at: String::new(),
            pruned: false,
            library_folders: Vec::new(),
        }
    }

    #[test]
    fn a_plan_builds_the_newest_fresh_selection_of_the_folders_set_and_profile_or_else_the_newest_older_one()
     {
        let f = folder();
        let older = selection(4, "Atari", "Mine", 1, "aa");
        let other_set = selection(9, "Other", "Mine", 2, "aa");
        let other_profile = selection(8, "Atari", "Theirs", 2, "aa");
        let old_dat = selection(5, "Atari", "Mine", 2, "00");
        assert_eq!(pick(&[], &f, Some("aa")), Pick::None);
        assert_eq!(
            pick(
                &[
                    older.clone(),
                    other_set.clone(),
                    other_profile.clone(),
                    old_dat.clone()
                ],
                &f,
                Some("aa")
            ),
            Pick::Older(5)
        );
        let fresh = selection(6, "Atari", "Mine", 2, "aa");
        let newer_fresh = selection(7, "Atari", "Mine", 2, "aa");
        let pruned = SelectionRecord {
            pruned: true,
            ..selection(10, "Atari", "Mine", 2, "aa")
        };
        assert_eq!(
            pick(
                &[fresh, older, newer_fresh, old_dat, pruned, other_set],
                &f,
                Some("aa")
            ),
            Pick::Fresh(7)
        );
        assert_eq!(
            pick(&[selection(3, "Atari", "Mine", 2, "00")], &f, None),
            Pick::Fresh(3),
            "a source set never audited has no DAT revision to be fresh against"
        );
    }

    #[test]
    fn the_plan_tab_shows_a_plan_being_made_built_or_waiting_on_you() {
        assert_eq!(plan_state(false, false, false), None);
        assert_eq!(plan_state(false, false, true), Some(PlanState::Open));
        assert_eq!(plan_state(false, true, false), Some(PlanState::Running));
        assert_eq!(plan_state(true, false, true), Some(PlanState::Proposing));
    }

    #[test]
    fn a_tab_is_read_from_its_key_and_anything_else_opens_the_overview() {
        assert_eq!(FolderTab::parse(Some("plan")), FolderTab::Plan);
        assert_eq!(FolderTab::parse(Some("kept")), FolderTab::Kept);
        assert_eq!(FolderTab::parse(Some("Plan")), FolderTab::Overview);
        assert_eq!(FolderTab::parse(None), FolderTab::Overview);
    }

    fn job(id: i64, kind: JobKind, state: JobState) -> Job {
        Job {
            id,
            kind,
            state,
            source_set: "Atari".into(),
            dat_revision: "ab".repeat(32),
            actor: "operator".into(),
            attempt: 1,
            requeued_from: None,
            audit: None,
            profile: None,
            selection: None,
            library_folder: None,
            ingest_folder: None,
            repair: None,
            additions_set: None,
            cancel_requested_at: None,
            plan: None,
            evidence: None,
            error: None,
            romm_scan: None,
            build: None,
            throughput: None,
            workers: Workers {
                readers: 1,
                hashers: 1,
            },
            created_at: "2026-01-01T00:00:00Z".into(),
            started_at: None,
            finished_at: None,
            version: 1,
        }
    }

    #[test]
    fn only_a_build_that_finished_is_ready() {
        assert!(build_finished(&job(1, JobKind::Build, JobState::Succeeded)));
        for state in [
            JobState::Queued,
            JobState::Running,
            JobState::Failed,
            JobState::Interrupted,
        ] {
            assert!(!build_finished(&job(1, JobKind::Build, state)), "{state:?}");
        }
        let cancelled = Job {
            cancel_requested_at: Some("2026-01-01T00:00:00Z".into()),
            ..job(1, JobKind::Build, JobState::Succeeded)
        };
        assert!(!build_finished(&cancelled));
        assert!(!build_finished(&job(
            1,
            JobKind::Check,
            JobState::Succeeded
        )));
    }

    fn selection_job(id: i64, state: JobState, set: &str, profile: &str) -> Job {
        Job {
            source_set: set.into(),
            profile: Some(ProfileVersionRef {
                name: profile.into(),
                version: 2,
            }),
            ..job(id, JobKind::Selection, state)
        }
    }

    #[test]
    fn a_folder_follows_a_selection_already_under_way_only_when_it_runs_the_newest_profile_on_the_latest_revision()
     {
        let latest = "ab".repeat(32);
        let older_profile = Job {
            profile: Some(ProfileVersionRef {
                name: "Mine".into(),
                version: 1,
            }),
            ..selection_job(4, JobState::Queued, "Atari", "Mine")
        };
        let older_revision = Job {
            dat_revision: "00".repeat(32),
            ..selection_job(3, JobState::Queued, "Atari", "Mine")
        };
        let jobs = [
            selection_job(9, JobState::Running, "Atari", "Other"),
            selection_job(8, JobState::Running, "Other", "Mine"),
            selection_job(7, JobState::Succeeded, "Atari", "Mine"),
            selection_job(6, JobState::Queued, "Atari", "Mine"),
            Job {
                library_folder: Some(1),
                ..job(5, JobKind::Check, JobState::Running)
            },
            older_profile,
            older_revision,
        ];
        let under_way = |jobs: &[Job], newest: u32| {
            selection_under_way(jobs, &folder(), Some(newest), Some(&latest))
        };
        assert_eq!(under_way(&jobs, 2), Some(6));
        assert_eq!(under_way(&jobs[..3], 2), None);
        assert_eq!(under_way(&jobs[5..], 2), None);
        assert_eq!(under_way(&jobs, 3), None, "the profile was saved since");
        assert_eq!(
            selection_under_way(&jobs, &folder(), None, Some(&latest)),
            None
        );
    }

    #[test]
    fn a_selection_a_followed_job_made_is_fresh_only_with_the_newest_profile_on_the_latest_revision()
     {
        let f = folder();
        assert!(fresh(
            &selection(4, "Atari", "Mine", 2, "aa"),
            &f,
            Some("aa")
        ));
        assert!(fresh(&selection(4, "Atari", "Mine", 2, "aa"), &f, None));
        assert!(!fresh(
            &selection(4, "Atari", "Mine", 1, "aa"),
            &f,
            Some("aa")
        ));
        assert!(!fresh(
            &selection(4, "Atari", "Mine", 2, "00"),
            &f,
            Some("aa")
        ));
        assert!(!fresh(
            &selection(4, "Atari", "Theirs", 2, "aa"),
            &f,
            Some("aa")
        ));
    }

    #[test]
    fn a_folder_runs_while_any_unfinished_job_works_on_it_even_before_it_is_fetched_again() {
        let check = |state| Job {
            library_folder: Some(1),
            ..job(5, JobKind::Check, state)
        };
        let elsewhere = Job {
            library_folder: Some(2),
            ..job(6, JobKind::Check, JobState::Running)
        };
        assert_eq!(
            live_status(&folder(), &[check(JobState::Queued), elsewhere.clone()]),
            LibraryStatus::Running
        );
        assert_eq!(
            live_status(&folder(), &[check(JobState::Succeeded), elsewhere]),
            LibraryStatus::NeverBuilt
        );
    }

    fn at(id: i64, path: &str) -> LibraryFolder {
        LibraryFolder {
            id,
            path: path.into(),
            ..built("2026-01-03T00:00:00Z")
        }
    }

    #[test]
    fn folders_with_drift_or_a_failed_build_come_first_and_are_the_ones_counted() {
        let drifted = LibraryFolder {
            check: Some(checked("2026-01-04T00:00:00Z", 2)),
            ..at(2, "nes")
        };
        let failed_first_build = LibraryFolder {
            unfinished: Some(UnfinishedBuild {
                build: 9,
                state: BuildState::Failed,
                applied: 3,
                uncertain: 0,
                not_written: 4,
                kept: 0,
            }),
            ..at(3, "snes")
        };
        let failed_first_build = LibraryFolder {
            build: None,
            ..failed_first_build
        };
        let never = LibraryFolder {
            id: 4,
            path: "a2600".into(),
            ..folder()
        };
        let folders = [
            at(1, "gb"),
            never,
            failed_first_build,
            drifted,
            at(5, "atari"),
        ];
        let working = Job {
            library_folder: Some(5),
            ..job(8, JobKind::Check, JobState::Running)
        };
        let rows = checked_in(&folders, &[working]);
        let order: Vec<(&str, LibraryStatus)> =
            rows.iter().map(|(s, f)| (f.path.as_str(), *s)).collect();
        assert_eq!(
            order,
            [
                ("atari", LibraryStatus::Running),
                ("nes", LibraryStatus::Drift),
                ("snes", LibraryStatus::Incomplete),
                ("gb", LibraryStatus::Current),
                ("a2600", LibraryStatus::NeverBuilt),
            ]
        );
        assert_eq!(needing_a_look(rows.iter().map(|(s, _)| *s)), 2);
    }

    #[test]
    fn only_the_latest_plan_asked_for_is_shown() {
        let mut replans = Replans::default();
        let first = replans.ask();
        assert!(replans.is_latest(first));
        let second = replans.ask();
        assert!(!replans.is_latest(first));
        assert!(replans.is_latest(second));
    }

    #[test]
    fn a_coverage_change_says_what_the_build_was_selected_under() {
        let asia: Vec<String> = stage_select_api::coverage::preset("Asia")
            .unwrap()
            .regions
            .iter()
            .map(|r| (*r).to_owned())
            .collect();
        assert_eq!(
            stale_label(&Stale::CoverageChanged {
                built: None,
                latest: Some(asia),
            }),
            "Coverage changed: its source set covers Asia now; its build was selected under every region."
        );
    }

    #[test]
    fn a_mame_folders_disk_folders_are_listed_for_romms_multi_file_exclusions() {
        let folders = ["blaster".to_owned(), "it's".to_owned()];
        assert_eq!(
            romm_exclusions(&folders),
            "exclude:\n  roms:\n    multi_file:\n      names:\n        - 'blaster'\n        - 'it''s'\n"
        );
    }

    #[test]
    fn a_folder_of_an_arcade_source_set_inherits_no_coverage_and_says_why() {
        let systems = [
            SourceSetSystem {
                source_set: "mame".into(),
                system: Some("MAME/MAME".into()),
                arcade: true,
            },
            // A MAME Logiqx DAT is no Arcade DAT: its Source Set takes a Coverage.
            SourceSetSystem {
                source_set: "mame-logiqx".into(),
                system: Some("MAME/MAME".into()),
                arcade: false,
            },
        ];
        assert_eq!(no_coverage(&systems, "mame"), Some(ARCADE_COVERAGE_REFUSAL));
        assert_eq!(no_coverage(&systems, "mame-logiqx"), None);
        assert_eq!(no_coverage(&systems, "atari"), None);
    }

    #[test]
    fn a_mame_folder_says_which_mame_version_runs_it() {
        assert_eq!(built_for("0.289 (mame0289)"), "MAME 0.289");
        assert_eq!(built_for("0.289"), "MAME 0.289");
        for said in [
            "Only an emulator of that MAME version",
            "RetroArch's current MAME core",
            "standalone MAME",
        ] {
            assert!(BUILT_FOR_NOTE.contains(said), "{said}");
        }
    }

    #[test]
    fn the_romm_exclusions_say_what_happens_without_them_and_that_they_apply_everywhere() {
        assert!(ROMM_BEFORE_SETTING.contains("each game with disks twice"));
        assert!(ROMM_AFTER_SETTING.contains("one list for every platform"));
        assert!(ROMM_AFTER_SETTING.contains("on every platform, not only this one"));
    }

    fn chosen(files: &[(&str, Option<Placement>)]) -> ChosenGames {
        ChosenGames::of(files.iter().map(|(g, p)| (*g, *p)))
    }

    #[test]
    fn a_checked_m3u_stands_for_its_releases_discs() {
        let all = [
            ("Saga (USA)", None),
            ("Saga (USA) (Disc 1)", None),
            ("Saga (USA) (Disc 2)", Some(Placement::Placed)),
            ("Pong", None),
        ];
        let c = ChosenGames::of_checked([("Saga (USA)", None, true), ("Pong", None, false)], &all);
        assert_eq!(
            c.games,
            ["Pong", "Saga (USA) (Disc 1)", "Saga (USA) (Disc 2)"]
        );
        assert_eq!(c.placed, ["Saga (USA) (Disc 2)"]);
    }

    #[test]
    fn a_checked_game_named_like_a_release_is_itself_and_only_a_playlist_is_its_discs() {
        let all = [("Orbit (USA)", None), ("Orbit (USA) (Disc 1)", None)];
        let c = ChosenGames::of_checked([("Orbit (USA)", None, false)], &all);
        assert_eq!(c.games, ["Orbit (USA)"]);

        let file = |target: &str, roms: usize| BuiltFile {
            target: target.into(),
            game: "Saga (USA)".into(),
            kind: stage_select_api::build::ItemKind::Repack,
            build: 1,
            selection: 1,
            audit: 1,
            dat_revision: String::new(),
            sources: Vec::new(),
            roms: vec![
                stage_select_api::build::ExpectedRom {
                    name: "Saga (USA).m3u".into(),
                    size: 1,
                    crc: None,
                    md5: None,
                    sha1: None,
                    sha256: None,
                    header: None,
                };
                roms
            ],
            file: stage_select_api::build::FileStat {
                size: 1,
                mtime_ns: 0,
            },
            sha256: None,
            torrentzip: true,
        };
        assert!(is_generated_playlist(&file(
            "Saga (USA).m3u/Saga (USA).m3u",
            0
        )));
        assert!(!is_generated_playlist(&file("Saga (USA).m3u", 1)));
        assert!(!is_generated_playlist(&file("Saga (USA).zip", 0)));
    }

    #[test]
    fn chosen_games_are_counted_once_and_placed_when_any_file_is() {
        let c = chosen(&[
            ("Pong", None),
            ("Joust", None),
            ("Joust", Some(Placement::Placed)),
            ("Pong", None),
        ]);
        assert_eq!(c.games, ["Joust", "Pong"]);
        assert_eq!(c.placed, ["Joust"]);
        assert!(!c.unknown);
        assert!(chosen(&[("Pong", Some(Placement::Unknown))]).unknown);
    }

    #[test]
    fn each_removal_says_why_it_doesnt_apply_to_the_games_you_checked() {
        let built = chosen(&[("Pong", None)]);
        let placed = chosen(&[("Joust", Some(Placement::Placed))]);
        let both = chosen(&[
            ("Pong", None),
            ("Combat", None),
            ("Joust", Some(Placement::Placed)),
        ]);
        let refusal = |a, c: &ChosenGames| removal_refusal(a, c);

        assert_eq!(
            refusal(RemoveAction::SendBack, &built).as_deref(),
            Some(
                "Pong isn't placed here from your source set, so there is nothing to send back. Set it aside or delete it."
            )
        );
        assert_eq!(refusal(RemoveAction::SendBack, &placed), None);
        assert!(
            refusal(RemoveAction::SendBack, &both)
                .unwrap()
                .starts_with("2 of these games aren't placed"),
        );
        assert!(
            refusal(RemoveAction::SetAside, &placed)
                .unwrap()
                .starts_with("Joust is placed here")
        );
        assert_eq!(refusal(RemoveAction::SetAside, &built), None);
        for action in RemoveAction::ALL {
            assert_eq!(refusal(RemoveAction::Delete, &both), None);
            assert!(refusal(action, &ChosenGames::default()).is_some());
            assert!(refusal(action, &chosen(&[("Pong", Some(Placement::Unknown))])).is_some());
        }
    }

    #[test]
    fn deleting_a_placed_game_says_it_is_lost_not_returned() {
        let placed = chosen(&[("Joust", Some(Placement::Placed)), ("Pong", None)]);
        let said = removal_result(RemoveAction::Delete, &placed);
        assert!(
            said.contains("Joust is placed here as your source set's only copy"),
            "{said}"
        );
        assert!(
            said.contains("nothing goes back to your source set"),
            "{said}"
        );
        assert!(
            !removal_result(RemoveAction::Delete, &chosen(&[("Pong", None)])).contains("only copy")
        );
    }
}
