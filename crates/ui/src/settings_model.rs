//! The Operations settings as the Operator edits them: a draft of every field, as typed, turned into the change
//! `PATCH /settings` takes. Only the sections that changed are sent, so a save never touches a section someone else
//! changed meanwhile, and the webhook's URL and secret are sent only when replaced or cleared.

use std::collections::BTreeMap;

use title_select_api::paths::PathTemplate;
use title_select_api::settings::{
    ChdTimeouts, FetchSettings, IngestCadence, NotifyEvent, NotifyUpdate, Pace, PathTemplates,
    Retention, SecretUpdate, SettingsUpdate, SettingsView, SourceSetChecks,
};

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
        Ok(SettingsUpdate {
            pace: (self.pace != saved.jobs.pace).then_some(self.pace),
            retention: (retention != saved.retention).then_some(retention),
            path_templates: (path_templates != saved.path_templates).then_some(path_templates),
            source_sets: (source_sets != saved.source_sets).then_some(source_sets),
            fetch: (self.fetch != saved.fetch).then_some(self.fetch),
            chd: (chd != saved.chd).then_some(chd),
            ingest_folders: (!ingest_folders.is_empty()).then_some(ingest_folders),
            notify: notify_changed.then_some(notify),
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
