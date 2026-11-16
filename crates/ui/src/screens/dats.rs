use std::cmp::Ordering;
use std::collections::BTreeMap;

use leptos::html::Form;
use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use leptos_router::components::A;
use title_select_api::{
    AuditSummary, CuePackImport, CuePackRecord, Dat, DatRevisionRecord, ImportOutcome,
    ImportedDatRevision, SourceSetRecord,
};

use crate::api;
use crate::app::{Loading, Problem};
use crate::events::use_live;
use crate::model::{imported_message, revision_named, when};
use crate::parts::icon::Glyph;
use crate::parts::legend::{Legend, present};
use crate::parts::list::{
    Clip, Column, List, ListRow, RowAction, ToolAction, Width, last_good_rows,
};
use crate::parts::overlay::Drawer;
use crate::parts::status::{DatFileStatus, StatusIcon};

/// An Audit that refers to a DAT Revision.
#[derive(Debug, Clone, PartialEq, Eq)]
struct UsedBy {
    audit: i64,
    source_set: String,
    pruned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RevisionItem {
    revision: DatRevisionRecord,
    used_by: Vec<UsedBy>,
    /// The cue packs imported for its DAT.
    cue_packs: Vec<CuePackRecord>,
    /// The Source Sets bound to its DAT, by name.
    source_sets: Vec<String>,
}

impl RevisionItem {
    fn file_status(&self) -> DatFileStatus {
        if self.revision.blob_present {
            DatFileStatus::Kept
        } else {
            DatFileStatus::Deleted
        }
    }

    /// The Audits that refer to it, newest first.
    fn audits_newest_first(&self) -> Vec<&UsedBy> {
        let mut audits: Vec<&UsedBy> = self.used_by.iter().collect();
        audits.sort_by_key(|a| std::cmp::Reverse(a.audit));
        audits
    }
}

const NOT_GIVEN: &str = "Not given";

/// Its DAT Provider, or "Unknown": never its author or format.
fn provider_label(r: &DatRevisionRecord) -> &str {
    r.provider.as_deref().unwrap_or("Unknown")
}

/// A header claim the DAT may leave empty.
fn given(text: &str) -> &str {
    if text.is_empty() { NOT_GIVEN } else { text }
}

/// A header claim the DAT may leave out.
fn given_or(text: Option<&str>) -> &str {
    given(text.unwrap_or_default())
}

/// The names of the Source Sets, given as `(name, dat)`, bound to `dat`.
fn using<'a>(dat: &str, sets: impl IntoIterator<Item = (&'a str, &'a str)>) -> Vec<String> {
    sets.into_iter()
        .filter(|&(_, d)| d == dat)
        .map(|(name, _)| name.to_string())
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RevisionCol {
    File,
    Name,
    Provider,
    Version,
    Date,
    SourceSets,
}

impl ListRow for RevisionItem {
    type Col = RevisionCol;
    const COLUMNS: &'static [RevisionCol] = &[
        RevisionCol::File,
        RevisionCol::Name,
        RevisionCol::Provider,
        RevisionCol::Version,
        RevisionCol::Date,
        RevisionCol::SourceSets,
    ];

    fn column(col: RevisionCol) -> Column {
        match col {
            RevisionCol::File => Column::new("file", "File", Width::Fixed(3.5))
                .sortable()
                .hint("Whether the store still keeps its DAT file"),
            RevisionCol::Name => Column::new(
                "name",
                "DAT",
                Width::Flex {
                    min: 12.0,
                    share: 3,
                },
            )
            .sortable()
            .hint("The name the DAT gives itself"),
            RevisionCol::Provider => {
                Column::new("provider", "Provider", Width::Flex { min: 7.0, share: 1 })
                    .sortable()
                    .hint("Who publishes the DAT, when its header shows it")
            }
            RevisionCol::Version => {
                Column::new("version", "Version", Width::Flex { min: 7.0, share: 1 }).sortable()
            }
            RevisionCol::Date => Column::new("date", "Date", Width::Flex { min: 6.5, share: 1 })
                .sortable()
                .hint("The date its header gives"),
            RevisionCol::SourceSets => Column::new("source-sets", "Source sets", Width::Fixed(6.5))
                .sortable()
                .numeric()
                .hint("How many source sets use its DAT"),
        }
    }

    fn key(&self) -> String {
        self.revision.sha256.clone()
    }

    fn title(&self) -> String {
        let r = &self.revision;
        if r.version.is_empty() {
            r.name.clone()
        } else {
            format!("{} {}", r.name, r.version)
        }
    }

    fn search(&self) -> String {
        let r = &self.revision;
        format!("{} {} {} {}", r.name, provider_label(r), r.version, r.file)
    }

    fn cell(&self, col: RevisionCol) -> AnyView {
        let r = &self.revision;
        match col {
            RevisionCol::File => {
                view! { <StatusIcon status=self.file_status() tab_stop=false /> }.into_any()
            }
            RevisionCol::Name => view! { <Clip text=r.name.clone() /> }.into_any(),
            RevisionCol::Provider => {
                view! { <Clip text=provider_label(r).to_string() /> }.into_any()
            }
            RevisionCol::Version => {
                view! { <Clip text=given(&r.version).to_string() /> }.into_any()
            }
            RevisionCol::Date => {
                view! { <Clip text=given_or(r.date.as_deref()).to_string() /> }.into_any()
            }
            RevisionCol::SourceSets => self.source_sets.len().to_string().into_any(),
        }
    }

    fn compare(&self, other: &Self, col: RevisionCol) -> Ordering {
        let (a, b) = (&self.revision, &other.revision);
        match col {
            RevisionCol::File => a.blob_present.cmp(&b.blob_present),
            RevisionCol::Name => a.name.cmp(&b.name),
            RevisionCol::Provider => a.provider.cmp(&b.provider),
            RevisionCol::Version => a.version.cmp(&b.version),
            RevisionCol::Date => a.date.cmp(&b.date),
            RevisionCol::SourceSets => self.source_sets.len().cmp(&other.source_sets.len()),
        }
    }
}

/// Every DAT Revision, with the Source Sets bound to its DAT and the Audits of every Source Set's history that refer
/// to it.
async fn revisions() -> Result<Vec<RevisionItem>, String> {
    let dats: Vec<Dat> = api::get("/dats").await?;
    let sets: Vec<SourceSetRecord> = api::get("/source-sets").await?;
    let packs: Vec<CuePackRecord> = api::get("/cue-packs").await?;
    let mut used_by: BTreeMap<String, Vec<UsedBy>> = BTreeMap::new();
    for set in &sets {
        let history: Vec<AuditSummary> =
            api::get(&format!("/source-sets/{}/history", api::segment(&set.name))).await?;
        for audit in history {
            used_by
                .entry(audit.dat_revision_sha256)
                .or_default()
                .push(UsedBy {
                    audit: audit.id,
                    source_set: audit.source_set,
                    pruned: audit.pruned,
                });
        }
    }
    Ok(dats
        .into_iter()
        .flat_map(|dat| dat.revisions.into_iter().rev())
        .map(|revision| RevisionItem {
            used_by: used_by.remove(&revision.sha256).unwrap_or_default(),
            cue_packs: packs
                .iter()
                .filter(|p| p.dat == revision.dat)
                .cloned()
                .collect(),
            source_sets: using(
                &revision.dat,
                sets.iter().map(|s| (s.name.as_str(), s.dat.as_str())),
            ),
            revision,
        })
        .collect())
}

/// The actions on one DAT Revision: open its newest Audit, then its other Audits.
fn revision_actions(item: &RevisionItem) -> Vec<RowAction> {
    let audits = item.audits_newest_first();
    let mut actions = Vec::new();
    if let Some(latest) = audits.first() {
        actions.push(RowAction::go(
            Glyph::ClipboardList,
            format!("Open its latest audit, #{}", latest.audit),
            format!("/audits/{}", latest.audit),
        ));
    }
    actions.push(download_action(&item.revision));
    actions.extend(audits.iter().skip(1).map(|u| {
        RowAction::go(
            Glyph::ClipboardList,
            format!("Open audit #{} of {}", u.audit, u.source_set),
            format!("/audits/{}", u.audit),
        )
    }));
    actions
}

/// Downloads the DAT Revision's kept file, or says it can't because the file is no longer kept.
fn download_action(r: &DatRevisionRecord) -> RowAction {
    if r.blob_present {
        RowAction::download(
            Glyph::Download,
            "Download DAT",
            api::url(&format!("/dat-revisions/{}/file", r.sha256)),
        )
    } else {
        RowAction::unavailable(Glyph::Download, "Can't download DAT: its file was deleted")
    }
}

/// Every DAT and its DAT Revisions, to name a DAT Revision by.
pub fn dats_resource() -> LocalResource<Result<Vec<Dat>, String>> {
    LocalResource::new(|| api::get::<Vec<Dat>>("/dats"))
}

/// The text naming the DAT Revision `sha256` once `dats` loads: its DAT and version, never its SHA-256. `version` is
/// what a record gave with it, shown until then or if it can't be found.
pub fn revision_text(
    dats: LocalResource<Result<Vec<Dat>, String>>,
    sha256: String,
    version: Option<String>,
) -> impl Fn() -> String + Send + Sync + 'static {
    move || match dats.get() {
        Some(Ok(dats)) => revision_named(Some(&dats), &sha256, version.as_deref()),
        Some(Err(_)) if version.as_deref().is_none_or(str::is_empty) => {
            "a DAT revision".to_string()
        }
        _ => revision_named(None, &sha256, version.as_deref()),
    }
}

/// A DAT Revision, named by its DAT and version.
#[component]
pub fn RevisionName(
    #[prop(into)] sha256: String,
    #[prop(optional, into)] version: Option<String>,
) -> impl IntoView {
    revision_text(dats_resource(), sha256, version)
}

/// The DATs and their DAT Revisions, with their provenance, and the form to import another.
#[component]
pub fn DatRevisions() -> impl IntoView {
    let live = use_live();
    let uploaded = RwSignal::new(0u32);
    let items = LocalResource::new(move || {
        live.settled.track();
        uploaded.track();
        revisions()
    });
    let rows = last_good_rows(move || items.get());
    let statuses =
        Signal::derive(move || present(rows.get().iter().map(RevisionItem::file_status)));
    let importing = RwSignal::new(false);
    let importing_cues = RwSignal::new(false);
    let actions = vec![
        ToolAction::page(Glyph::Upload, "Import", move || importing.set(true)),
        ToolAction::page(Glyph::Upload, "Import cue pack", move || {
            importing_cues.set(true)
        }),
    ];

    view! {
        <h1>"DAT revisions"</h1>
        <p class="lede">
            "Each import of a DAT file is a DAT revision, shown as its header describes it. Newest import first within \
             each DAT. When the header doesn't show who publishes the DAT, its provider is unknown. \
             Import a cue pack, a ZIP of a disc system's cue sheets, and a repair can fill a missing cue sheet, or \
             replace one that doesn't match, from it."
        </p>
        {move || match items.get() {
            None => view! { <Loading what="the DAT revisions" /> }.into_any(),
            Some(Err(message)) => view! { <Problem message /> }.into_any(),
            Some(Ok(_)) => ().into_any(),
        }}
        <List
            id="dat-revisions"
            label="DAT revisions"
            rows
            empty="No DAT revisions yet. Choose Import to add a DAT."
            placeholder="Filter by DAT, provider, version or file"
            actions
            row_actions=Callback::new(|item: RevisionItem| revision_actions(&item))
            detail=Callback::new(revision_detail)
            legend=move || view! { <Legend statuses list="dat-revisions" /> }
        />
        <super::fetching::Fetching />
        <Drawer open=importing title="Import a DAT">
            <Upload on_done=Callback::new(move |_| uploaded.update(|n| *n += 1)) />
        </Drawer>
        <Drawer open=importing_cues title="Import a cue pack">
            <CuePackUpload on_done=Callback::new(move |_| uploaded.update(|n| *n += 1)) />
        </Drawer>
    }
}

#[component]
fn CuePackUpload(on_done: Callback<()>) -> impl IntoView {
    let form = NodeRef::<Form>::new();
    let outcome = RwSignal::new(None::<Result<CuePackImport, String>>);
    let busy = RwSignal::new(false);
    let dats = dats_resource();
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let Some(el) = form.get_untracked() else {
            return;
        };
        let Ok(data) = web_sys::FormData::new_with_form(&el) else {
            return;
        };
        busy.set(true);
        leptos::task::spawn_local(async move {
            let result = api::post_form::<CuePackImport>("/cue-packs", data).await;
            busy.set(false);
            if result.is_ok() {
                el.reset();
                on_done.run(());
            }
            outcome.set(Some(result));
        });
    };
    view! {
        <section aria-label="Import a cue pack">
            <form class="form" node_ref=form on:submit=submit>
                <label>
                    "DAT"
                    <select name="dat" required>
                        {move || {
                            dats.get()
                                .and_then(Result::ok)
                                .unwrap_or_default()
                                .into_iter()
                                .map(|d| view! { <option value=d.dat.clone()>{d.dat.clone()}</option> })
                                .collect_view()
                        }}
                    </select>
                </label>
                <label>
                    "Source URL"
                    <input type="url" name="source_url" placeholder="Where the cue pack came from" />
                </label>
                <label>
                    "Cue pack"
                    <input type="file" name="file" required accept=".zip" />
                </label>
                <button type="submit" class="primary" disabled=busy>
                    {move || if busy.get() { "Importing…" } else { "Import" }}
                </button>
            </form>
            {move || {
                outcome
                    .get()
                    .map(|outcome| match outcome {
                        Ok(imported) => {
                            let p = imported.pack;
                            let what = match imported.outcome {
                                ImportOutcome::Added => "Added cue pack",
                                ImportOutcome::AlreadyImported => "Already imported: cue pack",
                                ImportOutcome::Restored => "Restored the kept file of cue pack",
                            };
                            let sheets = match p.cues {
                                1 => "1 cue sheet".to_string(),
                                n => format!("{n} cue sheets"),
                            };
                            view! {
                                <p class="done" role="status">
                                    {format!("{what} {} for {} ({sheets}).", p.file, p.dat)}
                                </p>
                            }
                                .into_any()
                        }
                        Err(message) => view! { <Problem message /> }.into_any(),
                    })
            }}
        </section>
    }
}

fn revision_detail(item: RevisionItem) -> AnyView {
    let r = item.revision;
    let claim = |text: Option<String>| match text {
        Some(text) if !text.is_empty() => text.into_any(),
        _ => NOT_GIVEN.into_any(),
    };
    let link = |text: Option<String>| match text {
        Some(text) if !text.is_empty() => view! { <code class="path">{text}</code> }.into_any(),
        _ => NOT_GIVEN.into_any(),
    };
    view! {
        <dl class="facts">
            <dt>"DAT"</dt>
            <dd>{r.name.clone()}</dd>
            <dt>"Description"</dt>
            <dd>{claim(r.description.clone())}</dd>
            <dt>"Provider"</dt>
            <dd>{provider_label(&r).to_string()}</dd>
            <dt>"Author"</dt>
            <dd>{claim(r.author.clone())}</dd>
            <dt>"Version"</dt>
            <dd>{given(&r.version).to_string()}</dd>
            <dt>"Date"</dt>
            <dd>{given_or(r.date.as_deref()).to_string()}</dd>
            <dt>"Homepage"</dt>
            <dd>{claim(r.homepage.clone())}</dd>
            <dt>"URL in the DAT"</dt>
            <dd>{link(r.header_url.clone())}</dd>
            <dt>"Format"</dt>
            <dd>{r.format.label()}</dd>
            <dt>"File"</dt>
            <dd>
                <code class="path">{r.file.clone()}</code>
            </dd>
            <dt>"Source URL"</dt>
            <dd>
                {match r.source_url.clone() {
                    Some(url) => view! { <code class="path">{url}</code> }.into_any(),
                    None => "Not recorded".into_any(),
                }}
            </dd>
            <dt>"Imported"</dt>
            <dd>{when(&r.imported_at)}</dd>
            <dt>"File kept"</dt>
            <dd>
                {if r.blob_present {
                    "Kept in the store"
                } else {
                    "Deleted from the store; the DAT revision is still recorded, and importing the same file again restores it"
                }}
            </dd>
            <dt>"Source sets"</dt>
            <dd>
                {if item.source_sets.is_empty() {
                    view! { "No source set uses this DAT" }.into_any()
                } else {
                    view! {
                        <ul>
                            {item
                                .source_sets
                                .into_iter()
                                .map(|name| {
                                    let href = format!("/source-sets/{}", api::segment(&name));
                                    view! {
                                        <li>
                                            <A href=href>{name}</A>
                                        </li>
                                    }
                                })
                                .collect_view()}
                        </ul>
                    }
                        .into_any()
                }}
            </dd>
            <dt>"Audits"</dt>
            <dd>
                {if item.used_by.is_empty() {
                    view! { "No audit refers to it" }.into_any()
                } else {
                    view! {
                        <ul>
                            {item
                                .used_by
                                .into_iter()
                                .map(|u| {
                                    view! {
                                        <li>
                                            <A href=format!("/audits/{}", u.audit)>{format!("Audit #{}", u.audit)}</A>
                                            {format!(
                                                " of {}{}",
                                                u.source_set,
                                                if u.pruned { ", pruned" } else { "" },
                                            )}
                                        </li>
                                    }
                                })
                                .collect_view()}
                        </ul>
                    }
                        .into_any()
                }}
            </dd>
            <dt>"Cue packs"</dt>
            <dd>
                {if item.cue_packs.is_empty() {
                    view! { "None imported for this DAT" }.into_any()
                } else {
                    view! {
                        <ul>
                            {item
                                .cue_packs
                                .into_iter()
                                .map(|p| {
                                    let sheets = match p.cues {
                                        1 => "1 cue sheet".to_string(),
                                        n => format!("{n} cue sheets"),
                                    };
                                    let kept = if p.blob_present {
                                        ""
                                    } else {
                                        "; its file was deleted since a later pack replaced it, so import it again to use it"
                                    };
                                    view! {
                                        <li>
                                            <code class="path">{p.file}</code>
                                            {format!(", {sheets}, imported {}{kept}", when(&p.imported_at))}
                                        </li>
                                    }
                                })
                                .collect_view()}
                        </ul>
                    }
                        .into_any()
                }}
            </dd>
        </dl>
    }
    .into_any()
}

#[component]
fn Upload(on_done: Callback<()>) -> impl IntoView {
    let form = NodeRef::<Form>::new();
    let outcome = RwSignal::new(None::<Result<ImportedDatRevision, String>>);
    let busy = RwSignal::new(false);
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let Some(el) = form.get_untracked() else {
            return;
        };
        let Ok(data) = web_sys::FormData::new_with_form(&el) else {
            return;
        };
        busy.set(true);
        leptos::task::spawn_local(async move {
            let result = api::post_form::<ImportedDatRevision>("/dat-revisions", data).await;
            busy.set(false);
            if result.is_ok() {
                el.reset();
                on_done.run(());
            }
            outcome.set(Some(result));
        });
    };
    view! {
        <section aria-label="Import a DAT">
            <form class="form" node_ref=form on:submit=submit>
                <label>
                    "Source URL"
                    <input type="url" name="source_url" placeholder="Where the DAT came from" />
                </label>
                <label>
                    "DAT file"
                    <input type="file" name="file" required accept=".dat,.xml" />
                </label>
                <button type="submit" class="primary" disabled=busy>
                    {move || if busy.get() { "Importing…" } else { "Import" }}
                </button>
            </form>
            {move || {
                outcome
                    .get()
                    .map(|outcome| match outcome {
                        Ok(imported) => {
                            view! {
                                <p class="done" role="status">
                                    {imported_message(imported.outcome, &imported.dat_revision)}
                                </p>
                            }
                                .into_any()
                        }
                        Err(message) => view! { <Problem message /> }.into_any(),
                    })
            }}
        </section>
    }
}

#[cfg(test)]
mod tests {
    use title_select_api::DatFormat;

    use super::*;

    fn revision(
        name: &str,
        provider: Option<&str>,
        version: &str,
        date: Option<&str>,
    ) -> RevisionItem {
        RevisionItem {
            revision: DatRevisionRecord {
                sha256: "254e0123456789abcdef".into(),
                dat: format!("{}/{name}", provider.unwrap_or("unknown-logiqx")),
                format: DatFormat::Logiqx,
                provider: provider.map(Into::into),
                name: name.into(),
                description: None,
                author: Some("Contributor".into()),
                homepage: None,
                header_url: None,
                version: version.into(),
                date: date.map(Into::into),
                file: format!("{name} ({version}).dat"),
                imported_at: "2026-11-01T10:00:00Z".into(),
                source_url: None,
                blob_present: true,
            },
            used_by: Vec::new(),
            cue_packs: Vec::new(),
            source_sets: Vec::new(),
        }
    }

    #[test]
    fn search_matches_name_provider_version_and_file_but_not_the_sha256() {
        let item = revision(
            "Nintendo - Game Boy",
            Some("No-Intro"),
            "20261101-101010",
            None,
        );
        let text = item.search().to_lowercase();
        for word in [
            "game boy",
            "no-intro",
            "20261101-101010",
            "(20261101-101010).dat",
        ] {
            assert!(text.contains(word), "{word} in {text}");
        }
        assert!(!text.contains("254e"), "{text}");
        assert!(
            !text.contains("contributor"),
            "the author is not the provider: {text}"
        );
    }

    #[test]
    fn an_unknown_provider_and_missing_date_and_version_say_so() {
        let item = revision("Homebrew", None, "", None);
        assert_eq!(provider_label(&item.revision), "Unknown");
        assert_eq!(given(&item.revision.version), "Not given");
        assert_eq!(given_or(item.revision.date.as_deref()), "Not given");
        assert!(item.search().to_lowercase().contains("unknown"));
        assert_eq!(item.title(), "Homebrew");

        let dated = revision(
            "Sony - PlayStation",
            Some("Redump"),
            "2026-11-01",
            Some("2026-11-01"),
        );
        assert_eq!(provider_label(&dated.revision), "Redump");
        assert_eq!(given_or(dated.revision.date.as_deref()), "2026-11-01");
        assert_eq!(dated.title(), "Sony - PlayStation 2026-11-01");
    }

    #[test]
    fn columns_sort_by_name_provider_date_and_source_sets() {
        let a = revision("A", Some("Redump"), "1", Some("2026-01-01"));
        let mut b = revision("B", None, "2", None);
        b.source_sets = vec!["gb".into(), "gbc".into()];
        assert_eq!(a.compare(&b, RevisionCol::Name), Ordering::Less);
        assert_eq!(a.compare(&b, RevisionCol::Provider), Ordering::Greater);
        assert_eq!(a.compare(&b, RevisionCol::Date), Ordering::Greater);
        assert_eq!(a.compare(&b, RevisionCol::SourceSets), Ordering::Less);
    }

    #[test]
    fn the_columns_fit_the_list_budget() {
        let widths: Vec<Width> = RevisionItem::COLUMNS
            .iter()
            .map(|&c| RevisionItem::column(c).width)
            .collect();
        let (_, min) = crate::parts::list_model::template(2.5, &widths, 2.25);
        assert!(min <= 50.0, "{min}rem");
    }

    #[test]
    fn the_source_sets_using_a_dat_are_those_bound_to_it() {
        let sets = [
            ("gb", "No-Intro/Nintendo - Game Boy"),
            ("psx", "Redump/Sony - PlayStation"),
            ("gb2", "No-Intro/Nintendo - Game Boy"),
        ];
        assert_eq!(
            using(
                "No-Intro/Nintendo - Game Boy",
                sets.iter().map(|&(n, d)| (n, d))
            ),
            vec!["gb".to_string(), "gb2".to_string()]
        );
    }

    #[test]
    fn a_kept_file_downloads_from_the_api_after_the_latest_audit_and_a_deleted_one_says_why_not() {
        let mut item = revision("Nintendo - Game Boy", Some("No-Intro"), "1", None);
        item.used_by = vec![
            UsedBy {
                audit: 3,
                source_set: "gb".into(),
                pruned: false,
            },
            UsedBy {
                audit: 7,
                source_set: "gb".into(),
                pruned: false,
            },
        ];
        let actions = revision_actions(&item);
        let labels: Vec<&str> = actions.iter().map(|a| a.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Open its latest audit, #7",
                "Download DAT",
                "Open audit #3 of gb"
            ]
        );
        assert!(
            matches!(&actions[1].act, crate::parts::list::Act::Download(url)
                if url == "/api/v1/dat-revisions/254e0123456789abcdef/file"),
        );

        item.revision.blob_present = false;
        let actions = revision_actions(&item);
        assert_eq!(actions[1].label, "Can't download DAT: its file was deleted");
        assert!(matches!(
            actions[1].act,
            crate::parts::list::Act::Unavailable
        ));
    }
}
