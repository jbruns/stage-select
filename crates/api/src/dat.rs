//! What a DAT says about itself: its DAT Format, and the DAT Provider its header signals, if any.
//!
//! A DAT is identified as `<provider>/<name>`: a recognised DAT Provider and the DAT's native name, or, when no provider
//! is recognised, a reserved provider part naming its DAT Format, so that two unrelated DATs of one name in different
//! formats stay apart. Neither a format nor an author alone establishes a DAT Provider.

use serde::{Deserialize, Serialize};

/// The DAT Provider No-Intro, spelled as the platform table spells it.
pub const NO_INTRO: &str = "No-Intro";
/// The DAT Provider Redump, spelled as the platform table, storage defaults and fetching spell it.
pub const REDUMP: &str = "Redump";
/// The MAME family: a grouping of ListXML and software lists by their format, not proof of who published them.
pub const MAME: &str = "MAME";

/// How a DAT file is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatFormat {
    /// Logiqx XML (`<datafile>`), as No-Intro and Redump publish.
    Logiqx,
    /// MAME's ListXML (`<mame>`).
    Listxml,
    /// A MAME software list (`<softwarelist>`).
    SoftwareList,
}

impl DatFormat {
    pub const ALL: [DatFormat; 3] = [
        DatFormat::Logiqx,
        DatFormat::Listxml,
        DatFormat::SoftwareList,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            DatFormat::Logiqx => "logiqx",
            DatFormat::Listxml => "listxml",
            DatFormat::SoftwareList => "software_list",
        }
    }

    pub fn parse(s: &str) -> Option<DatFormat> {
        DatFormat::ALL.into_iter().find(|f| f.as_str() == s)
    }

    /// Its name for a reader.
    pub fn label(self) -> &'static str {
        match self {
            DatFormat::Logiqx => "Logiqx XML",
            DatFormat::Listxml => "MAME ListXML",
            DatFormat::SoftwareList => "MAME software list",
        }
    }

    /// The provider part of `<provider>/<name>` for a DAT of this format whose provider is unknown. No provider is
    /// recognised by this spelling, in any case.
    pub fn unknown_provider(self) -> &'static str {
        match self {
            DatFormat::Logiqx => "unknown-logiqx",
            DatFormat::Listxml => "unknown-listxml",
            DatFormat::SoftwareList => "unknown-software-list",
        }
    }
}

/// What a DAT's header says of where it comes from, as written; each is absent when the header doesn't give it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HeaderClaims<'a> {
    pub homepage: Option<&'a str>,
    pub url: Option<&'a str>,
    pub author: Option<&'a str>,
}

/// The DAT Provider a DAT of `format` with these header claims is recognised as, or `None` when it is unknown.
///
/// A Logiqx DAT is No-Intro when its homepage is `No-Intro` or its URL is on `no-intro.org`, and Redump when its
/// homepage, URL or author names `redump.org`; signals for both, or neither, leave it unknown. An author never names
/// No-Intro: its DATs list their contributors there. ListXML and software lists are the MAME family.
pub fn recognise_provider(format: DatFormat, claims: HeaderClaims) -> Option<&'static str> {
    match format {
        DatFormat::Listxml | DatFormat::SoftwareList => Some(MAME),
        DatFormat::Logiqx => {
            let no_intro = claims
                .homepage
                .is_some_and(|h| h.trim().eq_ignore_ascii_case("No-Intro"))
                || claims.url.is_some_and(|u| names_host(u, "no-intro.org"));
            let redump = [claims.homepage, claims.url, claims.author]
                .into_iter()
                .flatten()
                .any(|text| names_host(text, "redump.org"));
            match (no_intro, redump) {
                (true, false) => Some(NO_INTRO),
                (false, true) => Some(REDUMP),
                _ => None,
            }
        }
    }
}

/// Whether `text` is `domain`, a host under it, or a URL on either, ignoring case.
fn names_host(text: &str, domain: &str) -> bool {
    let text = text.trim().to_ascii_lowercase();
    let rest = text
        .split_once("://")
        .map_or(text.as_str(), |(_, rest)| rest);
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = host.rsplit_once('@').map_or(host, |(_, host)| host);
    let host = host.split(':').next().unwrap_or_default();
    host == domain
        || host
            .strip_suffix(domain)
            .is_some_and(|sub| sub.ends_with('.'))
}

/// The provider part of the key of a DAT of `format` whose provider is `provider`.
pub fn provider_part(provider: Option<&str>, format: DatFormat) -> String {
    provider.map_or_else(|| format.unknown_provider().to_owned(), str::to_owned)
}

/// The DAT Provider a key's provider part names: `None` for a part reserved for a DAT whose provider is unknown.
pub fn known_provider(part: &str) -> Option<&str> {
    let reserved = DatFormat::ALL
        .iter()
        .any(|f| f.unknown_provider().eq_ignore_ascii_case(part));
    (!reserved).then_some(part)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn logiqx(
        homepage: Option<&str>,
        url: Option<&str>,
        author: Option<&str>,
    ) -> Option<&'static str> {
        recognise_provider(
            DatFormat::Logiqx,
            HeaderClaims {
                homepage,
                url,
                author,
            },
        )
    }

    #[test]
    fn a_no_intro_header_is_no_intro_by_its_homepage_or_url() {
        assert_eq!(
            logiqx(
                Some("No-Intro"),
                Some("https://www.no-intro.org"),
                Some("Alpha, Beta, Gamma")
            ),
            Some(NO_INTRO)
        );
        assert_eq!(logiqx(Some("no-intro"), None, None), Some(NO_INTRO));
        assert_eq!(
            logiqx(None, Some("https://no-intro.org/"), None),
            Some(NO_INTRO)
        );
        assert_eq!(logiqx(None, Some("https://not-no-intro.org"), None), None);
    }

    #[test]
    fn a_redump_header_is_redump_by_its_homepage_url_or_author() {
        assert_eq!(
            logiqx(
                Some("redump.org"),
                Some("http://redump.org/"),
                Some("redump.org")
            ),
            Some(REDUMP)
        );
        assert_eq!(logiqx(None, Some("http://redump.org/"), None), Some(REDUMP));
        assert_eq!(logiqx(None, None, Some("Redump.org")), Some(REDUMP));
        assert_eq!(logiqx(Some("redump.org.example"), None, None), None);
    }

    #[test]
    fn an_author_alone_never_names_no_intro() {
        assert_eq!(logiqx(None, None, Some("No-Intro")), None);
        assert_eq!(logiqx(None, None, Some("no-intro.org")), None);
    }

    #[test]
    fn signals_for_both_providers_or_neither_leave_the_provider_unknown() {
        assert_eq!(
            logiqx(Some("No-Intro"), Some("http://redump.org/"), None),
            None
        );
        assert_eq!(logiqx(Some("No-Intro"), None, Some("redump.org")), None);
        assert_eq!(logiqx(None, None, None), None);
        assert_eq!(
            logiqx(
                Some("Someone's site"),
                Some("https://example.com"),
                Some("Someone")
            ),
            None
        );
    }

    #[test]
    fn mame_formats_are_the_mame_family_whatever_their_header_says() {
        let claims = HeaderClaims {
            homepage: Some("No-Intro"),
            ..HeaderClaims::default()
        };
        assert_eq!(recognise_provider(DatFormat::Listxml, claims), Some(MAME));
        assert_eq!(
            recognise_provider(DatFormat::SoftwareList, HeaderClaims::default()),
            Some(MAME)
        );
    }

    #[test]
    fn an_unknown_provider_keys_by_format_so_unrelated_dats_of_one_name_stay_apart() {
        let parts: Vec<String> = DatFormat::ALL
            .into_iter()
            .map(|f| provider_part(None, f))
            .collect();
        assert_eq!(
            parts,
            ["unknown-logiqx", "unknown-listxml", "unknown-software-list"]
        );
        for part in &parts {
            assert_eq!(known_provider(part), None);
            assert!(!part.contains('/'));
        }
        assert_eq!(known_provider("Unknown-Logiqx"), None);
        assert_eq!(provider_part(Some(NO_INTRO), DatFormat::Logiqx), "No-Intro");
        assert_eq!(known_provider("No-Intro"), Some("No-Intro"));
    }
}
