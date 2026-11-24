//! The Roots screen's form, as typed, turned into the Root `POST /roots` registers or `PUT /roots/{name}` replaces;
//! and what the screen says of a Root and of the plans an edit voided.

use stage_select_api::roots::{RootKind, RootSpec, VoidedPlan, VoidedPlanKind};
use stage_select_api::{Root, SourceSetRecord};

/// A Root's form as typed. Its `writable_paths` aren't in it: each Source Set switches its own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RootDraft {
    pub name: String,
    pub path: String,
    pub kind: RootKind,
    pub writable: bool,
    /// Create the folder if it is missing, when registering.
    pub create: bool,
}

impl RootDraft {
    /// The form of `root`, to edit it.
    pub fn of(root: &Root) -> RootDraft {
        RootDraft {
            name: root.name.clone(),
            path: root.path.clone(),
            kind: root.kind,
            writable: root.writable,
            create: false,
        }
    }

    /// Fills in the path you chose, and names the root for its last folder if you haven't named it.
    pub fn choose_path(&mut self, path: String) {
        if self.name.trim().is_empty() {
            self.name = crate::welcome_model::root_name(&path);
        }
        self.path = path;
    }

    /// Whether the writable box can be changed: an ingest or additions root is always writable.
    pub fn writable_fixed(&self) -> bool {
        self.kind != RootKind::General
    }

    /// Whether it may be writable: as typed, or because of its kind.
    pub fn is_writable(&self) -> bool {
        self.writable || self.writable_fixed()
    }

    /// The Root to send: `was` is the Root being edited, whose Ceilings it keeps, as the form doesn't show them.
    /// A read-only Root keeps the writable paths `was` has, as each Source Set switches its own; a writable Root has
    /// none. Only a registration asks for its folder to be created. The Service checks the rest.
    pub fn spec(&self, was: Option<&Root>) -> Result<RootSpec, String> {
        let name = match was {
            Some(root) => root.name.clone(),
            None => self.name.trim().to_owned(),
        };
        if name.is_empty() {
            return Err("Give the root a name.".into());
        }
        let path = self.path.trim();
        if path.is_empty() {
            return Err("Give the path of the root's folder.".into());
        }
        let writable = self.is_writable();
        let writable_paths = match was {
            Some(root) if !writable => root.writable_paths.clone(),
            _ => Vec::new(),
        };
        Ok(RootSpec {
            name,
            path: path.to_owned(),
            kind: self.kind,
            writable,
            writable_paths,
            readers: was.and_then(|r| r.readers),
            read_mib_per_s: was.and_then(|r| r.read_mib_per_s),
            write_mib_per_s: was.and_then(|r| r.write_mib_per_s),
            create: self.create && was.is_none(),
        })
    }
}

/// A kind as its column and its choice say it.
pub fn kind_label(kind: RootKind) -> &'static str {
    match kind {
        RootKind::General => "General",
        RootKind::Ingest => "Ingest folder",
        RootKind::Additions => "Additions sets",
    }
}

/// What a kind is for, under its choice.
pub fn kind_note(kind: RootKind) -> &'static str {
    match kind {
        RootKind::General => {
            "Holds source sets, if read-only, or play library folders, if writable."
        }
        RootKind::Ingest => {
            "Where new files arrive. They move out only in plans you approve. Always writable."
        }
        RootKind::Additions => {
            "Where your additions sets live, a folder for each system. Always writable, and there is only one."
        }
    }
}

/// Where Stage Select may write in `root`, as its cell says it; the Source Sets it may change have a column of their
/// own.
pub fn access(root: &Root) -> &'static str {
    if root.writable {
        "Writable"
    } else {
        "Read-only"
    }
}

/// The names of the Source Sets in the Root `root` that Stage Select may change, in `sets`' order.
pub fn changeable_sets(root: &str, sets: &[SourceSetRecord]) -> Vec<String> {
    sets.iter()
        .filter(|s| s.root == root && s.repairs_may_change)
        .map(|s| s.name.clone())
        .collect()
}

/// What a read-only Root's form says of the `count` Source Sets in it that Stage Select may change.
pub fn changeable_line(count: usize) -> String {
    match count {
        0 => "Stage Select may change no source sets in this root.".into(),
        1 => "Stage Select may change 1 source set in this root:".into(),
        n => format!("Stage Select may change {n} source sets in this root:"),
    }
}

/// A line for each approved plan a change voided: which, what it would have written to, and that it changed nothing.
pub fn voided_lines(voided: &[VoidedPlan]) -> Vec<String> {
    voided
        .iter()
        .map(|v| {
            let kind = match v.kind {
                VoidedPlanKind::Build => "build",
                VoidedPlanKind::Repair => "repair",
            };
            format!(
                "The {kind} plan #{}, writing to {}: its job #{} failed, having changed nothing.",
                v.plan, v.writes_to, v.job
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chosen_path_names_an_unnamed_root_for_its_last_folder() {
        let mut draft = RootDraft::default();
        draft.choose_path("/mnt/roms".into());
        assert_eq!(
            (draft.name.as_str(), draft.path.as_str()),
            ("roms", "/mnt/roms")
        );
        draft.choose_path("/srv/games".into());
        assert_eq!(
            (draft.name.as_str(), draft.path.as_str()),
            ("roms", "/srv/games"),
            "a name you have is kept"
        );
    }

    fn root() -> Root {
        Root {
            name: "sets".into(),
            path: "/srv/sets".into(),
            kind: RootKind::General,
            writable: false,
            writable_paths: vec!["Nintendo - GBA".into(), "Sega - Saturn".into()],
            readers: Some(4),
            read_mib_per_s: None,
            write_mib_per_s: Some(90),
            updated_by: "Operator token".into(),
            updated_at: "2025-01-02T03:04:05Z".into(),
        }
    }

    #[test]
    fn a_new_root_is_sent_trimmed_writable_for_its_kind_with_no_writable_paths_and_created_if_asked()
     {
        let mut draft = RootDraft {
            name: " drop ".into(),
            path: " /srv/drop ".into(),
            kind: RootKind::Ingest,
            writable: false,
            create: false,
        };
        assert_eq!(
            draft.spec(None),
            Ok(RootSpec::new("drop", "/srv/drop").of_kind(RootKind::Ingest))
        );

        draft.kind = RootKind::General;
        assert_eq!(draft.spec(None), Ok(RootSpec::new("drop", "/srv/drop")));
        draft.create = true;
        assert_eq!(
            draft.spec(None),
            Ok(RootSpec::new("drop", "/srv/drop").creating())
        );
        draft.create = false;

        draft.name = "  ".into();
        assert_eq!(draft.spec(None), Err("Give the root a name.".into()));
        draft.name = "drop".into();
        draft.path = String::new();
        assert_eq!(
            draft.spec(None),
            Err("Give the path of the root's folder.".into())
        );
    }

    #[test]
    fn an_edit_keeps_the_roots_name_ceilings_and_the_source_sets_it_may_change_and_creates_nothing()
    {
        let was = root();
        let mut draft = RootDraft::of(&was);
        assert_eq!(draft.spec(Some(&was)), Ok(RootSpec::from(&was)));

        draft.name = "renamed".into();
        draft.writable = true;
        draft.create = true;
        assert_eq!(
            draft.spec(Some(&was)),
            Ok(RootSpec {
                writable: true,
                writable_paths: Vec::new(),
                ..RootSpec::from(&was)
            })
        );
    }

    #[test]
    fn a_voided_plan_says_what_it_would_have_written_to() {
        let voided = [VoidedPlan {
            kind: VoidedPlanKind::Repair,
            plan: 12,
            job: 30,
            writes_to: "source set GBA".into(),
        }];
        assert_eq!(
            voided_lines(&voided),
            [
                "The repair plan #12, writing to source set GBA: its job #30 failed, having changed nothing."
            ]
        );
    }

    fn set(name: &str, root: &str, may_change: bool) -> SourceSetRecord {
        SourceSetRecord {
            name: name.into(),
            root: root.into(),
            path: name.into(),
            dat: "No-Intro/Nintendo - Game Boy".into(),
            added_at: "2026-01-01T00:00:00Z".into(),
            last_verified_against: None,
            follow: false,
            stale: Vec::new(),
            coverage: None,
            repairs_may_change: may_change,
            folder_created: None,
        }
    }

    #[test]
    fn a_root_lists_the_source_sets_in_it_that_stage_select_may_change() {
        let sets = [
            set("GBA", "sets", true),
            set("Saturn", "sets", false),
            set("NES", "other", true),
            set("SNES", "sets", true),
        ];
        assert_eq!(changeable_sets("sets", &sets), ["GBA", "SNES"]);
        assert_eq!(
            changeable_line(0),
            "Stage Select may change no source sets in this root."
        );
        assert_eq!(
            changeable_line(1),
            "Stage Select may change 1 source set in this root:"
        );
        assert_eq!(
            changeable_line(3),
            "Stage Select may change 3 source sets in this root:"
        );
    }
}
