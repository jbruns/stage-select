//! The status vocabulary: each status the workbench shows has a glyph, a tone and a short label, so colour is
//! always paired with shape. A list shows its statuses with `StatusIcon` or `StatusBadge` and explains them with one
//! `Legend` (see `parts::legend`).

use leptos::prelude::*;
use stage_select_api::JobState;
use stage_select_api::additions::AdditionState;
use stage_select_api::build::{FileOutcome, ItemKind, ItemState};
use stage_select_api::ingest::IngestAction;
use stage_select_api::repair::RepairKind;
use stage_select_api::report::{Finding, GameStatus};
use stage_select_api::romm::{RommAdditionsOutcome, RommFlag, RommScanOutcome};
use stage_select_api::selection::{ChoiceStatus, Outcome};

use super::icon::{Glyph, Icon};
use crate::events::Stream;
use crate::model::Derived;

/// A status's colour, named by meaning. Each is a `--tone-*` token in `style/main.css` with a light and a dark value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tone {
    Good,
    Review,
    Failed,
    Active,
    Neutral,
}

impl Tone {
    pub const ALL: [Tone; 5] = [
        Tone::Good,
        Tone::Review,
        Tone::Failed,
        Tone::Active,
        Tone::Neutral,
    ];

    /// The class that colours an element with the tone.
    pub fn class(self) -> &'static str {
        match self {
            Tone::Good => "tone-good",
            Tone::Review => "tone-review",
            Tone::Failed => "tone-failed",
            Tone::Active => "tone-active",
            Tone::Neutral => "tone-neutral",
        }
    }

    /// The CSS custom property holding the tone's colour.
    pub fn token(self) -> &'static str {
        match self {
            Tone::Good => "--tone-good",
            Tone::Review => "--tone-review",
            Tone::Failed => "--tone-failed",
            Tone::Active => "--tone-active",
            Tone::Neutral => "--tone-neutral",
        }
    }
}

/// How a status looks: its glyph and tone, a label of three words or fewer for cells and legends, and a hint of one
/// sentence for its tooltip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Look {
    pub glyph: Glyph,
    pub tone: Tone,
    pub label: &'static str,
    pub hint: &'static str,
}

/// One closed set of statuses. `ALL` lists them in the order legends show them; for row statuses it is also their
/// precedence, highest first.
pub trait Status: Copy + Eq + Send + Sync + 'static {
    const ALL: &'static [Self];

    fn look(self) -> Look;

    /// Where the status comes in `ALL`.
    fn rank(self) -> usize {
        Self::ALL
            .iter()
            .position(|s| *s == self)
            .expect("every status is in ALL")
    }
}

/// The status of highest precedence among those that apply.
pub fn highest<S: Status>(applying: impl IntoIterator<Item = S>) -> Option<S> {
    applying.into_iter().min_by_key(|s| s.rank())
}

const fn look(glyph: Glyph, tone: Tone, label: &'static str, hint: &'static str) -> Look {
    Look {
        glyph,
        tone,
        label,
        hint,
    }
}

/// An Entry's Finding, with Verified for a Matched Entry read on Payload Evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FindingStatus {
    Verified,
    Matched,
    Misnamed,
    Conflicting,
    Mismatched,
    Unmatched,
    Unreadable,
}

impl FindingStatus {
    /// The status of an Entry with `finding`; only payload Evidence makes a Matched Entry Verified.
    pub fn of(finding: Finding, payload: bool) -> Self {
        match finding {
            Finding::Matched if payload => FindingStatus::Verified,
            Finding::Matched => FindingStatus::Matched,
            Finding::Misnamed => FindingStatus::Misnamed,
            Finding::Conflicting => FindingStatus::Conflicting,
            Finding::Mismatched => FindingStatus::Mismatched,
            Finding::Unmatched => FindingStatus::Unmatched,
            Finding::Unreadable => FindingStatus::Unreadable,
        }
    }
}

impl Status for FindingStatus {
    const ALL: &'static [Self] = &[
        FindingStatus::Verified,
        FindingStatus::Matched,
        FindingStatus::Misnamed,
        FindingStatus::Conflicting,
        FindingStatus::Mismatched,
        FindingStatus::Unmatched,
        FindingStatus::Unreadable,
    ];

    fn look(self) -> Look {
        match self {
            FindingStatus::Verified => look(
                Glyph::ShieldCheck,
                Tone::Good,
                "Verified",
                "Matched, and every byte of its payload was read.",
            ),
            FindingStatus::Matched => look(
                Glyph::CircleCheck,
                Tone::Good,
                "Matched",
                "Its payload matches the ROM its name gives.",
            ),
            FindingStatus::Misnamed => look(
                Glyph::Tag,
                Tone::Review,
                "Misnamed",
                "Its payload matches a ROM, but its name isn't one in the DAT revision.",
            ),
            FindingStatus::Conflicting => look(
                Glyph::ArrowLeftRight,
                Tone::Review,
                "Conflicting",
                "Its name is one ROM, but its payload matches another.",
            ),
            FindingStatus::Mismatched => look(
                Glyph::EqualNot,
                Tone::Review,
                "Mismatched",
                "Its name is a ROM, but its payload matches none. This isn't proof of damage.",
            ),
            FindingStatus::Unmatched => look(
                Glyph::CircleQuestionMark,
                Tone::Review,
                "Unmatched",
                "Neither its name nor its payload matches a ROM.",
            ),
            FindingStatus::Unreadable => look(
                Glyph::FileExclamationPoint,
                Tone::Failed,
                "Unreadable",
                "It couldn't be read. Its detail says why.",
            ),
        }
    }
}

impl From<Finding> for FindingStatus {
    fn from(finding: Finding) -> Self {
        FindingStatus::of(finding, false)
    }
}

impl Status for GameStatus {
    const ALL: &'static [Self] = &GameStatus::ALL;

    fn look(self) -> Look {
        match self {
            GameStatus::Complete => look(
                Glyph::CircleCheck,
                Tone::Good,
                "Complete",
                "Every ROM is present.",
            ),
            GameStatus::Incomplete => look(
                Glyph::CircleDashed,
                Tone::Review,
                "Incomplete",
                "Some of its ROMs are present.",
            ),
            GameStatus::Missing => look(
                Glyph::CircleOff,
                Tone::Neutral,
                "Missing",
                "None of its ROMs is present.",
            ),
            GameStatus::NothingToCheck => look(
                Glyph::CircleMinus,
                Tone::Neutral,
                "Nothing to check",
                "It lists no ROM you need that a file can match, so you have nothing to check.",
            ),
        }
    }
}

/// Where a Game is kept under Single Copy: in its Source Set's folder, or placed in a Library Folder as its only copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Placement {
    Placed,
    /// Whether a file is its Game's only copy isn't known: its Library Folder's Placed files are being read, or
    /// couldn't be.
    Unknown,
    SourceSet,
}

impl Status for Placement {
    const ALL: &'static [Self] = &[Placement::Placed, Placement::Unknown, Placement::SourceSet];

    fn look(self) -> Look {
        match self {
            Placement::Placed => look(
                Glyph::Library,
                Tone::Active,
                "Only copy",
                "Placed in a library folder under single copy: its file there is the game's only copy, so deleting it by hand loses the game.",
            ),
            Placement::Unknown => look(
                Glyph::CircleQuestionMark,
                Tone::Review,
                "Not known",
                "Whether this file is its game's only copy isn't known yet: treat it as one, and don't delete it by hand.",
            ),
            Placement::SourceSet => look(
                Glyph::Database,
                Tone::Neutral,
                "In source set",
                "Kept in your source set's own folder.",
            ),
        }
    }
}

/// A current condition of a Game apart from any Audit's verdict: a Build proved its placed file wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GameFlag {
    NeedsReview,
}

impl Status for GameFlag {
    const ALL: &'static [Self] = &[GameFlag::NeedsReview];

    fn look(self) -> Look {
        match self {
            GameFlag::NeedsReview => look(
                Glyph::TriangleAlert,
                Tone::Review,
                "Needs review",
                "A build found its placed file doesn't hold the game as the DAT lists it. Its audit status is as the audit found it; a full audit that verifies the file, or returning the game, clears this.",
            ),
        }
    }
}

/// Why a Source Set is Stale: each cause its own icon, with its tooltip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StaleCause {
    FilesChanged,
    NewerRevision,
}

impl Status for StaleCause {
    const ALL: &'static [Self] = &[StaleCause::FilesChanged, StaleCause::NewerRevision];

    fn look(self) -> Look {
        match self {
            StaleCause::FilesChanged => look(
                Glyph::FileDiff,
                Tone::Review,
                "Files changed",
                "A check found files changed, added or removed since its latest audit, by their sizes and times.",
            ),
            StaleCause::NewerRevision => look(
                Glyph::FileClock,
                Tone::Review,
                "Newer DAT revision",
                "You follow its DAT, and a revision newer than its latest audit's was imported.",
            ),
        }
    }
}

/// A Source Set's row status, highest precedence first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceSetStatus {
    Running,
    RepairOpen,
    NeedsReview,
    Stale,
    /// Audited with nothing to review, but some of it rests on Claim Evidence, which never makes it Verified.
    OnClaims,
    Verified,
    NeverAudited,
}

/// What is true of a Source Set, from which its row status follows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceSetFacts {
    /// A Job is queued or running for it.
    pub running: bool,
    /// A Repair plan is proposed and not yet approved, or approved and not yet finished.
    pub repair_open: bool,
    /// Its latest Audit has Needs Review Entries.
    pub needs_review: bool,
    pub stale: bool,
    /// Some Entries of its latest Audit rest on Claim Evidence, or it can't be told which.
    pub claims: bool,
    pub audited: bool,
}

impl SourceSetStatus {
    pub fn of(facts: SourceSetFacts) -> Self {
        let applying = [
            (facts.running, SourceSetStatus::Running),
            (facts.repair_open, SourceSetStatus::RepairOpen),
            (
                facts.audited && facts.needs_review,
                SourceSetStatus::NeedsReview,
            ),
            (facts.audited && facts.stale, SourceSetStatus::Stale),
            (facts.audited && facts.claims, SourceSetStatus::OnClaims),
            (facts.audited, SourceSetStatus::Verified),
            (true, SourceSetStatus::NeverAudited),
        ];
        highest(applying.into_iter().filter(|(a, _)| *a).map(|(_, s)| s))
            .unwrap_or(SourceSetStatus::NeverAudited)
    }
}

impl Status for SourceSetStatus {
    const ALL: &'static [Self] = &[
        SourceSetStatus::Running,
        SourceSetStatus::RepairOpen,
        SourceSetStatus::NeedsReview,
        SourceSetStatus::Stale,
        SourceSetStatus::OnClaims,
        SourceSetStatus::Verified,
        SourceSetStatus::NeverAudited,
    ];

    fn look(self) -> Look {
        match self {
            SourceSetStatus::Running => look(
                Glyph::LoaderCircle,
                Tone::Active,
                "Running",
                "A job is running for it.",
            ),
            SourceSetStatus::RepairOpen => look(
                Glyph::Wrench,
                Tone::Active,
                "Repair open",
                "A repair is planned or under way.",
            ),
            SourceSetStatus::NeedsReview => look(
                Glyph::TriangleAlert,
                Tone::Review,
                "Needs review",
                "Its latest audit found entries for you to review.",
            ),
            SourceSetStatus::Stale => look(
                Glyph::ClockAlert,
                Tone::Review,
                "Stale",
                "Its files look changed since its latest audit, or you follow its DAT and a newer revision is waiting.",
            ),
            SourceSetStatus::OnClaims => look(
                Glyph::FileQuestionMark,
                Tone::Review,
                "On claims",
                "Its latest audit found nothing to review, but some entries rest on claims taken from archive \
                 directories, not on payloads read. Audit it incrementally or fully to verify it.",
            ),
            SourceSetStatus::Verified => look(
                Glyph::ShieldCheck,
                Tone::Good,
                "Verified",
                "Its latest audit found nothing to review.",
            ),
            SourceSetStatus::NeverAudited => look(
                Glyph::ShieldQuestionMark,
                Tone::Neutral,
                "Never audited",
                "It hasn't been audited yet.",
            ),
        }
    }
}

/// A Library Folder's row status, highest precedence first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LibraryStatus {
    Running,
    PlanOpen,
    Drift,
    Incomplete,
    Stale,
    Current,
    NeverBuilt,
}

/// What is true of a Library Folder, from which its row status follows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LibraryFacts {
    /// A Job is queued or running for it.
    pub running: bool,
    /// A Build plan is proposed and not yet approved, or approved and not yet finished.
    pub plan_open: bool,
    pub drift: bool,
    pub incomplete: bool,
    pub stale: bool,
    pub built: bool,
}

impl LibraryStatus {
    pub fn of(facts: LibraryFacts) -> Self {
        let applying = [
            (facts.running, LibraryStatus::Running),
            (facts.plan_open, LibraryStatus::PlanOpen),
            (facts.built && facts.drift, LibraryStatus::Drift),
            (facts.built && facts.incomplete, LibraryStatus::Incomplete),
            (facts.built && facts.stale, LibraryStatus::Stale),
            (facts.built, LibraryStatus::Current),
            (true, LibraryStatus::NeverBuilt),
        ];
        highest(applying.into_iter().filter(|(a, _)| *a).map(|(_, s)| s))
            .unwrap_or(LibraryStatus::NeverBuilt)
    }
}

impl Status for LibraryStatus {
    const ALL: &'static [Self] = &[
        LibraryStatus::Running,
        LibraryStatus::PlanOpen,
        LibraryStatus::Drift,
        LibraryStatus::Incomplete,
        LibraryStatus::Stale,
        LibraryStatus::Current,
        LibraryStatus::NeverBuilt,
    ];

    fn look(self) -> Look {
        match self {
            LibraryStatus::Running => look(
                Glyph::LoaderCircle,
                Tone::Active,
                "Running",
                "A job is running for it.",
            ),
            LibraryStatus::PlanOpen => look(
                Glyph::ClipboardList,
                Tone::Active,
                "Plan open",
                "A build is planned or under way.",
            ),
            LibraryStatus::Drift => look(
                Glyph::FileDiff,
                Tone::Review,
                "Drift",
                "Its files differ from its current build.",
            ),
            LibraryStatus::Incomplete => look(
                Glyph::CircleDashed,
                Tone::Review,
                "Incomplete",
                "Its current build lacks games its selection chose.",
            ),
            LibraryStatus::Stale => look(
                Glyph::ClockAlert,
                Tone::Review,
                "Stale",
                "Its source set or selection profile changed since its build.",
            ),
            LibraryStatus::Current => look(
                Glyph::CircleCheck,
                Tone::Good,
                "Current",
                "It holds exactly its current build.",
            ),
            LibraryStatus::NeverBuilt => look(
                Glyph::CircleDotDashed,
                Tone::Neutral,
                "Never built",
                "It hasn't been built yet.",
            ),
        }
    }
}

/// The kind of a plan item, shown beside its *current → proposed* row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlanKind {
    Rename,
    Repack,
    Fill,
    Replace,
    SetAside,
    Convert,
}

impl From<RepairKind> for PlanKind {
    fn from(kind: RepairKind) -> Self {
        match kind {
            // Resolving a Conflicting Entry writes its payload under the name of the ROM it matches.
            RepairKind::Rename | RepairKind::Conflict => PlanKind::Rename,
            RepairKind::Repack => PlanKind::Repack,
            RepairKind::Fill => PlanKind::Fill,
            RepairKind::Replace => PlanKind::Replace,
            RepairKind::KeepAside => PlanKind::SetAside,
            RepairKind::Convert => PlanKind::Convert,
        }
    }
}

impl Status for PlanKind {
    const ALL: &'static [Self] = &[
        PlanKind::Rename,
        PlanKind::Repack,
        PlanKind::Fill,
        PlanKind::Replace,
        PlanKind::SetAside,
        PlanKind::Convert,
    ];

    fn look(self) -> Look {
        match self {
            PlanKind::Rename => look(
                Glyph::PencilLine,
                Tone::Active,
                "Rename",
                "Writes the game under the DAT's names.",
            ),
            PlanKind::Repack => look(
                Glyph::Package,
                Tone::Active,
                "Repack",
                "Stores the game's ROMs together, as your storage policy says.",
            ),
            PlanKind::Fill => look(
                Glyph::CirclePlus,
                Tone::Good,
                "Fill",
                "Adds a missing game from an ingest folder, or a missing cue sheet from a cue pack.",
            ),
            PlanKind::Replace => look(
                Glyph::Replace,
                Tone::Review,
                "Replace",
                "Replaces a mismatched or unreadable entry from an ingest folder or a cue pack, keeping the original.",
            ),
            PlanKind::SetAside => look(
                Glyph::Archive,
                Tone::Review,
                "Set aside",
                "Moves what no game needs to kept files.",
            ),
            PlanKind::Convert => look(
                Glyph::RefreshCw,
                Tone::Active,
                "Convert",
                "Rewrites the game in the format your storage policy now gives.",
            ),
        }
    }
}

/// Where a Source Set's repair is: the icon of its Repair tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RepairState {
    Proposing,
    Running,
    Approved,
    Open,
}

impl Status for RepairState {
    const ALL: &'static [Self] = &[
        RepairState::Proposing,
        RepairState::Running,
        RepairState::Approved,
        RepairState::Open,
    ];

    fn look(self) -> Look {
        match self {
            RepairState::Proposing => look(
                Glyph::Clock,
                Tone::Active,
                "Proposing",
                "A repair plan is being made.",
            ),
            RepairState::Running => look(
                Glyph::LoaderCircle,
                Tone::Active,
                "Running",
                "The approved repair is being carried out.",
            ),
            RepairState::Approved => look(
                Glyph::Check,
                Tone::Good,
                "Approved",
                "You approved the repair; it hasn't finished.",
            ),
            RepairState::Open => look(
                Glyph::ClipboardList,
                Tone::Review,
                "Open",
                "A repair plan waits for your review.",
            ),
        }
    }
}

/// The kind of a build plan item, or of a build's or cleanup's item, shown beside its row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuildKind {
    Copy,
    Repack,
    Place,
    Return,
    Adopt,
    Unchanged,
    NoSource,
    Conflict,
    Unresolved,
    Blocked,
    Remove,
    SetAside,
    Acknowledge,
    Restore,
}

impl From<ItemKind> for BuildKind {
    fn from(kind: ItemKind) -> Self {
        match kind {
            ItemKind::Copy => BuildKind::Copy,
            ItemKind::Place => BuildKind::Place,
            ItemKind::Return => BuildKind::Return,
            ItemKind::Repack => BuildKind::Repack,
            ItemKind::Adopt => BuildKind::Adopt,
            ItemKind::Unchanged => BuildKind::Unchanged,
            ItemKind::NoSource => BuildKind::NoSource,
            ItemKind::Conflict => BuildKind::Conflict,
            ItemKind::Unresolved => BuildKind::Unresolved,
            ItemKind::Blocked => BuildKind::Blocked,
            ItemKind::Remove => BuildKind::Remove,
            ItemKind::KeepAside => BuildKind::SetAside,
            ItemKind::Acknowledge => BuildKind::Acknowledge,
            ItemKind::Restore => BuildKind::Restore,
        }
    }
}

impl Status for BuildKind {
    const ALL: &'static [Self] = &[
        BuildKind::Copy,
        BuildKind::Repack,
        BuildKind::Place,
        BuildKind::Return,
        BuildKind::Adopt,
        BuildKind::Unchanged,
        BuildKind::NoSource,
        BuildKind::Conflict,
        BuildKind::Unresolved,
        BuildKind::Blocked,
        BuildKind::Remove,
        BuildKind::SetAside,
        BuildKind::Acknowledge,
        BuildKind::Restore,
    ];

    fn look(self) -> Look {
        match self {
            BuildKind::Copy => look(
                Glyph::Copy,
                Tone::Active,
                "Copy",
                "Copies an archive that is already the game's TorrentZip.",
            ),
            BuildKind::Repack => look(
                Glyph::Package,
                Tone::Active,
                "Repack",
                "Writes a TorrentZip from the verified ROMs.",
            ),
            BuildKind::Place => look(
                Glyph::ArrowRight,
                Tone::Active,
                "Place",
                "Moves or copies the game here as its only copy, taking it out of the source set.",
            ),
            BuildKind::Return => look(
                Glyph::ArrowLeftRight,
                Tone::Active,
                "Return",
                "Moves or copies a game placed here back to the source set, verified there before it leaves here.",
            ),
            BuildKind::Adopt => look(
                Glyph::FolderCheck,
                Tone::Good,
                "Adopt",
                "Records the file already there as built, because its ROMs are the ones the DAT expects.",
            ),
            BuildKind::Unchanged => look(
                Glyph::Check,
                Tone::Neutral,
                "Unchanged",
                "The folder already holds this file, built from the same ROMs.",
            ),
            BuildKind::NoSource => look(
                Glyph::CircleOff,
                Tone::Review,
                "No source",
                "Skipped: no entry in the audit holds every ROM.",
            ),
            BuildKind::Conflict => look(
                Glyph::TriangleAlert,
                Tone::Review,
                "Conflict",
                "Skipped: a file Stage Select didn't build, or one changed since, is at the target and is never overwritten.",
            ),
            BuildKind::Unresolved => look(
                Glyph::CircleQuestionMark,
                Tone::Review,
                "Unresolved",
                "Skipped: you chose to build the resolved game groups only, so its target is left as it is.",
            ),
            BuildKind::Blocked => look(
                Glyph::CircleX,
                Tone::Failed,
                "Blocked",
                "Skipped: it can't be built as planned, such as for an invalid name.",
            ),
            BuildKind::Remove => look(
                Glyph::Trash2,
                Tone::Review,
                "Remove",
                "Removes a built file whose game is no longer selected.",
            ),
            BuildKind::SetAside => look(
                Glyph::Archive,
                Tone::Review,
                "Set aside",
                "Moves an entry Stage Select didn't build to kept files.",
            ),
            BuildKind::Acknowledge => look(
                Glyph::Eye,
                Tone::Neutral,
                "Acknowledge",
                "Leaves an entry Stage Select didn't build where it is.",
            ),
            BuildKind::Restore => look(
                Glyph::Undo2,
                Tone::Active,
                "Restore",
                "Puts a kept file back in the folder.",
            ),
        }
    }
}

/// The kind of an ingest plan item, shown beside its *current → proposed* row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IngestPlanKind {
    Add,
    Import,
    Discard,
}

impl From<IngestAction> for IngestPlanKind {
    fn from(action: IngestAction) -> Self {
        match action {
            IngestAction::Addition => IngestPlanKind::Add,
            IngestAction::DatImport => IngestPlanKind::Import,
            IngestAction::Discard => IngestPlanKind::Discard,
        }
    }
}

impl Status for IngestPlanKind {
    const ALL: &'static [Self] = &[
        IngestPlanKind::Add,
        IngestPlanKind::Import,
        IngestPlanKind::Discard,
    ];

    fn look(self) -> Look {
        match self {
            IngestPlanKind::Add => look(
                Glyph::CirclePlus,
                Tone::Good,
                "Add",
                "Repacks it into its system's additions set, keeping the original once the copy reads back exactly.",
            ),
            IngestPlanKind::Import => look(
                Glyph::Upload,
                Tone::Active,
                "Import",
                "Imports it as a DAT revision, then keeps it in the folder's kept files.",
            ),
            IngestPlanKind::Discard => look(
                Glyph::Archive,
                Tone::Review,
                "Discard",
                "Sets it aside in the folder's kept files.",
            ),
        }
    }
}

/// Where an Ingest Folder item stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IngestStatus {
    Arriving,
    MatchesSourceSet,
    Duplicate,
    NoDatMatch,
    ImportFile,
    Unreadable,
    Dismissed,
    Moving,
    Done,
}

impl Status for IngestStatus {
    const ALL: &'static [Self] = &[
        IngestStatus::Arriving,
        IngestStatus::MatchesSourceSet,
        IngestStatus::Duplicate,
        IngestStatus::NoDatMatch,
        IngestStatus::ImportFile,
        IngestStatus::Unreadable,
        IngestStatus::Dismissed,
        IngestStatus::Moving,
        IngestStatus::Done,
    ];

    fn look(self) -> Look {
        match self {
            IngestStatus::Arriving => look(
                Glyph::Download,
                Tone::Neutral,
                "Arriving",
                "It's still changing; it's assessed once it settles.",
            ),
            IngestStatus::MatchesSourceSet => look(
                Glyph::FolderCheck,
                Tone::Good,
                "Matches source set",
                "It holds a game a source set needs.",
            ),
            IngestStatus::Duplicate => look(
                Glyph::Copy,
                Tone::Neutral,
                "Duplicate",
                "Every source set it matches already holds it.",
            ),
            IngestStatus::NoDatMatch => look(
                Glyph::FileQuestionMark,
                Tone::Review,
                "No DAT match",
                "It matches no game in any DAT revision.",
            ),
            IngestStatus::ImportFile => look(
                Glyph::FileText,
                Tone::Review,
                "File to import",
                "It's a DAT or MAME support file you may import.",
            ),
            IngestStatus::Unreadable => look(
                Glyph::FileX,
                Tone::Failed,
                "Unreadable",
                "It couldn't be read, or it's an archive Stage Select doesn't open.",
            ),
            IngestStatus::Dismissed => look(
                Glyph::CircleOff,
                Tone::Neutral,
                "Dismissed",
                "You dismissed it; it stays dismissed until it changes.",
            ),
            IngestStatus::Moving => look(
                Glyph::FolderInput,
                Tone::Active,
                "Moving",
                "A plan you approved is moving it.",
            ),
            IngestStatus::Done => look(
                Glyph::Check,
                Tone::Good,
                "Done",
                "A plan you approved moved it.",
            ),
        }
    }
}

/// How a game group stands in a Selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GroupStatus {
    ChoiceToReview,
    Unresolved,
    YourChoice,
    Chosen,
    Excluded,
}

impl Status for GroupStatus {
    const ALL: &'static [Self] = &[
        GroupStatus::ChoiceToReview,
        GroupStatus::Unresolved,
        GroupStatus::YourChoice,
        GroupStatus::Chosen,
        GroupStatus::Excluded,
    ];

    fn look(self) -> Look {
        match self {
            GroupStatus::ChoiceToReview => look(
                Glyph::TriangleAlert,
                Tone::Review,
                "Choice to review",
                "Your choice names a game that is now left out or gone, so the default applies until you change it.",
            ),
            GroupStatus::Unresolved => look(
                Glyph::CircleQuestionMark,
                Tone::Review,
                "Unresolved",
                "The selection picked a game by a tie-break, so it needs your decision.",
            ),
            GroupStatus::YourChoice => look(
                Glyph::PencilLine,
                Tone::Good,
                "Your choice",
                "Your choice decides which games are kept.",
            ),
            GroupStatus::Chosen => look(
                Glyph::CircleCheck,
                Tone::Good,
                "Chosen",
                "The selection chose a game by your settings.",
            ),
            GroupStatus::Excluded => look(
                Glyph::CircleOff,
                Tone::Neutral,
                "Left out",
                "Your settings leave out every game of the game group.",
            ),
        }
    }
}

/// Whether the store still keeps a DAT Revision's file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DatFileStatus {
    Kept,
    Deleted,
}

impl Status for DatFileStatus {
    const ALL: &'static [Self] = &[DatFileStatus::Kept, DatFileStatus::Deleted];

    fn look(self) -> Look {
        match self {
            DatFileStatus::Kept => look(
                Glyph::FileCheck,
                Tone::Good,
                "File kept",
                "Its DAT file is kept in the store.",
            ),
            DatFileStatus::Deleted => look(
                Glyph::FileX,
                Tone::Neutral,
                "File deleted",
                "Its DAT file was deleted from the store; import it again to restore it.",
            ),
        }
    }
}

/// A MAME Support File an arcade Selection read, against its ListXML's MAME version: drift, never a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SupportFileStatus {
    Older,
    Newer,
    NotRead,
    Uncompared,
    Current,
}

impl Status for SupportFileStatus {
    const ALL: &'static [Self] = &[
        SupportFileStatus::Older,
        SupportFileStatus::Newer,
        SupportFileStatus::NotRead,
        SupportFileStatus::Uncompared,
        SupportFileStatus::Current,
    ];

    fn look(self) -> Look {
        match self {
            SupportFileStatus::Older => look(
                Glyph::FileClock,
                Tone::Review,
                "Older version",
                "It is for an older MAME than the DAT revision, so machines newer than it are read as if it were absent.",
            ),
            SupportFileStatus::Newer => look(
                Glyph::FileUp,
                Tone::Neutral,
                "Newer version",
                "It is for a newer MAME than the DAT revision, so it may name machines the DAT revision lacks, which match nothing.",
            ),
            SupportFileStatus::NotRead => look(
                Glyph::FileX,
                Tone::Neutral,
                "Not read",
                "No file of this kind was read, so the selection did without it.",
            ),
            SupportFileStatus::Uncompared => look(
                Glyph::CircleQuestionMark,
                Tone::Neutral,
                "Version not compared",
                "The DAT revision names no MAME version that can be read, so this file can't be compared with it.",
            ),
            SupportFileStatus::Current => look(
                Glyph::FileCheck,
                Tone::Good,
                "Same version",
                "It is for the MAME version of the DAT revision.",
            ),
        }
    }
}

/// Where fetching one system's DATs and packs from a DAT provider stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FetchState {
    Running,
    Failed,
    Off,
    NotFetched,
    Fetched,
}

impl Status for FetchState {
    const ALL: &'static [Self] = &[
        FetchState::Running,
        FetchState::Failed,
        FetchState::Off,
        FetchState::NotFetched,
        FetchState::Fetched,
    ];

    fn look(self) -> Look {
        match self {
            FetchState::Running => look(
                Glyph::LoaderCircle,
                Tone::Active,
                "Fetching",
                "A fetch is running now.",
            ),
            FetchState::Failed => look(
                Glyph::CircleX,
                Tone::Failed,
                "Fetch failed",
                "The last fetch failed; the last good DAT revision stays, and it's tried again a day later.",
            ),
            FetchState::Off => look(
                Glyph::CircleOff,
                Tone::Neutral,
                "Fetching off",
                "Fetching from this provider is off until you turn it on under Settings, then Operations.",
            ),
            FetchState::NotFetched => look(
                Glyph::Clock,
                Tone::Neutral,
                "Not fetched",
                "Nothing has been fetched for it yet.",
            ),
            FetchState::Fetched => look(
                Glyph::CircleCheck,
                Tone::Good,
                "Fetched",
                "The last fetch worked.",
            ),
        }
    }
}

/// How one row of the calibration results stands: the host or a root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CalibrationStatus {
    Unreadable,
    Stale,
    TooSmall,
    TimedOut,
    NotMeasured,
    Measured,
}

impl Status for CalibrationStatus {
    const ALL: &'static [Self] = &[
        CalibrationStatus::Unreadable,
        CalibrationStatus::Stale,
        CalibrationStatus::TooSmall,
        CalibrationStatus::TimedOut,
        CalibrationStatus::NotMeasured,
        CalibrationStatus::Measured,
    ];

    fn look(self) -> Look {
        match self {
            CalibrationStatus::Unreadable => look(
                Glyph::CircleX,
                Tone::Failed,
                "Unreadable",
                "The calibration couldn't read it; the reason is under the results.",
            ),
            CalibrationStatus::Stale => look(
                Glyph::ClockAlert,
                Tone::Review,
                "Stale",
                "It changed since it was measured; its results still apply until you calibrate again.",
            ),
            CalibrationStatus::TooSmall => look(
                Glyph::CircleDashed,
                Tone::Neutral,
                "Too small",
                "It holds too little to read for a measurement worth having.",
            ),
            CalibrationStatus::TimedOut => look(
                Glyph::FileClock,
                Tone::Review,
                "Timed out",
                "The calibration couldn't find enough to read in time; calibrate again when it's quieter.",
            ),
            CalibrationStatus::NotMeasured => look(
                Glyph::Clock,
                Tone::Neutral,
                "Not measured",
                "No calibration has measured it yet.",
            ),
            CalibrationStatus::Measured => look(
                Glyph::CircleCheck,
                Tone::Good,
                "Measured",
                "Its results are current.",
            ),
        }
    }
}

impl Status for JobState {
    const ALL: &'static [Self] = &JobState::ALL;

    fn look(self) -> Look {
        match self {
            JobState::Queued => look(
                Glyph::Clock,
                Tone::Neutral,
                "Queued",
                "It starts when the jobs ahead of it finish.",
            ),
            JobState::Running => look(
                Glyph::LoaderCircle,
                Tone::Active,
                "Running",
                "It's running now.",
            ),
            JobState::Succeeded => {
                look(Glyph::CircleCheck, Tone::Good, "Succeeded", "It finished.")
            }
            JobState::Failed => look(
                Glyph::CircleX,
                Tone::Failed,
                "Failed",
                "It stopped with a problem. Its detail says why.",
            ),
            JobState::Interrupted => look(
                Glyph::CirclePause,
                Tone::Review,
                "Interrupted",
                "It stopped before finishing, because you cancelled it or the service restarted.",
            ),
        }
    }
}

/// How RomM's view of a file a build made disagrees with the build. Stage Select only shows it: RomM's matches are
/// changed in RomM.
impl Status for RommFlag {
    const ALL: &'static [Self] = &RommFlag::ALL;

    fn look(self) -> Look {
        match self {
            RommFlag::Unidentified => look(
                Glyph::FileQuestionMark,
                Tone::Review,
                "Unidentified",
                "RomM matched it to no game, so you can match it in RomM or ask RomM to scan it again.",
            ),
            RommFlag::TitleMismatch => look(
                Glyph::EqualNot,
                Tone::Review,
                "Title differs",
                "RomM matched it to a game whose name isn't the DAT's, so you can check the match in RomM.",
            ),
            RommFlag::HashMismatch => look(
                Glyph::FileExclamationPoint,
                Tone::Review,
                "Hash differs",
                "RomM's SHA-1 for it isn't what the build wrote, or RomM doesn't list it, so you can ask RomM to scan it again.",
            ),
        }
    }
}

/// How the RomM scan a build asked for once it ended went. Anything but scanning or scanned is a warning: the build
/// still succeeded.
impl Status for RommScanOutcome {
    const ALL: &'static [Self] = &RommScanOutcome::ALL;

    fn look(self) -> Look {
        match self {
            RommScanOutcome::Requested => look(
                Glyph::LoaderCircle,
                Tone::Active,
                "Scanning",
                "RomM is scanning this folder's platform now.",
            ),
            RommScanOutcome::Finished => look(
                Glyph::CircleCheck,
                Tone::Good,
                "Scanned",
                "RomM scanned this folder's platform, so it shows what the build changed.",
            ),
            RommScanOutcome::Stopped => look(
                Glyph::CircleQuestionMark,
                Tone::Review,
                "Scan not followed",
                "Stage Select stopped following the scan before it ended, so check the platform in RomM.",
            ),
            RommScanOutcome::Failed => look(
                Glyph::CircleX,
                Tone::Review,
                "Scan failed",
                "RomM's scan ended without finishing, so scan the platform in RomM.",
            ),
            RommScanOutcome::TimedOut => look(
                Glyph::ClockAlert,
                Tone::Review,
                "Scan timed out",
                "RomM didn't finish the scan in time, so check the platform in RomM.",
            ),
            RommScanOutcome::Unavailable => look(
                Glyph::CircleOff,
                Tone::Review,
                "RomM unavailable",
                "RomM had no scan worker running, so scan the platform in RomM once it does.",
            ),
            RommScanOutcome::Error => look(
                Glyph::TriangleAlert,
                Tone::Review,
                "Scan error",
                "Asking RomM to scan went wrong; the detail says why.",
            ),
            RommScanOutcome::Unmapped => look(
                Glyph::CircleDashed,
                Tone::Review,
                "No platform",
                "This folder isn't one RomM platform, so RomM wasn't asked to scan it.",
            ),
        }
    }
}

/// How keeping RomM's "Additions" collection in step went once the scan finished. Anything but updated is a warning:
/// the build still succeeded.
impl Status for RommAdditionsOutcome {
    const ALL: &'static [Self] = &RommAdditionsOutcome::ALL;

    fn look(self) -> Look {
        match self {
            RommAdditionsOutcome::Synced => look(
                Glyph::CircleCheck,
                Tone::Good,
                "Collection updated",
                "RomM's Additions collection holds every addition RomM has indexed.",
            ),
            RommAdditionsOutcome::Stopped => look(
                Glyph::CircleQuestionMark,
                Tone::Review,
                "Update stopped",
                "Stage Select stopped before the collection was in step; the next build RomM scans tries again.",
            ),
            RommAdditionsOutcome::Error => look(
                Glyph::TriangleAlert,
                Tone::Review,
                "Collection error",
                "Updating the collection went wrong; the detail says why.",
            ),
        }
    }
}

/// A Game's Selection Outcome: kept, a clone of a Game kept, left out, or needed by a Game kept.
impl Status for Outcome {
    const ALL: &'static [Self] = &Outcome::ALL;

    fn look(self) -> Look {
        match self {
            Outcome::Kept => look(
                Glyph::CircleCheck,
                Tone::Good,
                "Kept",
                "The version of a game the selection keeps.",
            ),
            Outcome::Clone => look(
                Glyph::Copy,
                Tone::Neutral,
                "Clone",
                "Another version of a game you kept.",
            ),
            Outcome::LeftOut => look(
                Glyph::CircleOff,
                Tone::Neutral,
                "Left out",
                "A rule of your selection profile leaves it out.",
            ),
            Outcome::Dependency => look(
                Glyph::Package,
                Tone::Neutral,
                "Dependency",
                "A game you kept needs it to run.",
            ),
        }
    }
}

/// What you have of a Game a Selection lists, from the latest Audit: its Game Status, saying when it rests on Claim
/// Evidence for some ROM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AvailabilityStatus {
    Missing,
    Incomplete,
    IncompleteOnClaims,
    CompleteOnClaims,
    Complete,
    NothingToCheck,
}

impl AvailabilityStatus {
    /// The availability of a Game with `status`, `claimed` when some ROM of it is present on Claim Evidence alone.
    pub fn of(status: GameStatus, claimed: bool) -> Self {
        match (status, claimed) {
            (GameStatus::Complete, false) => AvailabilityStatus::Complete,
            (GameStatus::Complete, true) => AvailabilityStatus::CompleteOnClaims,
            (GameStatus::Incomplete, false) => AvailabilityStatus::Incomplete,
            (GameStatus::Incomplete, true) => AvailabilityStatus::IncompleteOnClaims,
            (GameStatus::Missing, _) => AvailabilityStatus::Missing,
            (GameStatus::NothingToCheck, _) => AvailabilityStatus::NothingToCheck,
        }
    }
}

impl Status for AvailabilityStatus {
    const ALL: &'static [Self] = &[
        AvailabilityStatus::Missing,
        AvailabilityStatus::Incomplete,
        AvailabilityStatus::IncompleteOnClaims,
        AvailabilityStatus::CompleteOnClaims,
        AvailabilityStatus::Complete,
        AvailabilityStatus::NothingToCheck,
    ];

    fn look(self) -> Look {
        match self {
            AvailabilityStatus::Missing => look(
                Glyph::CircleOff,
                Tone::Neutral,
                "Missing",
                "You have none of its ROMs.",
            ),
            AvailabilityStatus::Incomplete => look(
                Glyph::CircleDashed,
                Tone::Review,
                "Incomplete",
                "You have some of its ROMs.",
            ),
            AvailabilityStatus::IncompleteOnClaims => look(
                Glyph::CircleDotDashed,
                Tone::Review,
                "Incomplete on claims",
                "You have some of its ROMs, and some of those only by their archive's claim, which a build never takes.",
            ),
            AvailabilityStatus::CompleteOnClaims => look(
                Glyph::ShieldQuestionMark,
                Tone::Good,
                "Complete on claims",
                "You have every ROM it needs, but some only by their archive's claim, which a build never takes.",
            ),
            AvailabilityStatus::Complete => look(
                Glyph::CircleCheck,
                Tone::Good,
                "Complete",
                "You have every ROM it needs.",
            ),
            AvailabilityStatus::NothingToCheck => look(
                Glyph::CircleMinus,
                Tone::Neutral,
                "Nothing to check",
                "It lists no ROM you need that a file can match, so it lacks nothing.",
            ),
        }
    }
}

/// The states the glossary derives from an Entry's Finding and Evidence.
impl Status for Derived {
    const ALL: &'static [Self] = &[Derived::NeedsReview, Derived::Verified];

    fn look(self) -> Look {
        match self {
            Derived::NeedsReview => look(
                Glyph::TriangleAlert,
                Tone::Review,
                "Needs review",
                "Its finding isn't matched, so look at it before you repair or build.",
            ),
            Derived::Verified => look(
                Glyph::ShieldCheck,
                Tone::Good,
                "Verified",
                "Matched, and every byte of its payload was read.",
            ),
        }
    }
}

/// How your choice for a game group relates to what the selection would pick without it.
impl Status for ChoiceStatus {
    const ALL: &'static [Self] = &[
        ChoiceStatus::Conflicting,
        ChoiceStatus::Stale,
        ChoiceStatus::Applied,
        ChoiceStatus::MatchesDefault,
    ];

    fn look(self) -> Look {
        match self {
            ChoiceStatus::Conflicting => look(
                Glyph::ArrowLeftRight,
                Tone::Review,
                "Conflicting choice",
                "A game you checked is now left out by your settings, so your choice isn't applied.",
            ),
            ChoiceStatus::Stale => look(
                Glyph::Clock,
                Tone::Review,
                "Stale choice",
                "A game you checked is gone from the game group, so your choice isn't applied.",
            ),
            ChoiceStatus::Applied => look(
                Glyph::PencilLine,
                Tone::Good,
                "Your choice",
                "Your choice decides which games are kept.",
            ),
            ChoiceStatus::MatchesDefault => look(
                Glyph::CircleCheck,
                Tone::Neutral,
                "Matches the default",
                "The selection now keeps exactly the games you checked, so you can drop your choice.",
            ),
        }
    }
}

/// A Selection's flags on a game group, shown on each of its Games.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SelectionFlag {
    Unresolved,
    StaleChoice,
    ConflictingChoice,
    CoverageDrift,
}

impl Status for SelectionFlag {
    const ALL: &'static [Self] = &[
        SelectionFlag::Unresolved,
        SelectionFlag::StaleChoice,
        SelectionFlag::ConflictingChoice,
        SelectionFlag::CoverageDrift,
    ];

    fn look(self) -> Look {
        match self {
            SelectionFlag::Unresolved => look(
                Glyph::CircleQuestionMark,
                Tone::Review,
                "Unresolved",
                "The selection picked a game by a tie-break, so it needs your decision.",
            ),
            SelectionFlag::StaleChoice => look(
                Glyph::Clock,
                Tone::Review,
                "Stale choice",
                "A game you checked is gone from the game group, so your choice isn't applied.",
            ),
            SelectionFlag::ConflictingChoice => look(
                Glyph::ArrowLeftRight,
                Tone::Review,
                "Conflicting choice",
                "A game you checked is now left out by your settings, so your choice isn't applied.",
            ),
            SelectionFlag::CoverageDrift => look(
                Glyph::Globe,
                Tone::Neutral,
                "Coverage drift",
                "A game in this game group has no metadata entry, so the selection may know less about it.",
            ),
        }
    }
}

/// Where an Addition is in its life.
impl Status for AdditionState {
    const ALL: &'static [Self] = &[
        AdditionState::Failed,
        AdditionState::Pending,
        AdditionState::Retiring,
        AdditionState::Restoring,
        AdditionState::Active,
        AdditionState::Retired,
    ];

    fn look(self) -> Look {
        match self {
            AdditionState::Failed => look(
                Glyph::CircleX,
                Tone::Failed,
                "Failed",
                "Its file couldn't be written into the additions set, and the original was left as it was.",
            ),
            AdditionState::Pending => look(
                Glyph::LoaderCircle,
                Tone::Active,
                "Being published",
                "Its file is being written into the additions set.",
            ),
            AdditionState::Retiring => look(
                Glyph::Archive,
                Tone::Active,
                "Being retired",
                "Its file is being moved into the additions set's kept files.",
            ),
            AdditionState::Restoring => look(
                Glyph::Undo2,
                Tone::Active,
                "Being restored",
                "Its file is being moved back from the additions set's kept files.",
            ),
            AdditionState::Active => look(
                Glyph::CircleCheck,
                Tone::Good,
                "Active",
                "Its file is in the additions set, and builds that opt in read it.",
            ),
            AdditionState::Retired => look(
                Glyph::CircleOff,
                Tone::Neutral,
                "Retired",
                "Builds no longer read it; its file is among the additions set's kept files.",
            ),
        }
    }
}

/// A Library Folder's latest Check, against the folder's current Build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CheckFlag {
    BeforeBuild,
}

impl Status for CheckFlag {
    const ALL: &'static [Self] = &[CheckFlag::BeforeBuild];

    fn look(self) -> Look {
        match self {
            CheckFlag::BeforeBuild => look(
                Glyph::FileClock,
                Tone::Review,
                "Before current build",
                "This check ran before the folder's current build, so check the folder again to see it as it is.",
            ),
        }
    }
}

/// What a Check found for one built file.
impl Status for FileOutcome {
    const ALL: &'static [Self] = &[
        FileOutcome::Conflict,
        FileOutcome::Missing,
        FileOutcome::Unreadable,
        FileOutcome::Repacked,
        FileOutcome::Refreshed,
        FileOutcome::Unchanged,
    ];

    fn look(self) -> Look {
        match self {
            FileOutcome::Conflict => look(
                Glyph::ArrowLeftRight,
                Tone::Review,
                "Conflict",
                "It changed, and no longer holds the ROMs the DAT expects there.",
            ),
            FileOutcome::Missing => look(Glyph::CircleOff, Tone::Review, "Missing", "It's gone."),
            FileOutcome::Unreadable => look(
                Glyph::FileExclamationPoint,
                Tone::Review,
                "Unreadable",
                "It couldn't be read, or changed while it was read.",
            ),
            FileOutcome::Repacked => look(
                Glyph::Package,
                Tone::Neutral,
                "Repacked",
                "It holds the ROMs the DAT expects there in another ZIP, such as one zipped again by another tool.",
            ),
            FileOutcome::Refreshed => look(
                Glyph::RefreshCw,
                Tone::Good,
                "Record refreshed",
                "Its size or time changed, but it's still the file the build wrote, so its record was refreshed.",
            ),
            FileOutcome::Unchanged => look(
                Glyph::CircleCheck,
                Tone::Good,
                "Unchanged",
                "Its size and modification time are as the build recorded them.",
            ),
        }
    }
}

/// Where a Build's item stands.
impl Status for ItemState {
    const ALL: &'static [Self] = &[
        ItemState::Failed,
        ItemState::Uncertain,
        ItemState::Publishing,
        ItemState::Pending,
        ItemState::Applied,
    ];

    fn look(self) -> Look {
        match self {
            ItemState::Failed => look(
                Glyph::CircleX,
                Tone::Failed,
                "Failed",
                "It couldn't be written, or failed its check, so nothing was put in place.",
            ),
            ItemState::Uncertain => look(
                Glyph::CircleQuestionMark,
                Tone::Review,
                "Uncertain",
                "The build stopped while putting it in place, and its target now holds another file.",
            ),
            ItemState::Publishing => look(
                Glyph::LoaderCircle,
                Tone::Active,
                "Writing",
                "It's written and checked, and being put in place.",
            ),
            ItemState::Pending => look(
                Glyph::Clock,
                Tone::Neutral,
                "Not written",
                "The build hasn't written it, or stopped before it did.",
            ),
            ItemState::Applied => look(Glyph::Check, Tone::Good, "Written", "It's in place."),
        }
    }
}

/// The workbench's live connection to the Service.
impl Status for Stream {
    const ALL: &'static [Self] = &[Stream::Reconnecting, Stream::Connecting, Stream::Live];

    fn look(self) -> Look {
        match self {
            Stream::Reconnecting => look(
                Glyph::RefreshCw,
                Tone::Review,
                "Reconnecting",
                "The connection to the service dropped, so what you see may be out of date until it's back.",
            ),
            Stream::Connecting => look(
                Glyph::LoaderCircle,
                Tone::Neutral,
                "Connecting",
                "The workbench is connecting to the service for live updates.",
            ),
            Stream::Live => look(
                Glyph::Activity,
                Tone::Good,
                "Live",
                "Jobs update here as they change.",
            ),
        }
    }
}

/// A status as its icon alone, for a table cell: its label is the icon's tooltip and accessible name. In a
/// `parts::list` row, set `tab_stop` false: the row's detail pane names it too.
#[component]
pub fn StatusIcon<S: Status>(status: S, #[prop(default = true)] tab_stop: bool) -> impl IntoView {
    let look = status.look();
    view! {
        <span class=format!("status {}", look.tone.class())>
            <Icon glyph=look.glyph label=look.label tab_stop />
        </span>
    }
}

/// Where a first-run step stands, with Skipped for one you passed over on the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StepStatus {
    Done,
    Next,
    Waiting,
    Skipped,
}

impl StepStatus {
    /// The status of a step in `state`; a step you've taken reads Done even if you skipped it.
    pub fn of(state: crate::welcome_model::State, skipped: bool) -> Self {
        use crate::welcome_model::State;
        match (state, skipped) {
            (State::Done, _) => StepStatus::Done,
            (_, true) => StepStatus::Skipped,
            (State::Ready, _) => StepStatus::Next,
            (State::Waiting(_), _) => StepStatus::Waiting,
        }
    }
}

impl Status for StepStatus {
    const ALL: &'static [Self] = &[
        StepStatus::Next,
        StepStatus::Waiting,
        StepStatus::Skipped,
        StepStatus::Done,
    ];

    fn look(self) -> Look {
        match self {
            StepStatus::Done => look(
                Glyph::CircleCheck,
                Tone::Good,
                "Done",
                "You've taken this step.",
            ),
            StepStatus::Next => look(
                Glyph::ArrowRight,
                Tone::Active,
                "Next",
                "You can take this step now.",
            ),
            StepStatus::Waiting => look(
                Glyph::CircleDashed,
                Tone::Neutral,
                "Waiting",
                "You need to take an earlier step first.",
            ),
            StepStatus::Skipped => look(
                Glyph::CircleMinus,
                Tone::Neutral,
                "Skipped",
                "You skipped this step, and can take it later.",
            ),
        }
    }
}

/// A status as its icon and label, for a detail pane or a summary: its hint is the icon's tooltip.
#[component]
pub fn StatusBadge<S: Status>(status: S) -> impl IntoView {
    let look = status.look();
    view! {
        <span class=format!("status {}", look.tone.class())>
            <Icon glyph=look.glyph label=look.hint />
            <span class="status-label">{look.label}</span>
        </span>
    }
}

/// A count tile in a `ul.counts`: how many have `status`, in its tone, its glyph and label below the number. `what`
/// names what is counted when the status's label alone doesn't ("Entries that need review").
#[component]
pub fn StatusCount<S: Status>(
    status: S,
    #[prop(optional, into)] what: Option<String>,
    children: Children,
) -> impl IntoView {
    let look = status.look();
    view! {
        <li class=format!("count status-count {}", look.tone.class())>
            <span class="n">{children()}</span>
            <span class="what">
                <span class=format!("status {}", look.tone.class())>
                    <Icon glyph=look.glyph label=look.hint />
                </span>
                " "
                {what.unwrap_or_else(|| look.label.to_string())}
            </span>
        </li>
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::HashSet;

    use super::*;

    fn vocabulary<S: Status>() -> Vec<Look> {
        S::ALL.iter().map(|s| s.look()).collect()
    }

    pub(crate) fn every_vocabulary() -> Vec<(&'static str, Vec<Look>)> {
        vec![
            ("findings", vocabulary::<FindingStatus>()),
            ("game status", vocabulary::<GameStatus>()),
            ("placements", vocabulary::<Placement>()),
            ("game flags", vocabulary::<GameFlag>()),
            ("source set rows", vocabulary::<SourceSetStatus>()),
            ("stale causes", vocabulary::<StaleCause>()),
            ("library folder rows", vocabulary::<LibraryStatus>()),
            ("plan kinds", vocabulary::<PlanKind>()),
            ("repair states", vocabulary::<RepairState>()),
            ("build plan kinds", vocabulary::<BuildKind>()),
            ("ingest items", vocabulary::<IngestStatus>()),
            ("ingest plan kinds", vocabulary::<IngestPlanKind>()),
            ("jobs", vocabulary::<JobState>()),
            ("RomM scans", vocabulary::<RommScanOutcome>()),
            ("RomM additions", vocabulary::<RommAdditionsOutcome>()),
            ("RomM comparisons", vocabulary::<RommFlag>()),
            ("DAT files", vocabulary::<DatFileStatus>()),
            ("fetching", vocabulary::<FetchState>()),
            ("support files", vocabulary::<SupportFileStatus>()),
            ("selection groups", vocabulary::<GroupStatus>()),
            ("calibration", vocabulary::<CalibrationStatus>()),
            ("first-run steps", vocabulary::<StepStatus>()),
            ("selection outcomes", vocabulary::<Outcome>()),
            ("availability", vocabulary::<AvailabilityStatus>()),
            ("entry states", vocabulary::<Derived>()),
            ("choices", vocabulary::<ChoiceStatus>()),
            ("selection flags", vocabulary::<SelectionFlag>()),
            ("additions", vocabulary::<AdditionState>()),
            ("folder checks", vocabulary::<CheckFlag>()),
            ("checked files", vocabulary::<FileOutcome>()),
            ("build items", vocabulary::<ItemState>()),
            ("the stream", vocabulary::<Stream>()),
        ]
    }

    #[test]
    fn labels_are_three_words_or_fewer_in_sentence_case() {
        for (name, looks) in every_vocabulary() {
            for look in looks {
                let words: Vec<&str> = look.label.split(' ').collect();
                assert!(
                    (1..=3).contains(&words.len()),
                    "{name}: {:?} is too long",
                    look.label
                );
                assert!(
                    words[0].starts_with(|c: char| c.is_ascii_uppercase()),
                    "{name}: {:?} starts in lowercase",
                    look.label
                );
                // Glossary terms are written in lowercase; only acronyms such as DAT keep capitals.
                for word in &words[1..] {
                    let acronym = word.chars().all(|c| c.is_ascii_uppercase());
                    assert!(
                        acronym || word.chars().all(|c| !c.is_ascii_uppercase()),
                        "{name}: {:?} capitalises {word}",
                        look.label
                    );
                }
            }
        }
    }

    #[test]
    fn statuses_in_one_vocabulary_differ_in_shape() {
        for (name, looks) in every_vocabulary() {
            let glyphs: HashSet<_> = looks.iter().map(|l| l.glyph).collect();
            let labels: HashSet<_> = looks.iter().map(|l| l.label).collect();
            assert_eq!(glyphs.len(), looks.len(), "{name} reuses a glyph");
            assert_eq!(labels.len(), looks.len(), "{name} reuses a label");
        }
    }

    #[test]
    fn hints_are_one_sentence_addressed_to_you() {
        for (name, looks) in every_vocabulary() {
            for look in looks {
                assert!(look.hint.ends_with('.'), "{name}: {:?}", look.hint);
                assert!(
                    !look.hint.contains("Operator") && !look.hint.contains("ADR"),
                    "{name}: {:?}",
                    look.hint
                );
            }
        }
    }

    #[test]
    fn every_build_item_kind_has_its_own_status() {
        let kinds: HashSet<BuildKind> = ItemKind::ALL.into_iter().map(BuildKind::from).collect();
        assert_eq!(kinds.len(), ItemKind::ALL.len());
        assert_eq!(kinds, BuildKind::ALL.iter().copied().collect());
        assert_eq!(BuildKind::from(ItemKind::NoSource), BuildKind::NoSource);
        assert_eq!(BuildKind::from(ItemKind::KeepAside), BuildKind::SetAside);
    }

    #[test]
    fn only_payload_evidence_makes_a_matched_entry_verified() {
        assert_eq!(
            FindingStatus::of(Finding::Matched, true),
            FindingStatus::Verified
        );
        assert_eq!(
            FindingStatus::of(Finding::Matched, false),
            FindingStatus::Matched
        );
        assert_eq!(
            FindingStatus::of(Finding::Unreadable, true),
            FindingStatus::Unreadable
        );
        for finding in Finding::ALL {
            assert_ne!(FindingStatus::from(finding), FindingStatus::Verified);
        }
    }

    #[test]
    fn a_source_set_shows_its_status_of_highest_precedence() {
        let audited = SourceSetFacts {
            audited: true,
            ..Default::default()
        };
        assert_eq!(
            SourceSetStatus::of(SourceSetFacts::default()),
            SourceSetStatus::NeverAudited
        );
        assert_eq!(SourceSetStatus::of(audited), SourceSetStatus::Verified);
        let claims = SourceSetFacts {
            claims: true,
            ..audited
        };
        assert_eq!(SourceSetStatus::of(claims), SourceSetStatus::OnClaims);
        let stale = SourceSetFacts {
            stale: true,
            ..claims
        };
        assert_eq!(SourceSetStatus::of(stale), SourceSetStatus::Stale);
        let review = SourceSetFacts {
            needs_review: true,
            ..stale
        };
        assert_eq!(SourceSetStatus::of(review), SourceSetStatus::NeedsReview);
        let repair = SourceSetFacts {
            repair_open: true,
            ..review
        };
        assert_eq!(SourceSetStatus::of(repair), SourceSetStatus::RepairOpen);
        let running = SourceSetFacts {
            running: true,
            ..repair
        };
        assert_eq!(SourceSetStatus::of(running), SourceSetStatus::Running);
        // Its first audit running.
        let first = SourceSetFacts {
            running: true,
            ..Default::default()
        };
        assert_eq!(SourceSetStatus::of(first), SourceSetStatus::Running);
    }

    #[test]
    fn a_library_folder_shows_its_status_of_highest_precedence() {
        let built = LibraryFacts {
            built: true,
            ..Default::default()
        };
        assert_eq!(
            LibraryStatus::of(LibraryFacts::default()),
            LibraryStatus::NeverBuilt
        );
        assert_eq!(LibraryStatus::of(built), LibraryStatus::Current);
        let stale = LibraryFacts {
            stale: true,
            ..built
        };
        assert_eq!(LibraryStatus::of(stale), LibraryStatus::Stale);
        let incomplete = LibraryFacts {
            incomplete: true,
            ..stale
        };
        assert_eq!(LibraryStatus::of(incomplete), LibraryStatus::Incomplete);
        let drift = LibraryFacts {
            drift: true,
            ..incomplete
        };
        assert_eq!(LibraryStatus::of(drift), LibraryStatus::Drift);
        let plan = LibraryFacts {
            plan_open: true,
            ..drift
        };
        assert_eq!(LibraryStatus::of(plan), LibraryStatus::PlanOpen);
        let first = LibraryFacts {
            plan_open: true,
            ..Default::default()
        };
        assert_eq!(LibraryStatus::of(first), LibraryStatus::PlanOpen);
        let running = LibraryFacts {
            running: true,
            ..plan
        };
        assert_eq!(LibraryStatus::of(running), LibraryStatus::Running);
    }

    #[test]
    fn highest_picks_by_order_of_all() {
        assert_eq!(
            highest([JobState::Succeeded, JobState::Running, JobState::Failed]),
            Some(JobState::Running)
        );
        assert_eq!(highest::<JobState>([]), None);
    }

    #[test]
    fn every_repair_kind_has_a_plan_kind() {
        assert_eq!(PlanKind::from(RepairKind::Conflict), PlanKind::Rename);
        assert_eq!(PlanKind::from(RepairKind::KeepAside), PlanKind::SetAside);
        assert_eq!(PlanKind::from(RepairKind::Convert), PlanKind::Convert);
    }

    #[test]
    fn availability_takes_the_tone_of_the_game_status_it_derives_from() {
        for status in GameStatus::ALL {
            for claimed in [false, true] {
                assert_eq!(
                    AvailabilityStatus::of(status, claimed).look().tone,
                    status.look().tone,
                    "{status:?}, claimed {claimed}"
                );
            }
        }
    }

    #[test]
    fn availability_names_a_status_resting_on_claims() {
        assert_eq!(
            AvailabilityStatus::of(GameStatus::Complete, false),
            AvailabilityStatus::Complete
        );
        assert_eq!(
            AvailabilityStatus::of(GameStatus::Complete, true),
            AvailabilityStatus::CompleteOnClaims
        );
        assert_eq!(
            AvailabilityStatus::of(GameStatus::Incomplete, true),
            AvailabilityStatus::IncompleteOnClaims
        );
        // Claims make a game no less missing, and leave nothing to check as it is.
        assert_eq!(
            AvailabilityStatus::of(GameStatus::Missing, true),
            AvailabilityStatus::Missing
        );
        assert_eq!(
            AvailabilityStatus::of(GameStatus::NothingToCheck, true),
            AvailabilityStatus::NothingToCheck
        );
    }
}
