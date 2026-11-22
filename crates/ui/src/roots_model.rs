//! The Roots screen's form, as typed, turned into the Root `POST /roots` registers or `PUT /roots/{name}` replaces;
//! and what the screen says of a Root and of the plans an edit voided.

use title_select_api::Root;
use title_select_api::roots::{RootEdited, RootKind, RootSpec, VoidedPlanKind};

/// A Root's form as typed: `writable_paths` one to a line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RootDraft {
    pub name: String,
    pub path: String,
    pub kind: RootKind,
    pub writable: bool,
    pub writable_paths: String,
}

impl RootDraft {
    /// The form of `root`, to edit it.
    pub fn of(root: &Root) -> RootDraft {
        RootDraft {
            name: root.name.clone(),
            path: root.path.clone(),
            kind: root.kind,
            writable: root.writable,
            writable_paths: root.writable_paths.join("\n"),
        }
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
    /// Writable paths are sent only for a read-only Root. The Service checks the rest.
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
        let writable_paths = if writable {
            Vec::new()
        } else {
            self.writable_paths
                .lines()
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(str::to_owned)
                .collect()
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

/// Where Title Select may write in `root`, as its cell says it; its writable paths have a column of their own.
pub fn access(root: &Root) -> &'static str {
    if root.writable {
        "Writable"
    } else {
        "Read-only"
    }
}

/// A line for each approved plan an edit voided: which, what it would have written to, and that it changed nothing.
pub fn voided_lines(edited: &RootEdited) -> Vec<String> {
    edited
        .voided
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
    use title_select_api::roots::VoidedPlan;

    use super::*;

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
    fn a_new_root_is_sent_trimmed_writable_for_its_kind_and_with_writable_paths_only_when_read_only()
     {
        let mut draft = RootDraft {
            name: " drop ".into(),
            path: " /srv/drop ".into(),
            kind: RootKind::Ingest,
            writable: false,
            writable_paths: "a\n".into(),
        };
        assert_eq!(
            draft.spec(None),
            Ok(RootSpec::new("drop", "/srv/drop").of_kind(RootKind::Ingest))
        );

        draft.kind = RootKind::General;
        assert_eq!(
            draft.spec(None),
            Ok(RootSpec::new("drop", "/srv/drop").writing(&["a"]))
        );

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
    fn an_edit_keeps_the_roots_name_and_ceilings() {
        let was = root();
        let mut draft = RootDraft::of(&was);
        assert_eq!(draft.spec(Some(&was)), Ok(RootSpec::from(&was)));

        draft.name = "renamed".into();
        draft.writable = true;
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
        let edited = RootEdited {
            root: root(),
            voided: vec![VoidedPlan {
                kind: VoidedPlanKind::Repair,
                plan: 12,
                job: 30,
                writes_to: "source set GBA".into(),
            }],
        };
        assert_eq!(
            voided_lines(&edited),
            [
                "The repair plan #12, writing to source set GBA: its job #30 failed, having changed nothing."
            ]
        );
    }
}
