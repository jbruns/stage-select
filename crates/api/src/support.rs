//! MAME support files: progetto-SNAPS's `catver.ini`, `languages.ini` and `bestgames.ini`, which the arcade selector
//! reads (ADR 0027). They carry no licence, so they are never shipped: Title Select fetches them under `[fetch] mame`,
//! or you drop them in (ADR 0012's amendment).

use serde::{Deserialize, Serialize};

use crate::fetch::FetchArtifact;

/// Which support file one is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportFileKind {
    /// Each machine's category, such as `Maze / Shooter Small`, and whether it is Mature.
    Catver,
    /// The languages each machine is in.
    Languages,
    /// Each machine's rating, in tiers.
    Bestgames,
}

impl SupportFileKind {
    pub const ALL: [SupportFileKind; 3] = [
        SupportFileKind::Catver,
        SupportFileKind::Languages,
        SupportFileKind::Bestgames,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            SupportFileKind::Catver => "catver",
            SupportFileKind::Languages => "languages",
            SupportFileKind::Bestgames => "bestgames",
        }
    }

    pub fn parse(s: &str) -> Option<SupportFileKind> {
        SupportFileKind::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// Its file's name, as progetto-SNAPS publishes it and its header names it.
    pub fn file_name(self) -> &'static str {
        match self {
            SupportFileKind::Catver => "catver.ini",
            SupportFileKind::Languages => "languages.ini",
            SupportFileKind::Bestgames => "bestgames.ini",
        }
    }

    /// What a request fetching it is recorded as.
    pub fn artifact(self) -> FetchArtifact {
        match self {
            SupportFileKind::Catver => FetchArtifact::Catver,
            SupportFileKind::Languages => FetchArtifact::Languages,
            SupportFileKind::Bestgames => FetchArtifact::Bestgames,
        }
    }
}

/// How a support file came into the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportFileOrigin {
    /// You dropped it in.
    Import,
    /// Title Select fetched it.
    Fetch,
}

impl SupportFileOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            SupportFileOrigin::Import => "import",
            SupportFileOrigin::Fetch => "fetch",
        }
    }
}

/// A support file as the store keeps it, unmodified, like a DAT Revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupportFileRecord {
    /// SHA-256 of the unmodified file; its identity.
    pub sha256: String,
    pub kind: SupportFileKind,
    /// The original filename.
    pub file: String,
    /// The MAME version its `;; … MAME 0.NNN ;;` header names, such as `0.289`.
    pub mame_version: String,
    /// The mirror commit it was first fetched at; absent for one you dropped in.
    pub commit: Option<String>,
    pub origin: SupportFileOrigin,
    pub source_url: Option<String>,
    pub imported_at: String,
}

/// The response to a support file upload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupportFileImport {
    pub file: SupportFileRecord,
    pub outcome: crate::ImportOutcome,
}
