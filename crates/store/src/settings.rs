//! The Operator's operational settings, kept in the store and edited in the workbench (#139): each section one row
//! of the `settings` table, its value JSON, with its defaults while it has no row. A change applies without a
//! restart: each use reads the settings as they are then, and the pace, which running Jobs read at each file, is held
//! in memory too.
//!
//! To add a section, add its field to [`Settings`], to [`title_select_api::settings::SettingsView`] and
//! [`title_select_api::settings::SettingsUpdate`], and a name for its row in [`Store::update_settings`] and
//! [`load`]. A secret is kept here in plain text, and only [`Settings::view`] is ever sent: it says whether a secret
//! is set, never what it is.

use std::collections::BTreeMap;

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use title_select_api::settings::{
    ChdTimeouts, FetchSettings, IngestCadence, JobsView, NotifyEvent, NotifyUpdate, NotifyView,
    Pace, PathTemplates, Retention, SecretUpdate, SettingsUpdate, SettingsView, SourceSetChecks,
};

use crate::config::Config;
use crate::{Error, Result, Store, now};

/// The webhook as stored, secrets included.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notify {
    /// Where each event is posted. It may carry a token, so it is never returned or logged.
    pub url: Option<String>,
    /// The key each body is signed with.
    pub secret: Option<String>,
    pub events: Vec<NotifyEvent>,
    /// The workbench's base URL, such as `https://<domain>`, without a trailing `/`.
    pub workbench_url: Option<String>,
}

impl Default for Notify {
    fn default() -> Self {
        Notify {
            url: None,
            secret: None,
            events: NotifyEvent::ALL.to_vec(),
            workbench_url: None,
        }
    }
}

impl std::fmt::Debug for Notify {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Notify")
            .field("url_set", &self.url.is_some())
            .field("secret_set", &self.secret.is_some())
            .field("events", &self.events)
            .field("workbench_url", &self.workbench_url)
            .finish()
    }
}

impl Notify {
    /// Whether events are posted: a URL and its secret are both set.
    pub fn is_on(&self) -> bool {
        self.url.is_some() && self.secret.is_some()
    }

    pub fn view(&self) -> NotifyView {
        NotifyView {
            url_set: self.url.is_some(),
            secret_set: self.secret.is_some(),
            events: self.events.clone(),
            workbench_url: self.workbench_url.clone(),
        }
    }

    /// This webhook with `update` applied, checked.
    fn updated(&self, update: &NotifyUpdate) -> Result<Notify> {
        let url = match &update.url {
            SecretUpdate::Keep => self.url.clone(),
            SecretUpdate::Clear => None,
            SecretUpdate::Set(url) => {
                let url = url.trim();
                // The URL may carry a token, so the refusal never names it.
                if let Some(why) = crate::fetch::refusal_unnamed(url) {
                    return Err(Error::Refused(format!(
                        "The webhook URL is refused: {why}."
                    )));
                }
                Some(url.to_owned())
            }
        };
        let secret = match &update.secret {
            SecretUpdate::Keep => self.secret.clone(),
            SecretUpdate::Clear => None,
            SecretUpdate::Set(secret) => match secret.trim() {
                "" => return Err(Error::Refused("The webhook secret is empty.".into())),
                secret => Some(secret.to_owned()),
            },
        };
        if url.is_some() && secret.is_none() {
            return Err(Error::Refused(
                "The webhook needs a secret to sign each event with: set one with its URL.".into(),
            ));
        }
        let workbench_url = match update.workbench_url.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(base) if base.starts_with("http://") || base.starts_with("https://") => {
                Some(base.trim_end_matches('/').to_owned())
            }
            Some(base) => {
                return Err(Error::Refused(format!(
                    "The workbench URL {base:?} is not an http or https URL."
                )));
            }
        };
        let mut events = Vec::new();
        for event in NotifyEvent::ALL {
            if update.events.contains(&event) {
                events.push(event);
            }
        }
        Ok(Notify {
            url,
            secret,
            events,
            workbench_url,
        })
    }
}

/// Every operational setting, as the store holds it now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings {
    pub pace: Pace,
    pub retention: Retention,
    pub path_templates: PathTemplates,
    pub source_sets: SourceSetChecks,
    pub fetch: FetchSettings,
    pub chd: ChdTimeouts,
    /// The cadence set for each Ingest Folder, by its Root's name. One not here has the defaults.
    pub ingest_folders: BTreeMap<String, IngestCadence>,
    pub notify: Notify,
}

impl Settings {
    /// How the Ingest Folder of the Root `root` is watched.
    pub fn ingest(&self, root: &str) -> IngestCadence {
        self.ingest_folders.get(root).copied().unwrap_or_default()
    }

    /// How long chdman may run on a disc.
    pub fn chd_limits(&self) -> title_select_engine::chd::Limits {
        title_select_engine::chd::Limits {
            base: std::time::Duration::from_secs(self.chd.seconds),
            per_gib: std::time::Duration::from_secs(self.chd.seconds_per_gib),
        }
    }

    /// What the API returns: every setting but the secrets, with the configured ceilings and the cadence of every
    /// Ingest Folder `config` names.
    pub fn view(&self, config: &Config) -> SettingsView {
        let ceilings = config.jobs.ceilings();
        SettingsView {
            jobs: JobsView {
                pace: self.pace,
                ceilings,
                active: self.pace.counts(ceilings),
            },
            retention: self.retention,
            path_templates: self.path_templates.clone(),
            source_sets: self.source_sets,
            fetch: self.fetch,
            chd: self.chd,
            ingest_folders: config
                .roots
                .iter()
                .filter(|(_, root)| root.is_ingest())
                .map(|(name, _)| (name.clone(), self.ingest(name)))
                .collect(),
            notify: self.notify.view(),
        }
    }
}

const PACE: &str = "pace";
const RETENTION: &str = "retention";
const PATH_TEMPLATES: &str = "path_templates";
const SOURCE_SETS: &str = "source_sets";
const FETCH: &str = "fetch";
const CHD: &str = "chd";
const INGEST_FOLDERS: &str = "ingest_folders";
const NOTIFY: &str = "notify";

fn parse<T: serde::de::DeserializeOwned>(section: &str, value: &str) -> Result<T> {
    serde_json::from_str(value)
        .map_err(|e| Error::Io(format!("the stored {section} settings do not read: {e}")))
}

/// The settings as stored, with the defaults of each section that has no row.
pub(crate) fn load(c: &Connection) -> Result<Settings> {
    let mut settings = Settings::default();
    let rows = c
        .prepare("SELECT section, value FROM settings")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (section, value) in rows {
        match section.as_str() {
            PACE => settings.pace = parse(&section, &value)?,
            RETENTION => settings.retention = parse(&section, &value)?,
            PATH_TEMPLATES => settings.path_templates = parse(&section, &value)?,
            SOURCE_SETS => settings.source_sets = parse(&section, &value)?,
            FETCH => settings.fetch = parse(&section, &value)?,
            CHD => settings.chd = parse(&section, &value)?,
            INGEST_FOLDERS => settings.ingest_folders = parse(&section, &value)?,
            NOTIFY => settings.notify = parse(&section, &value)?,
            _ => {}
        }
    }
    Ok(settings)
}

fn put<T: Serialize>(
    tx: &rusqlite::Transaction,
    section: &str,
    value: &T,
    actor: &str,
) -> Result<()> {
    let value = serde_json::to_string(value).expect("settings serialize");
    tx.execute(
        "INSERT INTO settings (section, value, updated_by, updated_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (section) DO UPDATE SET value = ?2, updated_by = ?3, updated_at = ?4",
        params![section, value, actor, now()],
    )?;
    Ok(())
}

impl Store {
    /// Every operational setting as it is now.
    pub fn settings(&self) -> Result<Settings> {
        self.read(load)
    }

    /// The pace running Jobs follow now.
    pub fn pace(&self) -> Pace {
        *self.pace.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Replaces each section `update` gives, as `actor`, checking it against `config`, and returns the settings
    /// now. Nothing is changed unless every section given is accepted.
    pub fn update_settings(
        &self,
        config: &Config,
        update: &SettingsUpdate,
        actor: &str,
    ) -> Result<Settings> {
        if let Some(retention) = &update.retention
            && (retention.audits_keep == 0 || retention.selections_keep == 0)
        {
            return Err(Error::Refused(
                "Keep at least 1 audit and 1 selection of each source set.".into(),
            ));
        }
        if let Some(chd) = &update.chd
            && chd.seconds == 0
        {
            return Err(Error::Refused(
                "Give chdman at least 1 second for each disc.".into(),
            ));
        }
        if let Some(folders) = &update.ingest_folders {
            for (name, cadence) in folders {
                if !config.root(name).is_some_and(|r| r.is_ingest()) {
                    return Err(Error::Refused(format!(
                        "{name} is not an ingest folder in your configuration."
                    )));
                }
                if cadence.scan_every_minutes == Some(0) {
                    return Err(Error::Refused(format!(
                        "Scan {name} at most every minute, or turn its schedule off."
                    )));
                }
            }
        }
        let settings = self.write(|tx| {
            let mut settings = load(tx)?;
            if let Some(notify) = &update.notify {
                settings.notify = settings.notify.updated(notify)?;
                put(tx, NOTIFY, &settings.notify, actor)?;
            }
            if let Some(pace) = update.pace {
                settings.pace = pace;
                put(tx, PACE, &pace, actor)?;
            }
            if let Some(retention) = update.retention {
                settings.retention = retention;
                put(tx, RETENTION, &retention, actor)?;
            }
            if let Some(templates) = &update.path_templates {
                settings.path_templates = templates.clone();
                put(tx, PATH_TEMPLATES, templates, actor)?;
            }
            if let Some(checks) = update.source_sets {
                settings.source_sets = checks;
                put(tx, SOURCE_SETS, &checks, actor)?;
            }
            if let Some(fetch) = update.fetch {
                settings.fetch = fetch;
                put(tx, FETCH, &fetch, actor)?;
            }
            if let Some(chd) = update.chd {
                settings.chd = chd;
                put(tx, CHD, &chd, actor)?;
            }
            if let Some(folders) = &update.ingest_folders {
                settings.ingest_folders.extend(folders.clone());
                put(tx, INGEST_FOLDERS, &settings.ingest_folders, actor)?;
            }
            Ok(settings)
        })?;
        *self.pace.write().unwrap_or_else(|e| e.into_inner()) = settings.pace;
        Ok(settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        (dir, store)
    }

    fn ingest_config() -> Config {
        Config::parse(
            "[roots.in]\npath = \"/in\"\nwritable = true\nkind = \"ingest\"\n\n[roots.sets]\npath = \"/sets\"",
        )
        .unwrap()
    }

    #[test]
    fn each_section_given_is_kept_across_a_restart_and_the_rest_keep_their_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let config = ingest_config();
        {
            let store = Store::open(dir.path()).unwrap();
            assert_eq!(store.settings().unwrap(), Settings::default());
            store
                .update_settings(
                    &config,
                    &SettingsUpdate {
                        pace: Some(Pace::Gentle),
                        retention: Some(Retention {
                            audits_keep: 3,
                            selections_keep: 4,
                        }),
                        fetch: Some(FetchSettings {
                            redump: true,
                            mame: false,
                        }),
                        ingest_folders: Some(BTreeMap::from([(
                            "in".to_owned(),
                            IngestCadence {
                                settle_secs: 5,
                                scan_every_minutes: Some(15),
                            },
                        )])),
                        ..SettingsUpdate::default()
                    },
                    "Operator",
                )
                .unwrap();
            assert_eq!(store.pace(), Pace::Gentle);
        }
        let store = Store::open(dir.path()).unwrap();
        let settings = store.settings().unwrap();
        assert_eq!(store.pace(), Pace::Gentle);
        assert_eq!(settings.retention.audits_keep, 3);
        assert!(settings.fetch.redump && !settings.fetch.mame);
        assert_eq!(settings.ingest("in").settle_secs, 5);
        assert_eq!(settings.chd, ChdTimeouts::default());
        assert_eq!(settings.path_templates, PathTemplates::default());
        let view = settings.view(&config);
        assert_eq!(view.ingest_folders.keys().collect::<Vec<_>>(), ["in"]);
        assert_eq!(view.jobs.active, Pace::Gentle.counts(view.jobs.ceilings));
    }

    #[test]
    fn nonsense_is_refused_and_changes_nothing() {
        let (_dir, store) = store();
        let config = ingest_config();
        let cadence = |scan| {
            BTreeMap::from([(
                "in".to_owned(),
                IngestCadence {
                    settle_secs: 60,
                    scan_every_minutes: scan,
                },
            )])
        };
        for bad in [
            SettingsUpdate {
                pace: Some(Pace::Unthrottled),
                retention: Some(Retention {
                    audits_keep: 0,
                    selections_keep: 1,
                }),
                ..SettingsUpdate::default()
            },
            SettingsUpdate {
                pace: Some(Pace::Unthrottled),
                ingest_folders: Some(cadence(Some(0))),
                ..SettingsUpdate::default()
            },
            SettingsUpdate {
                ingest_folders: Some(BTreeMap::from([(
                    "sets".to_owned(),
                    IngestCadence::default(),
                )])),
                ..SettingsUpdate::default()
            },
            SettingsUpdate {
                chd: Some(ChdTimeouts {
                    seconds: 0,
                    seconds_per_gib: 1,
                }),
                ..SettingsUpdate::default()
            },
        ] {
            assert!(
                store.update_settings(&config, &bad, "Operator").is_err(),
                "{bad:?}"
            );
        }
        assert_eq!(store.settings().unwrap(), Settings::default());
        assert_eq!(store.pace(), Pace::Default);
    }

    fn notify(url: SecretUpdate, secret: SecretUpdate) -> SettingsUpdate {
        SettingsUpdate {
            notify: Some(NotifyUpdate {
                url,
                secret,
                events: vec![NotifyEvent::DriftFound, NotifyEvent::JobFailed],
                workbench_url: Some("https://ts.example/".into()),
            }),
            ..SettingsUpdate::default()
        }
    }

    #[test]
    fn the_webhook_needs_its_secret_and_keeps_both_unless_told_otherwise() {
        let (_dir, store) = store();
        let config = Config::default();
        let set = |s: &str| SecretUpdate::Set(s.to_owned());
        assert!(
            store
                .update_settings(
                    &config,
                    &notify(set("https://hooks.example/x"), SecretUpdate::Keep),
                    "Operator"
                )
                .is_err()
        );
        let on = store
            .update_settings(
                &config,
                &notify(set("https://hooks.example/x"), set(" s3cret \n")),
                "Operator",
            )
            .unwrap()
            .notify;
        assert_eq!(on.url.as_deref(), Some("https://hooks.example/x"));
        assert_eq!(on.secret.as_deref(), Some("s3cret"));
        assert_eq!(
            on.events,
            [NotifyEvent::JobFailed, NotifyEvent::DriftFound],
            "in their documented order"
        );
        assert_eq!(on.workbench_url.as_deref(), Some("https://ts.example"));
        let kept = store
            .update_settings(
                &config,
                &notify(SecretUpdate::Keep, SecretUpdate::Keep),
                "Operator",
            )
            .unwrap()
            .notify;
        assert_eq!(kept, on);
        assert!(
            store
                .update_settings(
                    &config,
                    &notify(SecretUpdate::Keep, SecretUpdate::Clear),
                    "Operator"
                )
                .is_err()
        );
        let off = store
            .update_settings(
                &config,
                &notify(SecretUpdate::Clear, SecretUpdate::Clear),
                "Operator",
            )
            .unwrap()
            .notify;
        assert!(!off.is_on() && off.url.is_none() && off.secret.is_none());
        let view = format!("{:?}", serde_json::to_string(&kept.view()).unwrap());
        assert!(!view.contains("hooks.example") && !view.contains("s3cret"));
        assert!(!format!("{kept:?}").contains("s3cret"));
    }

    #[test]
    fn a_refused_webhook_url_is_never_echoed() {
        let (_dir, store) = store();
        for url in [
            "ftp://hooks.example/secret-token",
            "https://user:secret-token@hooks.example/x",
            "not a url secret-token",
            "https://hooks.no-intro.org/secret-token",
        ] {
            let err = store
                .update_settings(
                    &Config::default(),
                    &notify(SecretUpdate::Set(url.into()), SecretUpdate::Set("s".into())),
                    "Operator",
                )
                .unwrap_err()
                .to_string();
            assert!(err.contains("The webhook URL is refused: it "), "{err}");
            assert!(!err.contains("secret-token"), "{err}");
            assert!(!err.contains("hooks."), "{err}");
        }
    }

    #[test]
    fn path_templates_are_checked_as_they_are_read() {
        for (template, reason) in [
            ("", "the template must name a folder inside its root"),
            ("../{dat}", "the template must not contain `..`"),
            ("/{dat}", "the template must be relative to its root"),
            ("{system}", "{system}"),
        ] {
            let body = format!(
                r#"{{"path_templates": {{"source_set": {template:?}, "library_folder": "{{platform}}"}}}}"#
            );
            let err = serde_json::from_str::<SettingsUpdate>(&body)
                .unwrap_err()
                .to_string();
            assert!(err.contains(reason), "{template}: {err}");
        }
    }
}
