//! The Source Set registration form's pure logic: the path a Path Template proposes for a DAT, the live preview
//! of the folder the registration would use, whether the DAT takes a Coverage, and the Roots it may go in.

use stage_select_api::paths::{PathTemplate, TokenValues, relative_path};
use stage_select_api::roots::RootKind;
use stage_select_api::{Dat, Root};

/// The path `template` proposes for `dat`, or why it proposes none, as a sentence for the form.
pub fn propose(template: &str, dat: &Dat) -> Result<String, String> {
    PathTemplate::parse(template)
        .and_then(|t| {
            t.render(&TokenValues::for_provider(
                dat.provider.as_deref(),
                &dat.name,
            ))
        })
        .map_err(|e| sentence(&e.to_string()))
}

/// The path the form registers: the one you typed, else the proposal.
pub fn chosen_path(typed: Option<&str>, proposal: &Result<String, String>) -> Option<String> {
    match typed {
        Some(typed) => Some(typed.to_owned()),
        None => proposal.as_ref().ok().cloned(),
    }
}

/// The folder a registration of `path` in the Root at `root_path` would use, or why the path is not valid.
pub fn preview(root_path: &str, path: &str) -> Result<String, String> {
    let rel = relative_path(path).map_err(|e| sentence(&format!("the path {e}")))?;
    Ok(format!("{}/{rel}", root_path.trim_end_matches('/')))
}

/// Why a Source Set of the chosen `dat` takes no Coverage, as [`crate::source_set_model::no_coverage`] says from its
/// revisions' DAT Formats; `None` while none is chosen.
pub fn no_coverage(dat: Option<&Dat>) -> Option<&'static str> {
    let dat = dat?;
    crate::source_set_model::no_coverage(
        std::iter::once(dat.format).chain(dat.revisions.iter().map(|r| r.format)),
    )
}

/// The Coverage the form registers for `dat`: none for a DAT that takes none, whatever the editor held, else the
/// editor's `drafted` one.
pub fn coverage_for(
    dat: Option<&Dat>,
    drafted: Result<Option<Vec<String>>, String>,
) -> Result<Option<Vec<String>>, String> {
    match no_coverage(dat) {
        Some(_) => Ok(None),
        None => drafted,
    }
}

fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => {
            let mut out: String = first.to_uppercase().collect();
            out.push_str(chars.as_str());
            if !out.ends_with('.') {
                out.push('.');
            }
            out
        }
        None => String::new(),
    }
}

/// Whether a Source Set may be registered in `root`: only a read-only general Root holds them.
pub fn holds_source_sets(root: &Root) -> bool {
    root.kind == RootKind::General && !root.writable
}

/// The Roots the form offers for a Source Set, in the order given: the first is its default.
pub fn set_roots(roots: Vec<Root>) -> Vec<Root> {
    roots.into_iter().filter(holds_source_sets).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use stage_select_api::selection::ARCADE_COVERAGE_REFUSAL;

    fn root(name: &str, kind: RootKind, writable: bool) -> Root {
        Root {
            name: name.into(),
            path: format!("/mnt/{name}"),
            kind,
            writable,
            writable_paths: Vec::new(),
            readers: None,
            read_mib_per_s: None,
            write_mib_per_s: None,
            updated_by: "Operator token".into(),
            updated_at: "2026-11-22T10:00:00Z".into(),
        }
    }

    #[test]
    fn a_source_set_is_offered_only_the_read_only_general_roots_and_defaults_to_the_first() {
        let roots = vec![
            root("play", RootKind::General, true),
            root("in", RootKind::Ingest, true),
            root("additions", RootKind::Additions, true),
            root("games", RootKind::General, false),
            root("more", RootKind::General, false),
        ];
        let offered: Vec<String> = set_roots(roots).into_iter().map(|r| r.name).collect();
        assert_eq!(offered, ["games", "more"]);
        assert!(set_roots(vec![root("play", RootKind::General, true)]).is_empty());
    }

    fn dat(provider: &str, name: &str) -> Dat {
        Dat {
            dat: format!("{provider}/{name}"),
            format: stage_select_api::DatFormat::Logiqx,
            provider: Some(provider.into()),
            name: name.into(),
            revisions: Vec::new(),
        }
    }

    #[test]
    fn a_mame_listxml_is_registered_with_no_coverage_whatever_the_draft_says() {
        let mut mame = dat("MAME", "MAME");
        mame.format = stage_select_api::DatFormat::Listxml;
        let europe = Ok(Some(vec!["Europe".to_owned()]));

        assert_eq!(no_coverage(Some(&mame)), Some(ARCADE_COVERAGE_REFUSAL));
        assert_eq!(coverage_for(Some(&mame), europe.clone()), Ok(None));
        // A draft the Operator left invalid is no reason to refuse it either.
        assert_eq!(
            coverage_for(Some(&mame), Err("Choose a region.".into())),
            Ok(None)
        );

        let atari = dat("No-Intro", "Atari - Atari 2600");
        assert_eq!(no_coverage(Some(&atari)), None);
        assert_eq!(coverage_for(Some(&atari), europe.clone()), europe);
        assert_eq!(no_coverage(None), None);
    }

    #[test]
    fn the_atari_dat_is_proposed_its_provider_and_name_and_previewed_in_its_root() {
        let atari = dat("No-Intro", "Atari - Atari 2600");

        let proposal = propose("{provider}/{dat}", &atari);

        assert_eq!(proposal.as_deref(), Ok("No-Intro/Atari - Atari 2600"));
        let path = chosen_path(None, &proposal).unwrap();
        assert_eq!(
            preview("/data/sets/", &path).as_deref(),
            Ok("/data/sets/No-Intro/Atari - Atari 2600")
        );
    }

    #[test]
    fn a_typed_path_overrides_the_proposal_and_an_unmapped_system_asks_for_one() {
        let studio = dat("No-Intro", "RCA - Studio II");

        let proposal = propose("{platform}", &studio);

        let reason = proposal.clone().unwrap_err();
        assert!(reason.starts_with("No platform folder name"), "{reason}");
        assert!(reason.ends_with("type the path."), "{reason}");
        assert_eq!(chosen_path(None, &proposal), None);
        assert_eq!(
            chosen_path(Some("studio2"), &proposal).as_deref(),
            Some("studio2")
        );
        assert_eq!(
            preview("/sets", "../elsewhere"),
            Err("The path must not contain `..`.".into())
        );
    }
}
