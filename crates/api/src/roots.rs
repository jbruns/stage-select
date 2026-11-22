//! Roots: the folders Title Select may read, which the Operator registers, edits and unregisters in the store.

use serde::{Deserialize, Serialize};

/// What a Root is for.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum RootKind {
    /// Source Sets, if read-only, or Library Folders, if writable.
    #[default]
    General,
    /// An Ingest Folder: where new files arrive, assessed, and moved out only by plans you approve. Writable.
    Ingest,
    /// Where the Additions Sets live, one folder per system. Writable, and there is at most one.
    Additions,
}

impl RootKind {
    pub const ALL: [RootKind; 3] = [RootKind::General, RootKind::Ingest, RootKind::Additions];

    /// Its name in the API and the CLI.
    pub fn as_str(self) -> &'static str {
        match self {
            RootKind::General => "general",
            RootKind::Ingest => "ingest",
            RootKind::Additions => "additions",
        }
    }
}

impl std::str::FromStr for RootKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        RootKind::ALL
            .into_iter()
            .find(|k| k.as_str() == s)
            .ok_or_else(|| {
                format!("{s:?} is not a kind of root: give general, ingest or additions")
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
    /// Whether Title Select may write to it: Builds to its Library Folders, repairs to any Source Set in it.
    pub writable: bool,
    /// For a read-only Root: the paths of the Source Sets in it that a repair may write to.
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

/// A Root as `POST /roots` registers it and `PUT /roots/{name}` replaces it. An edit can't change its name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootSpec {
    pub name: String,
    /// An absolute path to a folder that exists.
    pub path: String,
    #[serde(default)]
    pub kind: RootKind,
    #[serde(default)]
    pub writable: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writable_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readers: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_mib_per_s: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_mib_per_s: Option<u64>,
}

impl RootSpec {
    /// A read-only Root of kind general named `name`, at `path`, with no Ceilings of its own.
    pub fn new(name: &str, path: impl AsRef<std::path::Path>) -> RootSpec {
        RootSpec {
            name: name.into(),
            path: path.as_ref().display().to_string(),
            ..RootSpec::default()
        }
    }

    /// This Root, writable.
    pub fn writable(self) -> RootSpec {
        RootSpec {
            writable: true,
            ..self
        }
    }

    /// This Root, of kind `kind`: writable too, unless general.
    pub fn of_kind(self, kind: RootKind) -> RootSpec {
        RootSpec {
            kind,
            writable: self.writable || kind != RootKind::General,
            ..self
        }
    }

    /// This Root, read-only but for `paths`.
    pub fn writing(self, paths: &[&str]) -> RootSpec {
        RootSpec {
            writable_paths: paths.iter().map(|p| (*p).to_owned()).collect(),
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
            writable: root.writable,
            writable_paths: root.writable_paths.clone(),
            readers: root.readers,
            read_mib_per_s: root.read_mib_per_s,
            write_mib_per_s: root.write_mib_per_s,
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
    /// Whether the Service can write to it now, as it would check when the Root is registered writable.
    pub writable: bool,
}
