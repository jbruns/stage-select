//! MAME Support Files: progetto-SNAPS's `catver.ini`, `languages.ini` and `bestgames.ini`, which the arcade selector
//! reads (ADR 0027). They carry no licence, so they are never shipped: Title Select fetches them under `[fetch] mame`,
//! or the Operator drops them in (ADR 0012's amendment).

use serde::{Deserialize, Serialize};

use crate::fetch::FetchArtifact;

/// Which Support File one is.
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

/// How a Support File came into the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportFileOrigin {
    /// The Operator dropped it in.
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

/// A Support File as the store keeps it, unmodified, like a DAT Revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupportFileRecord {
    /// SHA-256 of the unmodified file; its identity.
    pub sha256: String,
    pub kind: SupportFileKind,
    /// The original filename.
    pub file: String,
    /// The MAME version its `;; … MAME 0.NNN ;;` header names, such as `0.289`.
    pub mame_version: String,
    /// The mirror commit it was first fetched at; absent for one the Operator dropped in.
    pub commit: Option<String>,
    pub origin: SupportFileOrigin,
    pub source_url: Option<String>,
    pub imported_at: String,
}

/// The response to a Support File upload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupportFileImport {
    pub file: SupportFileRecord,
    pub outcome: crate::ImportOutcome,
}

/// How far into a file its header is looked for: progetto-SNAPS's comes after a short `[FOLDER_SETTINGS]` section.
const HEADER_WITHIN: usize = 4096;

/// A MAME version, such as `0.289`, the `0.289 (mame0289)` a ListXML's build names, or an update release such as
/// `0.149u1`, as numbers to compare: a release counts as update 0, so `0.149` < `0.149u1` < `0.150`.
pub fn version_number(version: &str) -> Option<(u32, u32, u32)> {
    let (major, minor) = version.split_whitespace().next()?.split_once('.')?;
    let (minor, update) = match minor.split_once('u') {
        Some((minor, update)) => (minor, update.parse().ok()?),
        None => (minor, 0),
    };
    Some((major.parse().ok()?, minor.parse().ok()?, update))
}

/// What a Support File's `;; <file> 0.NNN / <date> / MAME 0.NNN ;;` header says it is: which file, and the MAME
/// version it is for. `file` names it in an error.
pub fn read_header(
    bytes: &[u8],
    file: &str,
) -> std::result::Result<(SupportFileKind, String), String> {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(HEADER_WITHIN)]);
    let header = head
        .lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix(";;")?.strip_suffix(";;"))
        .ok_or_else(|| {
            format!("{file}: not a MAME support file: it has no \";; … MAME 0.NNN ;;\" header")
        })?;
    let named = header.split_whitespace().next().unwrap_or_default();
    let kind = SupportFileKind::ALL
        .into_iter()
        .find(|k| k.file_name().eq_ignore_ascii_case(named))
        .ok_or_else(|| {
            format!(
                "{file}: its header names {named}, but Title Select reads only catver.ini, languages.ini and \
                 bestgames.ini"
            )
        })?;
    let version = header
        .rsplit_once("MAME ")
        .and_then(|(_, v)| v.split_whitespace().next())
        .filter(|v| version_number(v).is_some())
        .ok_or_else(|| format!("{file}: its header names no MAME version, such as MAME 0.289"))?;
    Ok((kind, version.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::version_number;

    #[test]
    fn versions_compare_as_numbers() {
        assert!(version_number("0.99") < version_number("0.289 (mame0289)"));
        assert_eq!(version_number("mame0289"), None);
    }

    #[test]
    fn an_update_release_comes_between_its_release_and_the_next() {
        assert!(version_number("0.149") < version_number("0.149u1 (mame0149u1)"));
        assert!(version_number("0.149u1") < version_number("0.149u2"));
        assert!(version_number("0.149u2") < version_number("0.150"));
        assert_eq!(version_number("0.149ux"), None);
    }
}
