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
    ArchiveLimits, ChdTimeouts, HostOverrides, MAX_DICTIONARY_MIB, FetchSettings, IngestCadence, JobsView, MAX_CHD_SECONDS,
    MAX_CHECK_EVERY_MINUTES, MAX_KEPT, MAX_SCAN_EVERY_MINUTES, MAX_SETTLE_SECS, NotifyEvent,
    NotifyUpdate, NotifyView, Pace, PathTemplates, Retention, SecretUpdate, SettingsUpdate,
    SettingsView, SourceSetChecks,
};

use crate::roots::Roots;
use crate::romm::Romm;
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
    pub romm: Romm,
    /// The Host's Ceilings you set. Each Root's are kept with it.
    pub ceilings: HostOverrides,
    pub archive: ArchiveLimits,
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

    /// What the API returns: every setting but the secrets, with the Ceilings in force, `jobs`, and the cadence of
    /// every Ingest Folder in `roots`.
    pub fn view(&self, roots: &Roots, jobs: JobsView) -> SettingsView {
        SettingsView {
            jobs,
            retention: self.retention,
            path_templates: self.path_templates.clone(),
            source_sets: self.source_sets,
            fetch: self.fetch,
            chd: self.chd,
            ingest_folders: roots
                .ingest()
                .map(|root| (root.name.clone(), self.ingest(&root.name)))
                .collect(),
            notify: self.notify.view(),
            romm: self.romm.view(),
            archive: self.archive,
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
const ROMM: &str = "romm";
const CEILINGS: &str = "ceilings";
const ARCHIVE: &str = "archive";

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
            ROMM => settings.romm = parse(&section, &value)?,
            CEILINGS => settings.ceilings = parse(&section, &value)?,
            ARCHIVE => settings.archive = parse(&section, &value)?,
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

    /// Every operational setting, as the API returns it, with the Ceilings in force, worked out afresh so that the
    /// write throughput past Jobs reached is up to date.
    pub fn settings_view(&self) -> Result<SettingsView> {
        let jobs = self.refresh_ceilings()?;
        Ok(self.settings()?.view(&self.roots(), jobs))
    }

    /// The pace running Jobs follow now.
    pub fn pace(&self) -> Pace {
        *self.pace.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Replaces each section `update` gives, as `actor`, checking it against the Roots, and returns the settings
    /// now. Nothing is changed unless every section given is accepted. The Ceilings of each Root `update` names are
    /// kept with it.
    pub fn update_settings(&self, update: &SettingsUpdate, actor: &str) -> Result<Settings> {
        self.update_settings_and_publish(update, actor, |_| {})
    }

    /// As [`Store::update_settings`], then `publish`es the settings now to whatever applies them, such as the
    /// webhook. Each update's commit and publication run together, one update at a time, so a slower update never
    /// publishes its settings after a later one's.
    pub fn update_settings_and_publish(
        &self,
        update: &SettingsUpdate,
        actor: &str,
        publish: impl FnOnce(&Settings),
    ) -> Result<Settings> {
        Ok(self.update_settings_and_release(update, actor, publish)?.0)
    }

    /// As [`Store::update_settings_and_publish`], and says whether the update left RomM's "Additions" collection to
    /// delete, having turned RomM off or changed its address: it is recorded, with the connection it was made through,
    /// for [`Store::delete_released_romm_collection`], which is never part of saving.
    pub fn update_settings_and_release(
        &self,
        update: &SettingsUpdate,
        actor: &str,
        publish: impl FnOnce(&Settings),
    ) -> Result<(Settings, bool)> {
        // The lifecycle lock keeps a Root from being registered, edited or unregistered meanwhile.
        let _lifecycle = update.ceilings.as_ref().map(|_| self.lifecycle_lock());
        let _one_at_a_time = self
            .settings_update
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let roots = self.roots();
        check(&roots, update)?;
        let ceilings = update
            .ceilings
            .as_ref()
            .map(|c| crate::ceilings::checked(&roots, c))
            .transpose()?;
        let (settings, release) = self.write(|tx| {
            let mut settings = load(tx)?;
            let was = settings.romm.clone();
            if let Some(notify) = &update.notify {
                settings.notify = settings.notify.updated(notify)?;
                put(tx, NOTIFY, &settings.notify, actor)?;
            }
            if let Some(romm) = &update.romm {
                settings.romm = settings.romm.updated(&roots, romm)?;
                put(tx, ROMM, &settings.romm, actor)?;
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
            if let Some(ceilings) = &ceilings {
                settings.ceilings = ceilings.host.clone();
                put(tx, CEILINGS, &ceilings.host, actor)?;
                Store::set_root_ceilings(tx, &ceilings.roots, actor)?;
            }
            if let Some(archive) = update.archive {
                settings.archive = archive;
                put(tx, ARCHIVE, &archive, actor)?;
            }
            let release = crate::romm_additions::release(tx, &was, &settings.romm)?;
            Ok((settings, release))
        })?;
        *self.pace.write().unwrap_or_else(|e| e.into_inner()) = settings.pace;
        self.max_dictionary_mib.store(
            settings.archive.max_dictionary_mib,
            std::sync::atomic::Ordering::Relaxed,
        );
        if ceilings.is_some() {
            self.reload_roots()?;
        }
        if update.pace.is_some() || ceilings.is_some() {
            // Running Jobs take the new pools' sizes at their workers' next file.
            self.refresh_ceilings()?;
        }
        publish(&settings);
        Ok((settings, release))
    }
}

/// Refuses an update whose values are out of range, before anything is changed.
fn check(roots: &Roots, update: &SettingsUpdate) -> Result<()> {
    let refuse = |why: String| Err(Error::Refused(why));
    if let Some(retention) = &update.retention {
        for (what, keep) in [
            ("audits", retention.audits_keep),
            ("selections", retention.selections_keep),
        ] {
            if !(1..=MAX_KEPT).contains(&keep) {
                return refuse(format!(
                    "Keep from 1 to {MAX_KEPT} {what} of each source set."
                ));
            }
        }
    }
    if let Some(checks) = &update.source_sets
        && checks.check_every_minutes > MAX_CHECK_EVERY_MINUTES
    {
        return refuse(format!(
            "Check source sets for changes at least every {MAX_CHECK_EVERY_MINUTES} minutes (a year), or set 0 to \
             check only when you ask."
        ));
    }
    if let Some(chd) = &update.chd {
        if !(1..=MAX_CHD_SECONDS).contains(&chd.seconds) {
            return refuse(format!(
                "Give chdman from 1 to {MAX_CHD_SECONDS} seconds (a week) for each disc."
            ));
        }
        if chd.seconds_per_gib > MAX_CHD_SECONDS {
            return refuse(format!(
                "Add at most {MAX_CHD_SECONDS} seconds (a week) for each GiB of a disc."
            ));
        }
    }
    if let Some(archive) = &update.archive
        && !(1..=MAX_DICTIONARY_MIB).contains(&archive.max_dictionary_mib)
    {
        return refuse(format!(
            "Set the largest 7z dictionary to a whole number of MiB from 1 to {MAX_DICTIONARY_MIB}."
        ));
    }
    if let Some(folders) = &update.ingest_folders {
        for (name, cadence) in folders {
            if !roots.root(name).is_some_and(|r| r.is_ingest()) {
                return Err(Error::Refused(format!(
                    "{name} is not a registered ingest folder."
                )));
            }
            if cadence.settle_secs > MAX_SETTLE_SECS {
                return Err(Error::Refused(format!(
                    "Let items in {name} settle for at most {MAX_SETTLE_SECS} seconds (a day)."
                )));
            }
            if cadence
                .scan_every_minutes
                .is_some_and(|m| !(1..=MAX_SCAN_EVERY_MINUTES).contains(&m))
            {
                return Err(Error::Refused(format!(
                    "Scan {name} every 1 to {MAX_SCAN_EVERY_MINUTES} minutes (a week), or leave its \
                         schedule empty to scan only when you ask."
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use title_select_api::romm::{RommUpdate, RommView};

    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        (dir, store)
    }

    /// Registers each Root `specs` gives, in its own new folder under `dir`: a name, and whether it is an ingest
    /// folder or else writable.
    fn register(store: &Store, dir: &std::path::Path, specs: &[(&str, Option<bool>)]) {
        use title_select_api::roots::{RootKind, RootSpec};
        for (name, how) in specs {
            let path = dir.join("roots").join(name);
            std::fs::create_dir_all(&path).unwrap();
            let spec = match how {
                Some(true) => RootSpec::new(name, &path).of_kind(RootKind::Ingest),
                Some(false) => RootSpec::new(name, &path).writable(),
                None => RootSpec::new(name, &path),
            };
            store.register_root(&spec, "test").unwrap();
        }
    }

    const INGEST: &[(&str, Option<bool>)] = &[("in", Some(true)), ("sets", None)];
    const ROMM: &[(&str, Option<bool>)] = &[("library", Some(false)), ("in", Some(true))];

    #[test]
    fn each_section_given_is_kept_across_a_restart_and_the_rest_keep_their_defaults() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = Store::open(dir.path()).unwrap();
            assert_eq!(store.settings().unwrap(), Settings::default());
            register(&store, dir.path(), INGEST);
            store
                .update_settings(&SettingsUpdate {
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
        let view = store.settings_view().unwrap();
        assert_eq!(view.ingest_folders.keys().collect::<Vec<_>>(), ["in"]);
        let hashers = &view.jobs.host.hashers;
        assert_eq!(view.jobs.pace, Pace::Gentle);
        assert_eq!(
            hashers.in_force,
            Some(Pace::Gentle.of(hashers.ceiling.unwrap() as usize) as u64)
        );
    }

    #[test]
    fn nonsense_is_refused_and_changes_nothing() {
        let (dir, store) = store();
        register(&store, dir.path(), INGEST);
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
            SettingsUpdate {
                ingest_folders: Some(cadence(Some(MAX_SCAN_EVERY_MINUTES + 1))),
                ..SettingsUpdate::default()
            },
            SettingsUpdate {
                ingest_folders: Some(cadence(Some(u64::MAX / 4))),
                ..SettingsUpdate::default()
            },
            SettingsUpdate {
                ingest_folders: Some(BTreeMap::from([(
                    "in".to_owned(),
                    IngestCadence {
                        settle_secs: u64::MAX,
                        scan_every_minutes: None,
                    },
                )])),
                ..SettingsUpdate::default()
            },
            SettingsUpdate {
                source_sets: Some(SourceSetChecks {
                    check_every_minutes: u64::MAX,
                }),
                ..SettingsUpdate::default()
            },
            SettingsUpdate {
                retention: Some(Retention {
                    audits_keep: usize::MAX,
                    selections_keep: 1,
                }),
                ..SettingsUpdate::default()
            },
            SettingsUpdate {
                chd: Some(ChdTimeouts {
                    seconds: 1,
                    seconds_per_gib: u64::MAX,
                }),
                ..SettingsUpdate::default()
            },
            SettingsUpdate {
                archive: Some(ArchiveLimits {
                    max_dictionary_mib: 0,
                }),
                ..SettingsUpdate::default()
            },
            SettingsUpdate {
                archive: Some(ArchiveLimits {
                    max_dictionary_mib: MAX_DICTIONARY_MIB + 1,
                }),
                ..SettingsUpdate::default()
            },
        ] {
            let refused = store.update_settings(&bad, "Operator");
            assert!(
                matches!(refused, Err(Error::Refused(_))),
                "{bad:?}: {refused:?}"
            );
        }
        let longest = BTreeMap::from([(
            "in".to_owned(),
            IngestCadence {
                settle_secs: MAX_SETTLE_SECS,
                scan_every_minutes: Some(MAX_SCAN_EVERY_MINUTES),
            },
        )]);
        let (dir2, other) = self::store();
        register(&other, dir2.path(), INGEST);
        other
            .update_settings(&SettingsUpdate {
                    ingest_folders: Some(longest),
                    ..SettingsUpdate::default()
                },
                "Operator",
            )
            .unwrap();
        assert_eq!(store.settings().unwrap(), Settings::default());
        assert_eq!(store.pace(), Pace::Default);
    }

    #[test]
    fn concurrent_updates_publish_in_the_order_they_commit() {
        let (_dir, store) = store();
        let published = std::sync::Mutex::new(None::<bool>);
        let set = |s: &str| SecretUpdate::Set(s.to_owned());
        std::thread::scope(|scope| {
            for i in 0..8 {
                let (store, published) = (&store, &published);
                scope.spawn(move || {
                    for j in 0..25 {
                        let update = if (i + j) % 2 == 0 {
                            notify(set("https://hooks.example/x"), set("s"))
                        } else {
                            notify(SecretUpdate::Clear, SecretUpdate::Clear)
                        };
                        store
                            .update_settings_and_publish(&update, "Operator", |now| {
                                // Widens the window between commit and publication.
                                std::thread::sleep(std::time::Duration::from_micros(200));
                                assert_eq!(
                                    store.settings().unwrap().notify,
                                    now.notify,
                                    "nothing commits while a publication is under way"
                                );
                                *published.lock().unwrap() = Some(now.notify.is_on());
                            })
                            .unwrap();
                    }
                });
            }
        });
        assert_eq!(
            *published.lock().unwrap(),
            Some(store.settings().unwrap().notify.is_on()),
            "the last publication is the settings as stored"
        );
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
        let set = |s: &str| SecretUpdate::Set(s.to_owned());
        assert!(
            store
                .update_settings(&notify(set("https://hooks.example/x"), SecretUpdate::Keep),
                    "Operator"
                )
                .is_err()
        );
        let on = store
            .update_settings(&notify(set("https://hooks.example/x"), set(" s3cret \n")),
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
            .update_settings(&notify(SecretUpdate::Keep, SecretUpdate::Keep),
                "Operator",
            )
            .unwrap()
            .notify;
        assert_eq!(kept, on);
        assert!(
            store
                .update_settings(&notify(SecretUpdate::Keep, SecretUpdate::Clear),
                    "Operator"
                )
                .is_err()
        );
        let off = store
            .update_settings(&notify(SecretUpdate::Clear, SecretUpdate::Clear),
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

    fn romm(
        enabled: bool,
        base_url: Option<&str>,
        token: SecretUpdate,
        root: Option<&str>,
    ) -> SettingsUpdate {
        SettingsUpdate {
            romm: Some(RommUpdate {
                enabled,
                base_url: base_url.map(str::to_owned),
                token,
                root: root.map(str::to_owned),
            }),
            ..SettingsUpdate::default()
        }
    }

    #[test]
    fn romm_is_off_by_default_and_its_token_is_set_replaced_and_cleared_but_never_shown() {
        let (dir, store) = store();
        register(&store, dir.path(), ROMM);
        let set = |s: &str| SecretUpdate::Set(s.to_owned());
        assert_eq!(
            store.settings_view().unwrap().romm,
            RommView::default()
        );

        let on = store
            .update_settings(&romm(
                    true,
                    Some(" https://romm.example/ "),
                    set(" rmm_first-token \n"),
                    Some("library"),
                ),
                "Operator",
            )
            .unwrap();
        assert_eq!(on.romm.token.as_deref(), Some("rmm_first-token"));
        assert_eq!(
            on.view(&store.roots(), store.ceilings().unwrap()).romm,
            RommView {
                enabled: true,
                base_url: Some("https://romm.example".into()),
                token_set: true,
                root: Some("library".into()),
            }
        );

        let kept = store
            .update_settings(&romm(
                    false,
                    Some("https://romm.example"),
                    SecretUpdate::Keep,
                    Some("library"),
                ),
                "Operator",
            )
            .unwrap();
        assert_eq!(kept.romm.token.as_deref(), Some("rmm_first-token"));
        assert!(!kept.romm.enabled, "turning RomM off keeps its connection");

        let replaced = store
            .update_settings(&romm(
                    true,
                    Some("https://romm.example"),
                    set("rmm_second-token"),
                    Some("library"),
                ),
                "Operator",
            )
            .unwrap();
        assert_eq!(replaced.romm.token.as_deref(), Some("rmm_second-token"));
        let shown =
            serde_json::to_string(&replaced.view(&store.roots(), store.ceilings().unwrap()))
                .unwrap();
        assert!(!shown.contains("rmm_"), "{shown}");
        assert!(!format!("{replaced:?}").contains("rmm_"));

        let cleared = store
            .update_settings(&romm(
                    false,
                    Some("https://romm.example"),
                    SecretUpdate::Clear,
                    None,
                ),
                "Operator",
            )
            .unwrap();
        assert_eq!(cleared.romm.token, None);
        assert!(
            !cleared
                .view(&store.roots(), store.ceilings().unwrap())
                .romm
                .token_set
        );
        assert_eq!(store.settings().unwrap().romm, cleared.romm);
    }

    #[test]
    fn an_incomplete_or_unsafe_romm_connection_is_refused_and_changes_nothing() {
        let (dir, store) = store();
        register(&store, dir.path(), ROMM);
        let set = |s: &str| SecretUpdate::Set(s.to_owned());
        let url = Some("https://romm.example");
        for (bad, why) in [
            (romm(true, None, set("rmm_t"), Some("library")), "address"),
            (
                romm(true, url, SecretUpdate::Keep, Some("library")),
                "token",
            ),
            (romm(true, url, set("rmm_t"), None), "root"),
            (romm(false, url, set("  "), None), "empty"),
            (romm(false, url, set("rmm_t"), Some("nowhere")), "nowhere"),
            (romm(false, url, set("rmm_t"), Some("in")), "ingest"),
            (
                romm(false, Some("ftp://romm.example"), set("rmm_t"), None),
                "http",
            ),
            (
                romm(false, Some("https://u:p@romm.example"), set("rmm_t"), None),
                "credentials",
            ),
            (
                romm(false, Some("https://romm.example/?q=1"), set("rmm_t"), None),
                "query",
            ),
        ] {
            let refused = store.update_settings(&bad, "Operator");
            match refused {
                Err(Error::Refused(message)) => assert!(message.contains(why), "{why}: {message}"),
                other => panic!("{bad:?}: {other:?}"),
            }
        }
        assert_eq!(store.settings().unwrap(), Settings::default());

        store
            .update_settings(&romm(true, url, set("rmm_t"), Some("library")),
                "Operator",
            )
            .unwrap();
        for (bad, why) in [
            (
                romm(
                    true,
                    Some("https://elsewhere.example"),
                    SecretUpdate::Keep,
                    Some("library"),
                ),
                "token again",
            ),
            (
                romm(true, url, SecretUpdate::Clear, Some("library")),
                "token",
            ),
        ] {
            let refused = store.update_settings(&bad, "Operator");
            match refused {
                Err(Error::Refused(message)) => assert!(message.contains(why), "{why}: {message}"),
                other => panic!("{bad:?}: {other:?}"),
            }
        }
        let moved = store
            .update_settings(&romm(
                    true,
                    Some("https://elsewhere.example"),
                    set("rmm_u"),
                    Some("library"),
                ),
                "Operator",
            )
            .unwrap();
        assert_eq!(
            moved.romm.base_url.as_deref(),
            Some("https://elsewhere.example")
        );
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
