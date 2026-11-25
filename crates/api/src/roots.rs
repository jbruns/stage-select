//! Roots: the folders Stage Select may read, which the Operator registers, edits and unregisters in the store.

use serde::{Deserialize, Serialize};

/// What a Root is for.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum RootKind {
    /// Source Sets. Read-only: Stage Select changes only the Source Sets the Operator lets it, each through its own
    /// switch (`SourceSetRecord::repairs_may_change`), which lists it among the Root's `writable_paths`.
    #[default]
    Sources,
    /// Play Library Folders. Writable.
    Library,
    /// An Ingest Folder: where new files arrive, assessed, and moved out only by plans you approve. Writable.
    Ingest,
    /// Where the Additions Sets live, one folder per system. Writable, and there is at most one.
    Additions,
}

impl RootKind {
    pub const ALL: [RootKind; 4] = [
        RootKind::Sources,
        RootKind::Library,
        RootKind::Ingest,
        RootKind::Additions,
    ];

    /// Its name in the API and the CLI.
    pub fn as_str(self) -> &'static str {
        match self {
            RootKind::Sources => "sources",
            RootKind::Library => "library",
            RootKind::Ingest => "ingest",
            RootKind::Additions => "additions",
        }
    }

    /// Whether a Root of this kind is writable as a whole: every kind but `sources`.
    pub fn writable(self) -> bool {
        self != RootKind::Sources
    }
}

impl std::str::FromStr for RootKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        RootKind::ALL
            .into_iter()
            .find(|k| k.as_str() == s)
            .ok_or_else(|| {
                format!("{s:?} is not a kind of root: give sources, library, ingest or additions")
            })
    }
}

/// A registered Root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Root {
    /// Fixed once registered: every record of a folder in it refers to it by name.
    pub name: String,
    pub path: String,
    pub kind: RootKind,
    /// Whether Stage Select may write to it as a whole, which its kind fixes: read-only for `sources`, writable for
    /// every other kind. Never part of a [`RootSpec`].
    pub writable: bool,
    /// For a `sources` Root: the paths of the Source Sets in it that Stage Select may change, by Repairs and by
    /// Single Copy placing their Games. Each Source Set switches its own (`SourceSetRecord::repairs_may_change`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writable_paths: Vec<String>,
    /// Your Ceilings for it: payload readers across every Job, and the MiB per second every Job may read from it and
    /// write to it. None leaves each to the Calibration or the CPU count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readers: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_mib_per_s: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_mib_per_s: Option<u64>,
    /// Who last registered or edited it, and when.
    pub updated_by: String,
    pub updated_at: String,
}

/// A Root as `POST /roots` registers it and `PUT /roots/{name}` replaces it. An edit can't change its name. Its kind
/// fixes whether it is writable, and only each Source Set's own switch changes its `writable_paths`, so neither is
/// given here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootSpec {
    pub name: String,
    /// An absolute path to a folder that exists.
    pub path: String,
    #[serde(default)]
    pub kind: RootKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readers: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_mib_per_s: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_mib_per_s: Option<u64>,
    /// When registering, create the folder and its missing parents first, as the Service's user, if it is missing.
    /// An edit ignores it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub create: bool,
}

impl RootSpec {
    /// A `sources` Root named `name`, at `path`, with no Ceilings of its own.
    pub fn new(name: &str, path: impl AsRef<std::path::Path>) -> RootSpec {
        RootSpec {
            name: name.into(),
            path: path.as_ref().display().to_string(),
            ..RootSpec::default()
        }
    }

    /// This Root, of kind `library`.
    pub fn library(self) -> RootSpec {
        self.of_kind(RootKind::Library)
    }

    /// This Root, of kind `kind`.
    pub fn of_kind(self, kind: RootKind) -> RootSpec {
        RootSpec { kind, ..self }
    }

    /// This Root, its folder created when it is registered if it is missing.
    pub fn creating(self) -> RootSpec {
        RootSpec {
            create: true,
            ..self
        }
    }
}

impl From<&Root> for RootSpec {
    fn from(root: &Root) -> Self {
        RootSpec {
            name: root.name.clone(),
            path: root.path.clone(),
            kind: root.kind,
            readers: root.readers,
            read_mib_per_s: root.read_mib_per_s,
            write_mib_per_s: root.write_mib_per_s,
            create: false,
        }
    }
}

/// The kind of approved plan an edit voided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoidedPlanKind {
    Build,
    Repair,
}

/// An approved plan, not yet started, that an edit voided because it wrote where the Root may no longer write. Its
/// Job ended failed, having changed nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoidedPlan {
    pub kind: VoidedPlanKind,
    pub plan: i64,
    pub job: i64,
    /// What it would have written to: `play library folder <root>/<path>` or `source set <name>`.
    pub writes_to: String,
}

/// What `PUT /roots/{name}` answers: the Root as it is now, and each approved plan the edit voided.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootEdited {
    pub root: Root,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub voided: Vec<VoidedPlan>,
}

/// One change to a Root, as `GET /roots/{name}/changes` lists it, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootChange {
    /// `registered`, `edited` or `unregistered`.
    pub action: String,
    /// The Root as the change left it; none once unregistered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<Root>,
    pub changed_by: String,
    pub changed_at: String,
    /// The folder this change created, absolute: the Root's own folder, when registering it created it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_folder: Option<String>,
}

/// The folders `GET /folder-suggestions` suggests registering as Roots: each common mount point the Service can see,
/// and the folders directly in it, but for those already registered.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderSuggestions {
    pub folders: Vec<FolderSuggestion>,
    /// Some mount point held more than the listing reads, so some of its folders aren't suggested.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub more: bool,
}

/// A folder to register as a Root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderSuggestion {
    pub path: String,
    /// Whether the Service can write to it now, as it would check when a Root of a writable kind is registered.
    pub writable: bool,
}

/// The folders in one folder, as `GET /folders?path=` lists them to browse for a path: any folder the Service can
/// see, from `/` down. Only names are read, never a file's contents.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderListing {
    /// The folder listed, absolute.
    pub path: String,
    /// The folder it is in; none for `/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// The folders in it, by name, case-insensitively.
    pub folders: Vec<Folder>,
    /// It holds more folders than the listing shows, so some are left out.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub more: bool,
}

/// One folder in a [`FolderListing`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folder {
    pub name: String,
    /// Its absolute path, through the listed folder: a link's own path, not where it points.
    pub path: String,
    /// Whether the Service can write to it now.
    pub writable: bool,
    /// The registered Root it is in, if any: the deepest, when Roots nest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// It is a symbolic link to a folder.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub link: bool,
}

/// The query of `GET /folders`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoldersQuery {
    /// The absolute path of the folder to list.
    pub path: String,
    /// List hidden folders too: those whose names start with a dot, and `lost+found`.
    #[serde(default)]
    pub hidden: bool,
}
