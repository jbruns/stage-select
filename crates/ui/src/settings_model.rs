//! The Operations settings as the Operator edits them: a draft of every field, as typed, turned into the change
//! `PATCH /settings` takes. Only the sections that changed are sent, so a save never touches a section someone else
//! changed meanwhile, and the webhook's URL and secret and the RomM token are sent only when replaced or cleared.

use std::collections::BTreeMap;

use title_select_api::paths::PathTemplate;
use title_select_api::romm::{RommCheck, RommPlatform, RommUpdate, TESTED_RELEASES};
use title_select_api::settings::{
    ChdTimeouts, FetchSettings, IngestCadence, NotifyEvent, NotifyUpdate, Pace, PathTemplates,
    Retention, SecretUpdate, SettingsUpdate, SettingsView, SourceSetChecks,
};

use crate::parts::status::Tone;

/// An Ingest Folder's cadence as typed: an empty `scan_every_minutes` scans only when asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CadenceDraft {
    pub settle_secs: String,
    pub scan_every_minutes: String,
}

/// Every Operations setting as typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub pace: Pace,
    pub audits_keep: String,
    pub selections_keep: String,
    pub source_set_template: String,
    pub library_folder_template: String,
    pub check_every_minutes: String,
    pub fetch: FetchSettings,
    pub chd_seconds: String,
    pub chd_seconds_per_gib: String,
    pub ingest_folders: BTreeMap<String, CadenceDraft>,
    pub url: SecretUpdate,
    pub secret: SecretUpdate,
    pub events: Vec<NotifyEvent>,
    pub workbench_url: String,
    pub romm_enabled: bool,
    pub romm_base_url: String,
    pub romm_token: SecretUpdate,
    /// The Root RomM's library is in, or empty for none.
    pub romm_root: String,
}

impl Draft {
    /// The settings as saved, untouched.
    pub fn of(saved: &SettingsView) -> Draft {
        Draft {
            pace: saved.jobs.pace,
            audits_keep: saved.retention.audits_keep.to_string(),
            selections_keep: saved.retention.selections_keep.to_string(),
            source_set_template: saved.path_templates.source_set.as_str().to_owned(),
            library_folder_template: saved.path_templates.library_folder.as_str().to_owned(),
            check_every_minutes: saved.source_sets.check_every_minutes.to_string(),
            fetch: saved.fetch,
            chd_seconds: saved.chd.seconds.to_string(),
            chd_seconds_per_gib: saved.chd.seconds_per_gib.to_string(),
            ingest_folders: saved
                .ingest_folders
                .iter()
                .map(|(root, c)| {
                    (
                        root.clone(),
                        CadenceDraft {
                            settle_secs: c.settle_secs.to_string(),
                            scan_every_minutes: c
                                .scan_every_minutes
                                .map(|m| m.to_string())
                                .unwrap_or_default(),
                        },
                    )
                })
                .collect(),
            url: SecretUpdate::Keep,
            secret: SecretUpdate::Keep,
            events: saved.notify.events.clone(),
            workbench_url: saved.notify.workbench_url.clone().unwrap_or_default(),
            romm_enabled: saved.romm.enabled,
            romm_base_url: saved.romm.base_url.clone().unwrap_or_default(),
            romm_token: SecretUpdate::Keep,
            romm_root: saved.romm.root.clone().unwrap_or_default(),
        }
    }

    /// Turns `event` on or off, keeping the events in their usual order.
    pub fn set_event(&mut self, event: NotifyEvent, on: bool) {
        self.events.retain(|e| *e != event);
        if on {
            self.events.push(event);
        }
        self.events
            .sort_by_key(|e| NotifyEvent::ALL.iter().position(|a| a == e));
    }

    /// The change that saves this draft over `saved`: only the sections that differ. A field that isn't a whole
    /// number, or a template that doesn't parse, refuses the save and says which.
    pub fn update(&self, saved: &SettingsView) -> Result<SettingsUpdate, String> {
        let retention = Retention {
            audits_keep: whole("Audits kept", &self.audits_keep)?,
            selections_keep: whole("Selections kept", &self.selections_keep)?,
        };
        let path_templates = PathTemplates {
            source_set: template("The source set path template", &self.source_set_template)?,
            library_folder: template(
                "The library folder path template",
                &self.library_folder_template,
            )?,
        };
        let source_sets = SourceSetChecks {
            check_every_minutes: whole("Check for changes every", &self.check_every_minutes)?,
        };
        let chd = ChdTimeouts {
            seconds: whole("Each chdman run's time limit", &self.chd_seconds)?,
            seconds_per_gib: whole("The time added for each GiB", &self.chd_seconds_per_gib)?,
        };
        let mut ingest_folders = BTreeMap::new();
        for (root, draft) in &self.ingest_folders {
            let cadence = IngestCadence {
                settle_secs: whole(&format!("{root}: settle after"), &draft.settle_secs)?,
                scan_every_minutes: match draft.scan_every_minutes.trim() {
                    "" => None,
                    every => Some(whole(&format!("{root}: scan every"), every)?),
                },
            };
            if saved.ingest_folders.get(root) != Some(&cadence) {
                ingest_folders.insert(root.clone(), cadence);
            }
        }
        let workbench_url = Some(self.workbench_url.trim().trim_end_matches('/'))
            .filter(|u| !u.is_empty())
            .map(str::to_owned);
        let notify = NotifyUpdate {
            url: self.url.clone(),
            secret: self.secret.clone(),
            events: self.events.clone(),
            workbench_url,
        };
        let notify_changed = notify.url != SecretUpdate::Keep
            || notify.secret != SecretUpdate::Keep
            || notify.events != saved.notify.events
            || notify.workbench_url != saved.notify.workbench_url;
        let romm = RommUpdate {
            enabled: self.romm_enabled,
            base_url: Some(self.romm_base_url.trim().trim_end_matches('/'))
                .filter(|u| !u.is_empty())
                .map(str::to_owned),
            token: self.romm_token.clone(),
            root: Some(self.romm_root.trim())
                .filter(|r| !r.is_empty())
                .map(str::to_owned),
        };
        let romm_changed = romm.token != SecretUpdate::Keep
            || romm.enabled != saved.romm.enabled
            || romm.base_url != saved.romm.base_url
            || romm.root != saved.romm.root;
        Ok(SettingsUpdate {
            pace: (self.pace != saved.jobs.pace).then_some(self.pace),
            retention: (retention != saved.retention).then_some(retention),
            path_templates: (path_templates != saved.path_templates).then_some(path_templates),
            source_sets: (source_sets != saved.source_sets).then_some(source_sets),
            fetch: (self.fetch != saved.fetch).then_some(self.fetch),
            chd: (chd != saved.chd).then_some(chd),
            ingest_folders: (!ingest_folders.is_empty()).then_some(ingest_folders),
            notify: notify_changed.then_some(notify),
            romm: romm_changed.then_some(romm),
        })
    }
}

fn whole<T: std::str::FromStr>(what: &str, text: &str) -> Result<T, String> {
    text.trim()
        .parse()
        .map_err(|_| format!("{what}: enter a whole number."))
}

fn template(what: &str, text: &str) -> Result<PathTemplate, String> {
    PathTemplate::parse(text.trim()).map_err(|e| format!("{what}: {e}"))
}

/// What a secret's control says: whether it is set, and what saving will do to it.
pub fn secret_state(set: bool, update: &SecretUpdate) -> &'static str {
    match (update, set) {
        (SecretUpdate::Set(_), true) => "Set; replaced when you save",
        (SecretUpdate::Set(_), false) => "Not set; set when you save",
        (SecretUpdate::Clear, true) => "Set; cleared when you save",
        (SecretUpdate::Clear, false) | (SecretUpdate::Keep, false) => "Not set",
        (SecretUpdate::Keep, true) => "Set",
    }
}

/// What a test of the RomM connection found, a line each, in the tone it deserves: the release, the user the token
/// signs in as, its scopes, and what stopped the test if anything did.
pub fn romm_check_lines(check: &RommCheck) -> Vec<(Tone, String)> {
    let mut lines = Vec::new();
    if let Some(version) = &check.version {
        lines.push(if check.version_tested {
            (Tone::Good, format!("RomM {version}, a tested release."))
        } else {
            (
                Tone::Review,
                format!(
                    "RomM {version}, a release not yet tested: title-select is tested with RomM \
                     {TESTED_RELEASES}."
                ),
            )
        });
    }
    if check.failure.is_none() {
        lines.push(match &check.user {
            Some(user) if user.admin => (
                Tone::Review,
                format!(
                    "Signed in as {}, an admin: a user with only the scopes title-select needs is safer.",
                    user.username
                ),
            ),
            Some(user) => (Tone::Good, format!("Signed in as {}.", user.username)),
            None => (
                Tone::Neutral,
                "Add the me.read scope to your token to see which user it signs in as.".into(),
            ),
        });
        lines.extend(check.missing_scopes.iter().map(|scope| {
            (
                Tone::Failed,
                format!("Your token is missing the {scope} scope."),
            )
        }));
        if check.missing_scopes.is_empty() {
            lines.push((
                Tone::Good,
                "Your token has every scope title-select needs.".into(),
            ));
        }
    }
    if let Some(failure) = &check.failure {
        lines.push((Tone::Failed, failure.clone()));
    }
    lines
}

/// The RomM platform a library folder maps to, terse for its cell, and why it maps to none for its tooltip.
pub fn romm_platform_label(platform: &RommPlatform) -> (String, Option<&'static str>) {
    let none = |why| ("No platform".to_owned(), Some(why));
    match platform {
        RommPlatform::Mapped { fs_slug } => (fs_slug.clone(), None),
        RommPlatform::NoRoot => {
            none("Choose the root RomM's library is in to map your library folders.")
        }
        RommPlatform::OutsideRoot => {
            none("This library folder is outside the root RomM's library is in.")
        }
        RommPlatform::Nested => {
            none("RomM sees only the folders directly in its library; this one is nested deeper.")
        }
        RommPlatform::IsRoot => {
            none("This library folder is RomM's whole library, not a platform folder in it.")
        }
        RommPlatform::Unresolved => {
            none("This library folder or the root RomM's library is in can't be found on disk.")
        }
    }
}

/// How `pace` reads.
pub fn pace_label(pace: Pace) -> &'static str {
    match pace {
        Pace::Gentle => "Gentle: a quarter of each ceiling",
        Pace::Default => "Default: half of each ceiling",
        Pace::Unthrottled => "Unthrottled: every ceiling in full",
    }
}

#[cfg(test)]
mod tests {
    use title_select_api::romm::{RommUser, RommView};
    use title_select_api::settings::{Ceilings, JobsView, NotifyView};

    use super::*;

    fn saved() -> SettingsView {
        let ceilings = Ceilings {
            concurrent: 4,
            readers: 8,
            hashers: 8,
        };
        SettingsView {
            jobs: JobsView {
                pace: Pace::Default,
                ceilings,
                active: Pace::Default.counts(ceilings),
            },
            retention: Retention::default(),
            path_templates: PathTemplates::default(),
            source_sets: SourceSetChecks::default(),
            fetch: FetchSettings::default(),
            chd: ChdTimeouts::default(),
            ingest_folders: [("drop".to_owned(), IngestCadence::default())].into(),
            notify: NotifyView {
                url_set: true,
                secret_set: true,
                events: NotifyEvent::ALL.to_vec(),
                workbench_url: Some("https://ts.example".into()),
            },
            romm: RommView {
                enabled: false,
                base_url: Some("http://romm.example:8080".into()),
                token_set: true,
                root: Some("romm".into()),
            },
        }
    }

    #[test]
    fn an_untouched_draft_changes_nothing() {
        let saved = saved();
        assert_eq!(
            Draft::of(&saved).update(&saved),
            Ok(SettingsUpdate::default())
        );
    }

    #[test]
    fn only_the_sections_you_changed_are_sent() {
        let saved = saved();
        let mut draft = Draft::of(&saved);
        draft.pace = Pace::Unthrottled;
        draft.audits_keep = " 5 ".into();
        draft
            .ingest_folders
            .get_mut("drop")
            .unwrap()
            .scan_every_minutes = "30".into();
        let update = draft.update(&saved).unwrap();
        assert_eq!(update.pace, Some(Pace::Unthrottled));
        assert_eq!(
            update.retention,
            Some(Retention {
                audits_keep: 5,
                selections_keep: 20
            })
        );
        assert_eq!(
            update.ingest_folders,
            Some(
                [(
                    "drop".to_owned(),
                    IngestCadence {
                        settle_secs: 60,
                        scan_every_minutes: Some(30)
                    }
                )]
                .into()
            )
        );
        assert_eq!(update.path_templates, None);
        assert_eq!(update.fetch, None);
        assert_eq!(update.notify, None);
    }

    #[test]
    fn the_webhook_is_sent_whole_but_its_secrets_only_when_replaced_or_cleared() {
        let saved = saved();
        let mut draft = Draft::of(&saved);
        draft.set_event(NotifyEvent::JobFinished, false);
        draft.workbench_url = "https://ts.example/".into();
        let notify = draft.update(&saved).unwrap().notify.unwrap();
        assert_eq!(
            (notify.url, notify.secret),
            (SecretUpdate::Keep, SecretUpdate::Keep)
        );
        assert_eq!(notify.events, &NotifyEvent::ALL[1..]);
        assert_eq!(notify.workbench_url.as_deref(), Some("https://ts.example"));

        draft.set_event(NotifyEvent::JobFinished, true);
        assert_eq!(draft.events, NotifyEvent::ALL, "back in their usual order");
        draft.secret = SecretUpdate::Clear;
        assert_eq!(
            draft.update(&saved).unwrap().notify.unwrap().secret,
            SecretUpdate::Clear
        );
    }

    #[test]
    fn a_field_that_isnt_a_whole_number_or_a_template_that_doesnt_parse_is_named() {
        let saved = saved();
        let mut draft = Draft::of(&saved);
        draft.chd_seconds = "ten".into();
        assert_eq!(
            draft.update(&saved),
            Err("Each chdman run's time limit: enter a whole number.".into())
        );
        let mut draft = Draft::of(&saved);
        draft.source_set_template = "{nope}".into();
        assert!(
            draft
                .update(&saved)
                .unwrap_err()
                .starts_with("The source set path template: ")
        );
    }

    #[test]
    fn romm_is_sent_whole_when_changed_but_its_token_only_when_replaced_or_cleared() {
        let saved = saved();
        let mut draft = Draft::of(&saved);
        draft.romm_enabled = true;
        draft.romm_base_url = " http://romm.example:8080/ ".into();
        assert_eq!(
            draft.update(&saved).unwrap().romm,
            Some(RommUpdate {
                enabled: true,
                base_url: Some("http://romm.example:8080".into()),
                token: SecretUpdate::Keep,
                root: Some("romm".into()),
            })
        );

        let mut draft = Draft::of(&saved);
        draft.romm_root = String::new();
        draft.romm_token = SecretUpdate::Set("rmm_new".into());
        let romm = draft.update(&saved).unwrap().romm.unwrap();
        assert_eq!(romm.root, None, "no root chosen");
        assert_eq!(romm.token, SecretUpdate::Set("rmm_new".into()));
    }

    #[test]
    fn a_romm_test_says_what_it_found_and_what_to_fix() {
        let check = RommCheck {
            version: Some("5.5.0".into()),
            version_tested: false,
            user: Some(RommUser {
                username: "svc".into(),
                admin: true,
            }),
            missing_scopes: vec!["tasks.run".into()],
            failure: None,
        };
        let lines = romm_check_lines(&check);
        assert_eq!(
            lines[0],
            (
                Tone::Review,
                "RomM 5.5.0, a release not yet tested: title-select is tested with RomM 5.4."
                    .into()
            )
        );
        assert_eq!(lines[1], (Tone::Review, "Signed in as svc, an admin: a user with only the scopes title-select needs is safer.".into()));
        assert_eq!(
            lines[2],
            (
                Tone::Failed,
                "Your token is missing the tasks.run scope.".into()
            )
        );

        let fine = RommCheck {
            version: Some("5.4.1".into()),
            version_tested: true,
            user: None,
            missing_scopes: vec![],
            failure: None,
        };
        assert_eq!(
            romm_check_lines(&fine),
            [
                (Tone::Good, "RomM 5.4.1, a tested release.".into()),
                (
                    Tone::Neutral,
                    "Add the me.read scope to your token to see which user it signs in as.".into()
                ),
                (
                    Tone::Good,
                    "Your token has every scope title-select needs.".into()
                ),
            ]
        );
        let failed = RommCheck {
            failure: Some("RomM refused the token.".into()),
            ..fine
        };
        assert_eq!(
            romm_check_lines(&failed).last(),
            Some(&(Tone::Failed, "RomM refused the token.".into()))
        );
    }

    #[test]
    fn each_mapping_names_its_platform_or_says_why_it_has_none() {
        let label = |p| romm_platform_label(&p);
        assert_eq!(
            label(RommPlatform::Mapped {
                fs_slug: "gba".into()
            }),
            ("gba".into(), None)
        );
        for none in [
            RommPlatform::NoRoot,
            RommPlatform::OutsideRoot,
            RommPlatform::Nested,
            RommPlatform::IsRoot,
            RommPlatform::Unresolved,
        ] {
            let (cell, why) = label(none);
            assert_eq!(cell, "No platform");
            assert!(why.is_some_and(|w| w.ends_with('.')));
        }
    }

    #[test]
    fn a_secret_says_whether_it_is_set_and_what_saving_does() {
        assert_eq!(secret_state(true, &SecretUpdate::Keep), "Set");
        assert_eq!(secret_state(false, &SecretUpdate::Keep), "Not set");
        assert_eq!(
            secret_state(true, &SecretUpdate::Set("s".into())),
            "Set; replaced when you save"
        );
        assert_eq!(
            secret_state(true, &SecretUpdate::Clear),
            "Set; cleared when you save"
        );
    }
}
