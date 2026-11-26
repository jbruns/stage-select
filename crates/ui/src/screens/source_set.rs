//! A source set's page: one hub, in tabs, for its status, games, entries, library folders, additions, history, kept
//! files and repair. The tab is the URL's `tab`, so every tab can be linked to.

use std::cmp::Ordering;

use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use leptos_router::components::A;
use leptos_router::hooks::{use_params_map, use_query_map};
use stage_select_api::additions::AdditionsSet;
use stage_select_api::build::{BuildSummary, LibraryFolder};
use stage_select_api::ingest::IngestPlan;
use stage_select_api::repair::RepairPlan;
use stage_select_api::{
    AuditEvidence, AuditSummary, Dat, DatChange, FollowedRevision, Job, OutsideCoverage,
    Placements, SetDat, SetFollow, SetHoldingFolder, SetRepairsMayChange, SourceSetChanged,
    SourceSetRecord, SourceSetSummary, StartAudit, StatWalk,
};

use crate::api;
use crate::app::{Loading, navigator};
use crate::build_model::build_state_label;
use crate::coverage_model::{label, outside_line, regions_line};
use crate::events::use_live;
use crate::kept_model::KeptPlace;
use crate::library_model::live_status;
use crate::model::{audit_choices, audit_evidence_label, revision_label, warning_counts, when};
use crate::parts::choice::Choice;
use crate::parts::help::Help;
use crate::parts::icon::Glyph;
use crate::parts::icon::Icon;
use crate::parts::layout::Crumbs;
use crate::parts::legend::{Legend, present};
use crate::parts::list::{Column, List, ListRow, RowAction, Width, last_good_rows};
use crate::parts::message::{Problem, Said};
use crate::parts::outcome::{JobOutcome, Report, Reports, ShowReports};
use crate::parts::overlay::{Drawer, Menu, MenuItem};
use crate::parts::status::{
    GameFlag, LibraryStatus, Placement, StaleCause, Status, StatusBadge, StatusIcon,
};
use crate::parts::tabs::{Tab, Tabs, use_tab};
use crate::placement_model::{
    Holding, flag_detail, folder_name, placed_line, present_games, review_line,
};
use crate::screens::additions::{AdditionsList, SetView};
use crate::screens::audit::{AuditCounts, EntriesPanel, FollowAudit, GamesPanel, load};
use crate::screens::coverage::{self, ByRegion, CoverageDrawer, UNAVAILABLE};
use crate::screens::fetching::{FETCHING_SETTING, FETCHING_STATUS};
use crate::screens::kept::KeptFiles;
use crate::screens::library_folders::live_jobs;
use crate::screens::lifecycle::{LifecycleSection, Paths, PlanAction, UnregisterAction};
use crate::screens::permissions::{PermissionsTab, Place};
use crate::screens::repair::{RepairDesk, RepairTab};
use crate::screens::source_sets::{
    Proposal, ProposalStatus, RepairNow, new_library_href, preview_href, repair_plan_href,
    stale_lines, start_audit, start_check, tab_href,
};
use crate::source_set_model::{
    self as model, ExportScope, Happened, MAY_CHANGE, MAY_CHANGE_NOTE, MissingExport, Moment,
    PageTab, no_coverage,
};
use crate::summary_model::{self, NextStep};

/// What following a DAT does, for the toggle's tooltip.
const FOLLOW_HINT: &str =
    "A newer revision of its DAT then makes it stale; following never starts an audit";

/// One source set's page.
#[component]
pub fn SourceSet() -> impl IntoView {
    let params = use_params_map();
    let name = Memo::new(move |_| params.read().get("name").unwrap_or_default());
    let set = LocalResource::new(move || {
        let name = name.get();
        async move {
            api::get::<SourceSetRecord>(&format!("/source-sets/{}", api::segment(&name))).await
        }
    });
    move || match set.get() {
        None => view! { <Loading what="the source set" /> }.into_any(),
        Some(Err(message)) => view! {
            <h1>"Source set"</h1>
            <Problem message />
        }
        .into_any(),
        // Each source set's page is its own, from its repair desk down: one left for another's is gone, not kept
        // while the other loads.
        Some(Ok(set)) if set.name == name.get() => view! { <SetPage set /> }.into_any(),
        Some(Ok(_)) => view! { <Loading what="the source set" /> }.into_any(),
    }
}

#[component]
fn SetPage(set: SourceSetRecord) -> impl IntoView {
    let live = use_live();
    let query = use_query_map();
    let name = StoredValue::new(set.name.clone());
    let desk = RepairDesk::new(set.name.clone(), set.dat.clone());
    let refresh = RwSignal::new(0u64);
    let summary = LocalResource::new(move || {
        live.settled.track();
        desk.track();
        refresh.track();
        // Owned, as the page may be gone by the time the answers come.
        let name = name.get_value();
        async move {
            let sets: Vec<SourceSetSummary> = api::get("/source-set-summaries").await?;
            let dats: Vec<Dat> = api::get("/dats").await?;
            let summary = sets
                .into_iter()
                .find(|s| s.set.name == name)
                .ok_or_else(|| "That source set is gone.".to_string())?;
            let revisions = dats
                .into_iter()
                .find(|d| d.dat == summary.set.dat)
                .map(|d| d.revisions)
                .unwrap_or_default();
            Ok::<_, String>((summary, revisions))
        }
    });
    let good = Memo::new(move |previous: Option<&Option<_>>| match summary.get() {
        Some(Ok(s)) => Some(s),
        _ => previous.cloned().flatten(),
    });
    let jobs_running = move || {
        live.book
            .with(|b| model::running(b.newest_first().iter().map(|l| &l.job)))
    };
    let status = Memo::new(move |_| {
        good.with(|g| {
            g.as_ref()
                .map(|(s, _)| model::status(s, jobs_running().contains(&name.get_value())))
        })
    });
    let auditing = Memo::new(move |_| {
        live.book.with(|b| {
            summary_model::audit_job(b.newest_first().iter().map(|l| &l.job), &name.get_value())
        })
    });
    let next = Memo::new(move |_| {
        good.with(|g| {
            g.as_ref()
                .map(|(s, _)| summary_model::next_step(s, auditing.get()))
        })
    });
    let repair_state = Memo::new(move |_| {
        let open = good.get().and_then(|(s, _)| s.repair);
        let job_running = open.as_ref().and_then(|o| o.job).is_some_and(|job| {
            live.book
                .with(|b| b.get(job).is_some_and(|l| !l.job.state.is_final()))
        });
        model::repair_state(open.as_ref(), job_running, desk.proposing().get())
    });
    let tab = Memo::new(move |_| PageTab::from_query(query.read().get("tab").as_deref()));
    // Each its own memo, so a change to the rest of the query (a filter, a sort, a row) remounts no tab.
    let named = Memo::new(move |_| query.read().get("plan").and_then(|p| p.parse::<i64>().ok()));
    let current = Memo::new(move |_| {
        good.with(|g| {
            g.as_ref()
                .and_then(|(s, _)| s.repair.as_ref().map(|r| r.id))
        })
    });
    let running = Memo::new(move |_| {
        good.with(|g| {
            g.as_ref()
                .is_some_and(|(s, _)| s.repair.as_ref().is_some_and(|r| r.approved))
        })
    });
    let ready = Memo::new(move |_| good.with(Option::is_some));
    let takes_coverage = Memo::new(move |_| {
        good.with(|g| {
            g.as_ref().is_some_and(|(_, revisions)| {
                no_coverage(revisions.iter().map(|r| r.format)).is_none()
            })
        })
    });
    let said = RwSignal::new(None::<Result<String, String>>);
    let editing = RwSignal::new(query.read_untracked().get("edit").as_deref() == Some("coverage"));
    let coverage =
        Signal::derive(move || good.with(|g| g.as_ref().map(|(s, _)| s.set.coverage.clone())));
    let reports = Reports::new();
    let url_tab = use_tab();
    let audit_now = move |_| {
        reports.clear();
        start_audit(name.get_value(), reports)
    };
    let check_now = move || {
        reports.clear();
        start_check(name.get_value(), reports)
    };
    let navigate = navigator();
    let proposal = Proposal::new();
    let started = Signal::derive(move || {
        reports
            .get()
            .into_iter()
            .filter_map(|r| match r {
                Report::Job(job) => Some(job.id),
                _ => None,
            })
            .collect::<Vec<_>>()
    });
    let options = RwSignal::new(false);
    let changing_dat = RwSignal::new(false);
    let audited =
        Memo::new(move |_| good.with(|g| g.as_ref().is_some_and(|(s, _)| s.latest.is_some())));
    let follows = Memo::new(move |_| good.with(|g| g.as_ref().is_some_and(|(s, _)| s.set.follow)));
    let follow = Callback::new(move |follow: bool| {
        let request = SetFollow { follow };
        leptos::task::spawn_local(async move {
            let path = format!("/source-sets/{}/follow", api::segment(&name.get_value()));
            if let Err(problem) = api::put::<_, SourceSetRecord>(&path, &request).await {
                said.set(Some(Err(format!(
                    "Couldn't change whether it follows its DAT: {problem}"
                ))));
            }
            refresh.update(|n| *n += 1);
        });
    });
    let may_change = Memo::new(move |_| {
        good.with(|g| g.as_ref().is_some_and(|(s, _)| s.set.repairs_may_change))
    });
    let let_change = Callback::new(move |on: bool| {
        let request = SetRepairsMayChange {
            repairs_may_change: on,
        };
        leptos::task::spawn_local(async move {
            let set = name.get_value();
            let path = format!("/source-sets/{}/repairs-may-change", api::segment(&set));
            said.set(Some(
                match api::put::<_, SourceSetChanged>(&path, &request).await {
                    Ok(changed) => Ok(model::may_change_said(&set, on, &changed.voided)),
                    Err(problem) => Err(format!(
                        "Couldn't change whether Stage Select may change it: {problem}"
                    )),
                },
            ));
            refresh.update(|n| *n += 1);
        });
    });

    let page_tabs = Signal::derive(move || {
        let open = tab.get();
        PageTab::ALL
            .into_iter()
            .map(|t| {
                let key = (t != PageTab::Overview).then_some(t.slug());
                let status = (t == PageTab::Repair).then(|| repair_state.get()).flatten();
                Tab::new(url_tab.href(key), t.label(), t == open).status(status)
            })
            .collect::<Vec<_>>()
    });

    view! {
        <Crumbs trail=vec![("/source-sets".to_string(), "Source sets".to_string())] here=set.name.clone() />
        <header class="page-head">
            <h1>{set.name.clone()}</h1>
            {move || status.get().map(|s| view! { <StatusBadge status=s /> })}
        </header>
        <div class="actions">
            {move || match next.get() {
                Some(NextStep::Repair) => view! {
                    <RepairNow
                        proposal
                        name=name.get_value()
                        title="Proposes a repair from its latest audit, and opens the plan for your review. Nothing changes until you approve it."
                    />
                    <button type="button" on:click=audit_now>"Audit"</button>
                }
                .into_any(),
                // Its audit is on its way: follow it below rather than propose a repair it is about to supersede.
                Some(NextStep::Auditing(_)) => view! {
                    <button type="button" on:click=audit_now>"Audit"</button>
                }
                .into_any(),
                Some(NextStep::Review(plan)) => view! {
                    <A href=repair_plan_href(&name.get_value(), plan) attr:class="button primary">
                        {format!("Review repair #{plan}")}
                    </A>
                    <button type="button" on:click=audit_now>"Audit"</button>
                }
                .into_any(),
                // The latest audit found nothing a repair surely changes, but spare copies and the storage policy
                // aren't counted ahead of a repair.
                Some(NextStep::MaybeRepair) => view! {
                    <button type="button" class="primary" on:click=audit_now>"Audit"</button>
                    <RepairNow
                        proposal
                        name=name.get_value()
                        title="A repair may still set aside spare copies or repack games to your storage policy, which an audit doesn't count. It opens the plan for your review; nothing changes until you approve it."
                        secondary=true
                    />
                }
                .into_any(),
                _ => view! {
                    <button type="button" class="primary" on:click=audit_now>"Audit"</button>
                }
                .into_any(),
            }}
            <button type="button" on:click=move |_| options.set(true)>
                "Audit with options"
            </button>
            <Menu glyph=Glyph::Ellipsis label="More actions">
                <MenuItem
                    label="Check for changes"
                    on_select=Callback::new(move |()| check_now())
                    disabled=Signal::derive(move || !audited.get())
                />
                <MenuItem
                    label="Preview a selection"
                    on_select=Callback::new(move |()| navigate.run(preview_href(&name.get_value())))
                />
                <MenuItem
                    label="New library folder"
                    on_select=Callback::new(move |()| navigate.run(new_library_href(&name.get_value())))
                />
                <MenuItem
                    label="Change DAT…"
                    on_select=Callback::new(move |()| changing_dat.set(true))
                />
            </Menu>
        </div>
        <Said said />
        <ProposalStatus proposal />
        <ShowReports reports />
        <FollowAudit source_set=set.name.clone() shown=started repair=desk.audit_job() />
        <Tabs label="Source set" tabs=page_tabs />
        {move || match summary.get() {
            Some(Err(message)) if good.with(Option::is_none) => {
                view! { <Problem message /> }.into_any()
            }
            _ if good.with(Option::is_none) => view! { <Loading what="its status" /> }.into_any(),
            _ => ().into_any(),
        }}
        {move || {
            if !ready.get() {
                return None;
            }
            if tab.get() == PageTab::Repair {
                return Some(view! { <RepairTab desk named current running /> }.into_any());
            }
            Some((move || {
            let (summary, revisions) = good.get()?;
            let latest = summary.latest.clone();
            Some(match tab.get() {
                PageTab::Overview => view! {
                    <Overview
                        set=summary.set.clone()
                        latest
                        revisions
                        outside=summary.outside_coverage.clone()
                        unavailable=summary.coverage_unavailable
                        present=present_games(&summary)
                        editing
                        follows
                        follow
                        may_change
                        let_change
                    />
                }
                .into_any(),
                PageTab::Games => view! { <LatestAudit latest games=true /> }.into_any(),
                PageTab::Entries => view! { <LatestAudit latest games=false /> }.into_any(),
                PageTab::Libraries => {
                    view! { <Libraries name=name.get_value() /> }.into_any()
                }
                PageTab::Additions => view! { <Additions system=summary.set.dat.clone() /> }.into_any(),
                PageTab::History => view! { <History name=name.get_value() /> }.into_any(),
                PageTab::Kept => {
                    view! { <KeptFiles place=KeptPlace::SourceSet(name.get_value()) /> }.into_any()
                }
                PageTab::Permissions => view! {
                    <PermissionsTab
                        place=Place::SourceSet(name.get_value())
                    />
                }
                .into_any(),
                PageTab::Repair => ().into_any(),
            })
            }).into_any())
        }}
        // A MAME Source Set takes no Coverage, so not even a link opens the editor for it.
        {move || {
            takes_coverage
                .get()
                .then(|| view! { <CoverageDrawer name=name.get_value() open=editing coverage refresh said /> })
        }}
        <Drawer open=changing_dat title="Change DAT">
            // Mounted as it opens, so each opening previews afresh.
            {move || {
                changing_dat.get().then(|| {
                    good.get_untracked().map(|(s, _)| {
                        view! { <ChangeDat set=s.set open=changing_dat refresh said /> }
                    })
                })
            }}
        </Drawer>
        <Drawer open=options title="Audit with options">
            // Read once as it opens: a job settling while it's open keeps the form, and the outcome under it.
            {move || {
                good.get_untracked()
                    .map(|(s, revisions)| {
                        view! { <StartAuditForm name=name.get_value() latest=s.latest revisions /> }
                    })
            }}
        </Drawer>
    }
}

/// The overview: why the source set stands as it does, the latest audit's games and findings, each count opening
/// the list it counts, and where its games are kept under single copy. Its coverage, its facts, the export of what's
/// missing and unregistering wait folded beneath.
#[component]
fn Overview(
    set: SourceSetRecord,
    latest: Option<AuditSummary>,
    revisions: Vec<stage_select_api::DatRevisionRecord>,
    outside: Option<OutsideCoverage>,
    unavailable: bool,
    present: Option<u64>,
    editing: RwSignal<bool>,
    follows: Memo<bool>,
    follow: Callback<bool>,
    may_change: Memo<bool>,
    let_change: Callback<bool>,
) -> impl IntoView {
    let name = set.name.clone();
    // A coverage set is still yours when it can't be applied; only the counts then aren't scoped to it.
    let covered = set.coverage.is_some() && !unavailable;
    let unavailable_note = (set.coverage.is_some() && unavailable)
        .then(|| view! { <p class="note">{UNAVAILABLE}</p> });
    let outside_line = outside
        .filter(|_| covered)
        .map(|o| view! { <p class="note">{outside_line(&o)}</p> });
    let counts = match &latest {
        None => view! {
            <p class="note">"Not audited yet. Audit it to see its games and entries."</p>
        }
        .into_any(),
        Some(a) => view! {
            <p class="note small from-audit">
                "From "
                <A href=format!("/audits/{}", a.id)>{format!("audit #{}", a.id)}</A>
                {format!(", {}.", when(&a.finished_at))}
            </p>
            <AuditCounts audit=a.clone() lists=summary_model::ON_SOURCE_SET covered />
            {unavailable_note}
            {outside_line}
        }
        .into_any(),
    };
    let facts = latest.clone().map(|a| {
        view! {
            <dt>"Against"</dt>
            <dd>
                <super::dats::RevisionName
                    sha256=a.dat_revision_sha256.clone()
                    version=a.dat_revision_version.clone()
                />
            </dd>
            <dt>"Evidence"</dt>
            <dd>{audit_evidence_label(a.evidence)}</dd>
            <dt>"Entries read"</dt>
            <dd>{model::evidence_coverage(&a)}</dd>
            {warning_counts(&a).map(|said| view! {
                <dt>"Warnings"</dt>
                <dd>{said}</dd>
            })}
        }
    });
    let coverage = set.coverage.clone();
    let (glyph, kind) = coverage::glyph(coverage.as_deref(), unavailable);
    // A MAME Source Set covers every Game, and offers no control to change that.
    let refused = no_coverage(revisions.iter().map(|r| r.format));
    let (kind, line) = match (refused, coverage.as_deref()) {
        (Some(why), _) => ("Every game", why.to_owned()),
        (None, None) => (kind, regions_line(None)),
        (None, Some(regions)) => (
            kind,
            format!("{}: {}", label(Some(regions)), regions_line(Some(regions))),
        ),
    };
    view! {
        {stale_lines(&set)}
        {counts}
        <SingleCopy name=set.name.clone() present />
        <details class="fold">
            <summary>"Coverage"</summary>
            <p>
                <Icon glyph label=kind />
                " "
                {line}
            </p>
            {refused.is_none().then(|| view! {
                <div class="actions">
                    <button type="button" on:click=move |_| editing.set(true)>
                        "Edit coverage"
                    </button>
                </div>
            })}
            {latest.is_some().then(|| view! { <ByRegion name=name.clone() /> })}
        </details>
        <details class="fold">
            <summary>"About it"</summary>
            <dl class="facts">
                <dt>"DAT"</dt>
                <dd>{set.dat.clone()}</dd>
                <dt>"Fetching its DAT"</dt>
                <dd>
                    <A href=FETCHING_STATUS>"Where it stands"</A>
                    " · "
                    <A href=FETCHING_SETTING>"Turn it on or off"</A>
                </dd>
                <dt>"Root and path"</dt>
                <dd>
                    <code class="path">{format!("{}: {}", set.root, set.path)}</code>
                </dd>
                <dt>"Registered"</dt>
                <dd>{when(&set.added_at)}</dd>
                {facts}
                <dt>"Last verified against"</dt>
                <dd>
                    {set
                        .last_verified_against
                        .clone()
                        .map_or("Never".to_string(), |last| format!("{} ({})", last.version, when(&last.finished_at)))}
                </dd>
            </dl>
            <label class="inline" title=FOLLOW_HINT>
                <input
                    type="checkbox"
                    prop:checked=follows
                    on:change=move |ev| follow.run(event_target_checked(&ev))
                />
                " Follow its DAT"
            </label>
            <Choice checked=may_change on_change=let_change note=|| MAY_CHANGE_NOTE>
                {MAY_CHANGE}
            </Choice>
        </details>
        {latest
            .clone()
            .filter(|a| !a.pruned)
            .map(|a| view! { <ExportMissing name=set.name.clone() latest=a coverage_set=set.coverage.is_some() unavailable /> })}
        <SetLifecycle name=set.name.clone() />
    }
}

/// Binding the source set to another DAT of its format: a choice of DAT, what the change would do or why it's
/// refused, and a confirmation.
#[component]
fn ChangeDat(
    set: SourceSetRecord,
    open: RwSignal<bool>,
    refresh: RwSignal<u64>,
    said: RwSignal<Option<Result<String, String>>>,
) -> impl IntoView {
    let name = StoredValue::new(set.name.clone());
    let current = set.dat.clone();
    let dats = LocalResource::new(|| api::get::<Vec<Dat>>("/dats"));
    let choices = Memo::new(move |_| {
        dats.get()
            .and_then(Result::ok)
            .map(|dats| model::dat_choices(&dats, &current))
    });
    let chosen = RwSignal::new(String::new());
    Effect::new(move || {
        if chosen.get_untracked().is_empty()
            && let Some(first) = choices.get().and_then(|c| c.into_iter().next())
        {
            chosen.set(first);
        }
    });
    let preview = LocalResource::new(move || {
        let dat = chosen.get();
        let path = format!(
            "/source-sets/{}/dat-change?dat={}",
            api::segment(&name.get_value()),
            api::segment(&dat)
        );
        async move {
            if dat.is_empty() {
                return None;
            }
            Some(api::get::<DatChange>(&path).await)
        }
    });
    let busy = RwSignal::new(false);
    let problem = RwSignal::new(None::<String>);
    let confirm = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let request = SetDat {
            dat: chosen.get_untracked(),
        };
        busy.set(true);
        problem.set(None);
        leptos::task::spawn_local(async move {
            let path = format!("/source-sets/{}/dat", api::segment(&name.get_value()));
            let changed = api::put::<_, DatChange>(&path, &request).await;
            busy.set(false);
            match changed {
                Ok(change) => {
                    said.set(Some(Ok(model::dat_changed_said(&change))));
                    refresh.update(|n| *n += 1);
                    open.set(false);
                }
                Err(message) => problem.set(Some(format!("Couldn't change its DAT: {message}"))),
            }
        });
    };
    view! {
        <p>{format!("Bound to {}.", set.dat)}</p>
        {move || match choices.get() {
            None => view! { <Loading what="the DATs" /> }.into_any(),
            Some(choices) if choices.is_empty() => view! {
                <p class="note">"No other DAT of its DAT format is imported. Import the DAT you want first."</p>
            }
            .into_any(),
            Some(choices) => view! {
                <form class="form" on:submit=confirm>
                    <label>
                        "Bind it to"
                        <select required prop:value=chosen on:change=move |ev| chosen.set(event_target_value(&ev))>
                            {choices
                                .into_iter()
                                .map(|d| view! { <option value=d.clone()>{d.clone()}</option> })
                                .collect_view()}
                        </select>
                    </label>
                    {move || match preview.get().flatten() {
                        None => view! { <p class="note">"Checking what it would change…"</p> }.into_any(),
                        Some(Err(message)) => view! { <Problem message /> }.into_any(),
                        Some(Ok(change)) => view! {
                            <ul>
                                {model::dat_change_lines(&change)
                                    .into_iter()
                                    .map(|line| view! { <li>{line}</li> })
                                    .collect_view()}
                            </ul>
                        }
                        .into_any(),
                    }}
                    <button
                        type="submit"
                        class="primary"
                        disabled=move || busy.get() || !matches!(preview.get().flatten(), Some(Ok(_)))
                    >
                        "Change DAT"
                    </button>
                </form>
            }
            .into_any(),
        }}
        {move || problem.get().map(|message| view! { <Problem message /> })}
    }
}

/// Where the source set keeps its games under single copy: how many each library folder holds as their only copy,
/// the folder that holds them and, when several are built from it, your choice of it; and the games a build found
/// need review.
#[component]
fn SingleCopy(name: String, present: Option<u64>) -> impl IntoView {
    let live = use_live();
    let name = StoredValue::new(name);
    let refresh = RwSignal::new(0u64);
    let loaded = LocalResource::new(move || {
        live.settled.track();
        refresh.track();
        let path = format!(
            "/source-sets/{}/placements",
            api::segment(&name.get_value())
        );
        async move {
            let placements: Placements = api::get(&path).await?;
            let folders: Vec<LibraryFolder> = api::get("/libraries").await?;
            Ok::<_, String>((placements, folders))
        }
    });
    let problem = RwSignal::new(None::<String>);
    let choosing = RwSignal::new(false);
    let choose = move |folder: Option<i64>| {
        choosing.set(true);
        problem.set(None);
        let request = SetHoldingFolder { folder };
        leptos::task::spawn_local(async move {
            let path = format!(
                "/source-sets/{}/holding-folder",
                api::segment(&name.get_value())
            );
            if let Err(message) = api::put::<_, Placements>(&path, &request).await {
                problem.set(Some(format!(
                    "Couldn't change the folder holding its games: {message}"
                )));
            }
            choosing.set(false);
            refresh.update(|n| *n += 1);
        });
    };
    move || {
        let (placements, folders) = match loaded.get()? {
            Ok(loaded) => loaded,
            Err(message) => return Some(view! { <Problem message /> }.into_any()),
        };
        let holding = Holding::of(&folders, &name.get_value(), placements.holding_folder);
        if holding == Holding::NoFolder
            && placements.placed.is_empty()
            && placements.review_flags.is_empty()
        {
            return None;
        }
        let placed = if placements.placed.is_empty() {
            view! { <p>"No games placed: each is kept in your source set's own folder."</p> }
                .into_any()
        } else {
            view! {
                <ul class="plain">
                    {placements
                        .placed
                        .iter()
                        .map(|p| {
                            let line = placed_line(p.games, present, &folder_name(&folders, p.library_folder));
                            view! {
                                <li>
                                    <StatusIcon status=Placement::Placed />
                                    " "
                                    <A href=format!("/libraries/{}", p.library_folder)>{line}</A>
                                </li>
                            }
                        })
                        .collect_view()}
                </ul>
            }
            .into_any()
        };
        let note = holding.note();
        let choice = match holding {
            Holding::NoFolder => ().into_any(),
            Holding::Only(id) => view! {
                <p>
                    "Library folder holding placed games: "
                    <A href=format!("/libraries/{id}")>{folder_name(&folders, id)}</A>
                </p>
            }
            .into_any(),
            Holding::Choose {
                folders: bound,
                chosen,
            } => {
                let options = bound
                    .iter()
                    .map(|id| {
                        view! {
                            <option value=id.to_string() selected=chosen == Some(*id)>
                                {folder_name(&folders, *id)}
                            </option>
                        }
                    })
                    .collect_view();
                view! {
                    <label class="stacked" for="holding-folder">
                        "Library folder holding placed games"
                    </label>
                    <select
                        id="holding-folder"
                        disabled=choosing
                        on:change=move |ev| choose(event_target_value(&ev).parse::<i64>().ok())
                    >
                        <option value="" selected=chosen.is_none()>
                            "None chosen"
                        </option>
                        {options}
                    </select>
                }
                .into_any()
            }
        };
        let flags = (!placements.review_flags.is_empty()).then(|| {
            view! {
                <h3>{review_line(placements.review_flags.len())}</h3>
                <ul class="plain">
                    {placements
                        .review_flags
                        .iter()
                        .map(|f| {
                            view! {
                                <li>
                                    <StatusIcon status=GameFlag::NeedsReview />
                                    " "
                                    <strong>{f.game.clone()}</strong>
                                    {format!(": {}", flag_detail(f))}
                                </li>
                            }
                        })
                        .collect_view()}
                </ul>
            }
        });
        Some(
            view! {
                <section aria-labelledby="single-copy-title">
                    <h2 id="single-copy-title">"Single copy"</h2>
                    {placed}
                    {choice}
                    <p class="note">{note}</p>
                    {move || problem.get().map(|message| view! { <Problem message /> })}
                    {flags}
                </section>
            }
            .into_any(),
        )
    }
}

/// Downloads of what the latest audit found missing: in the source set's coverage, in the whole DAT, or among one
/// selection's winners.
#[component]
fn ExportMissing(
    name: String,
    latest: AuditSummary,
    coverage_set: bool,
    unavailable: bool,
) -> impl IntoView {
    let set = api::segment(&name);
    let scope = RwSignal::new(ExportScope::default_for(coverage_set));
    let cant_scope = move || {
        (unavailable && scope.get() == ExportScope::Coverage).then(|| {
            view! {
                <p class="note" role="status">
                    "Your coverage can't be applied now, as the games' regions can't be read, so this fixdat can't \
                    be made. Choose the whole DAT to list everything missing."
                </p>
            }
        })
    };
    let selections = {
        let set = set.clone();
        LocalResource::new(move || {
            let path = format!("/selections?source_set={set}");
            async move { api::get::<Vec<stage_select_api::SelectionRecord>>(&path).await }
        })
    };
    let options = move || {
        selections.get().and_then(Result::ok).map(|records| {
            model::export_selections(&latest, &records)
                .into_iter()
                .map(|s| view! { <option value=s.id.to_string()>{model::selection_option(s)}</option> })
                .collect_view()
        })
    };
    let link = move |export: MissingExport| {
        let set = set.clone();
        move || api::url(&model::missing_export_path(&set, export, scope.get()))
    };
    view! {
        <details class="fold">
            <summary>"Export what's missing"</summary>
            <p class="note">
                "Lists what your latest audit found missing, as a fixdat or a have/miss CSV. Nothing is fetched. \
                The have/miss CSV lists every game of the DAT unless you choose a selection."
            </p>
            <div class="form">
                <label>
                    "Games"
                    <select on:change=move |ev| scope.set(ExportScope::parse(&event_target_value(&ev)))>
                        {coverage_set.then(|| view! {
                            <option value=ExportScope::Coverage.value()>"Missing in your coverage"</option>
                        })}
                        <option value=ExportScope::Dat.value()>"The whole DAT"</option>
                        {options}
                    </select>
                </label>
            </div>
            <div class="actions">
                <a class="button" href=link(MissingExport::Fixdat) download="">
                    "Download fixdat"
                </a>
                <a class="button" href=link(MissingExport::HaveMiss) download="">
                    "Download have/miss CSV"
                </a>
            </div>
            {cant_scope}
        </details>
    }
}

/// Setting the source set aside, or unregistering it.
#[component]
fn SetLifecycle(name: String) -> impl IntoView {
    let base = format!("/source-sets/{}", api::segment(&name));
    let review = name.clone();
    let href = Callback::new(move |plan: i64| {
        format!("{}&plan={plan}", tab_href(&review, PageTab::Repair))
    });
    view! {
        <LifecycleSection>
            <PlanAction
                label="Set aside"
                explain="Set aside plans to keep every file of the source set aside in its own folder, renamed in \
                    place so that it takes no more space, and then to unregister it. You review the plan before \
                    anything moves; once its kept files are deleted, the folder can be registered again."
                    .to_string()
                path=format!("{base}/set-aside")
                href
            />
            <UnregisterAction
                what=format!("the source set {name}")
                paths=Paths {
                    holds: format!("{base}/holds"),
                    unregister: format!("{base}/unregistration"),
                }
                left="Its files stay where they are, no longer managed."
            />
        </LifecycleSection>
    }
}

#[component]
fn StartAuditForm(
    name: String,
    latest: Option<AuditSummary>,
    revisions: Vec<stage_select_api::DatRevisionRecord>,
) -> impl IntoView {
    let (default, newer) = audit_choices(latest.as_slice(), &revisions);
    let default = match default {
        Some(r) => format!("{}, the default", revision_label(r)),
        None => "The default".to_string(),
    };
    let newer: Vec<_> = newer.into_iter().cloned().collect();
    let revision = RwSignal::new(String::new());
    let evidence = RwSignal::new(AuditEvidence::Incremental);
    let outcome = RwSignal::new(None::<Result<Job, String>>);
    let name = StoredValue::new(name);
    let start = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let chosen = revision.get_untracked();
        let request = StartAudit {
            source_set: name.get_value(),
            revision: (!chosen.is_empty()).then_some(chosen),
            evidence: evidence.get_untracked(),
        };
        leptos::task::spawn_local(async move {
            outcome.set(Some(api::post::<_, Job>("/audits", &request).await));
        });
    };
    view! {
        <form class="form" on:submit=start>
                <label>
                    "DAT revision"
                    <select prop:value=revision on:change=move |ev| revision.set(event_target_value(&ev))>
                        <option value="">{default}</option>
                        {newer
                            .into_iter()
                            .map(|r| {
                                let label = format!("{}, newer", revision_label(&r));
                                view! { <option value=r.sha256>{label}</option> }
                            })
                            .collect_view()}
                    </select>
                </label>
                <label>
                    "Evidence"
                    <select
                        prop:value=move || evidence.get().as_str()
                        on:change=move |ev| {
                            if let Some(chosen) = AuditEvidence::parse(&event_target_value(&ev)) {
                                evidence.set(chosen);
                            }
                        }
                    >
                        {AuditEvidence::ALL
                            .into_iter()
                            .map(|e| view! { <option value=e.as_str()>{audit_evidence_label(e)}</option> })
                            .collect_view()}
                    </select>
                </label>
                <button type="submit" class="primary">
                    "Start audit"
                </button>
            </form>
            {move || {
                outcome
                    .get()
                    .map(|outcome| match outcome {
                        Ok(job) => view! { <JobOutcome job=job.id seed=job /> }.into_any(),
                        Err(message) => view! { <Problem message /> }.into_any(),
                    })
            }}
    }
}

/// The latest audit's game statuses, or its entries, narrowed by the URL as their lists read it.
#[component]
fn LatestAudit(latest: Option<AuditSummary>, games: bool) -> impl IntoView {
    let Some(audit) = latest else {
        return view! { <p class="note">"Not audited yet."</p> }.into_any();
    };
    if audit.pruned {
        return view! {
            <p class="note">"The latest audit is pruned: only its summary is kept, on the overview."</p>
        }
        .into_any();
    }
    let id = audit.id;
    let rule = audit.header_rule.as_ref().and_then(|r| r.rule);
    let data = LocalResource::new(move || load(id));
    view! {
        <p class="note">
            {format!("From audit #{id}, {}. ", when(&audit.finished_at))}
            <A href=format!("/audits/{id}")>"Open the audit"</A>
        </p>
        {move || match data.get() {
            None => view! { <Loading what="the audit" /> }.into_any(),
            Some(Err(message)) => view! { <Problem message /> }.into_any(),
            Some(Ok(loaded)) if games => {
                view! { <GamesPanel audit=id games=loaded.games rule source_set=audit.source_set.clone() /> }.into_any()
            }
            Some(Ok(loaded)) => {
                view! {
                    <EntriesPanel entries=loaded.entries source_set=audit.source_set.clone() />
                }.into_any()
            }
        }}
    }
    .into_any()
}

#[derive(Clone, PartialEq)]
struct FolderRow {
    folder: LibraryFolder,
    status: LibraryStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FolderCol {
    Status,
    Path,
    Profile,
    Built,
}

impl ListRow for FolderRow {
    type Col = FolderCol;
    const COLUMNS: &'static [FolderCol] = &[
        FolderCol::Status,
        FolderCol::Path,
        FolderCol::Profile,
        FolderCol::Built,
    ];

    fn column(col: FolderCol) -> Column {
        match col {
            FolderCol::Status => Column::new("status", "Status", Width::Fixed(3.5)).sortable(),
            FolderCol::Path => Column::new(
                "path",
                "Library folder",
                Width::Flex {
                    min: 14.0,
                    share: 2,
                },
            )
            .sortable(),
            FolderCol::Profile => Column::new("profile", "Profile", Width::Fixed(10.0)).sortable(),
            FolderCol::Built => Column::new("built", "Built", Width::Fixed(11.0)).sortable(),
        }
    }

    fn key(&self) -> String {
        self.folder.id.to_string()
    }

    fn title(&self) -> String {
        format!("{}: {}", self.folder.root, self.folder.path)
    }

    fn search(&self) -> String {
        format!(
            "{} {} {}",
            self.title(),
            self.folder.profile,
            self.status.look().label
        )
    }

    fn href(&self) -> Option<String> {
        Some(format!("/libraries/{}", self.folder.id))
    }

    fn cell(&self, col: FolderCol) -> AnyView {
        match col {
            FolderCol::Status => {
                view! { <StatusIcon status=self.status tab_stop=false /> }.into_any()
            }
            FolderCol::Path => view! { <code class="path">{self.title()}</code> }.into_any(),
            FolderCol::Profile => self.folder.profile.clone().into_any(),
            FolderCol::Built => built(&self.folder).into_any(),
        }
    }

    fn compare(&self, other: &Self, col: FolderCol) -> Ordering {
        match col {
            FolderCol::Status => self.status.rank().cmp(&other.status.rank()),
            FolderCol::Path => self.title().cmp(&other.title()),
            FolderCol::Profile => self.folder.profile.cmp(&other.folder.profile),
            FolderCol::Built => built_at(&self.folder).cmp(&built_at(&other.folder)),
        }
    }
}

fn built_at(folder: &LibraryFolder) -> Option<String> {
    folder.build.as_ref().map(|b| {
        b.finished_at
            .clone()
            .unwrap_or_else(|| b.started_at.clone())
    })
}

fn built(folder: &LibraryFolder) -> String {
    built_at(folder).map_or("Never".to_string(), |at| when(&at))
}

/// The source set's library folders, with their status.
#[component]
fn Libraries(name: String) -> impl IntoView {
    let live = use_live();
    let name = StoredValue::new(name);
    let folders = LocalResource::new(move || {
        live.settled.track();
        async move { api::get::<Vec<LibraryFolder>>("/libraries").await }
    });
    let jobs = Memo::new(move |_| live_jobs(live));
    // A failed refresh keeps the rows last shown; the problem shows above them.
    let rows = last_good_rows(move || {
        let jobs = jobs.get();
        folders.get().map(|loaded| {
            loaded.map(|folders| {
                folders
                    .into_iter()
                    .filter(|f| f.source_set == name.get_value())
                    .map(|folder| FolderRow {
                        status: live_status(&folder, &jobs),
                        folder,
                    })
                    .collect::<Vec<_>>()
            })
        })
    });
    view! {
        {move || match folders.get() {
            None => view! { <Loading what="its library folders" /> }.into_any(),
            Some(Err(message)) => view! { <Problem message /> }.into_any(),
            Some(Ok(_)) => ().into_any(),
        }}
        <List
            id="set-libraries"
            label="Library folders"
            rows
            empty="No library folder is built from this source set yet."
            placeholder="Filter by folder, profile or status"
            legend=move || {
                let statuses = Signal::derive(move || present(rows.get().iter().map(|r| r.status)));
                view! { <Legend statuses list="set-libraries" /> }
            }
            row_actions=Callback::new(|row: FolderRow| {
                vec![RowAction::go(Glyph::Library, "Open the library folder", format!("/libraries/{}", row.folder.id))]
            })
            detail=Callback::new(|row: FolderRow| {
                view! {
                    <dl class="facts">
                        <dt>"Status"</dt>
                        <dd><StatusBadge status=row.status /></dd>
                        <dt>"Profile"</dt>
                        <dd>{row.folder.profile.clone()}</dd>
                        <dt>"Built"</dt>
                        <dd>{built(&row.folder)}</dd>
                        <dt>"Files"</dt>
                        <dd>{row.folder.files}</dd>
                    </dl>
                }
                    .into_any()
            })
        />
        <p>
            <A href=new_library_href(&name.get_value())>"Make a new library folder from it"</A>
        </p>
    }
}

/// The additions set of the source set's DAT.
#[component]
fn Additions(system: String) -> impl IntoView {
    let live = use_live();
    let refresh = RwSignal::new(0u64);
    let system = StoredValue::new(system);
    // Every additions set, rather than the one of the DAT: a DAT without additions has none, which isn't a problem.
    let set = LocalResource::new(move || {
        live.settled.track();
        refresh.track();
        let system = system.get_value();
        async move {
            let sets = api::get::<Vec<AdditionsSet>>("/additions-sets").await?;
            Ok::<_, String>(sets.into_iter().find(|s| s.system == system))
        }
    });
    view! {
        <p class="lede">
            "Files no DAT knows that you chose to keep for this DAT, each known by its own hashes and a name you give \
            it. A repair adds one when you choose to add an unmatched file as an addition. "
            <A href="/additions">"Every additions set"</A>
        </p>
        {move || match set.get() {
            None => view! { <Loading what="its additions" /> }.into_any(),
            Some(Err(message)) => view! { <Problem message /> }.into_any(),
            Some(Ok(None)) => view! { <p class="note">"This DAT has no additions yet."</p> }.into_any(),
            Some(Ok(Some(set))) => {
                let additions = Signal::stored(std::sync::Arc::new(set.additions.clone()));
                view! {
                    <AdditionsList additions refresh />
                    <SetView set refresh />
                }
                .into_any()
            }
        }}
    }
}

#[derive(Clone, PartialEq)]
struct MomentRow {
    moment: Moment,
    set: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MomentCol {
    When,
    What,
}

impl MomentRow {
    fn what(&self) -> String {
        match &self.moment.what {
            Happened::Audit { id, .. } => format!("Audit #{id}"),
            Happened::Repair { id, .. } => format!("Repair #{id}"),
            Happened::Ingest { repair, .. } => format!("Ingest by repair #{repair}"),
            Happened::Ingested { id, .. } => format!("Ingest plan #{id}"),
            Happened::Build { id, .. } => format!("Build #{id}"),
            Happened::FilesChanged { .. } => StaleCause::FilesChanged.look().label.to_string(),
            Happened::NewerRevision { .. } => StaleCause::NewerRevision.look().label.to_string(),
        }
    }

    /// The icon of a moment that made the source set stale.
    fn cause(&self) -> Option<StaleCause> {
        match &self.moment.what {
            Happened::FilesChanged { .. } => Some(StaleCause::FilesChanged),
            Happened::NewerRevision { .. } => Some(StaleCause::NewerRevision),
            _ => None,
        }
    }

    fn detail(&self) -> String {
        match &self.moment.what {
            Happened::Audit {
                revision,
                needs_review,
                pruned,
                ..
            } => format!(
                "Against {revision}; {needs_review} {} review{}",
                if *needs_review == 1 {
                    "entry needs"
                } else {
                    "entries need"
                },
                if *pruned { "; pruned" } else { "" }
            ),
            Happened::Repair {
                changes,
                approved,
                finished,
                ..
            } => format!(
                "{changes} {}; {}",
                if *changes == 1 { "change" } else { "changes" },
                match (approved, finished) {
                    (_, true) => "finished",
                    (true, false) => "approved",
                    (false, false) => "proposed",
                }
            ),
            Happened::Ingest { games, .. } => format!(
                "Filled {games} {} from ingest folders",
                if *games == 1 { "file" } else { "files" }
            ),
            Happened::Ingested {
                root,
                addition,
                items,
                finished,
                ..
            } => format!(
                "{} {items} {} from {root}{}",
                if *addition { "Added" } else { "Imported" },
                match (addition, *items == 1) {
                    (true, true) => "addition",
                    (true, false) => "additions",
                    (false, true) => "DAT",
                    (false, false) => "DATs",
                },
                if *finished { "" } else { "; still running" }
            ),
            Happened::Build { path, state, .. } => {
                format!("{} into {path}", build_state_label(*state))
            }
            Happened::FilesChanged {
                audit,
                changed,
                added,
                removed,
                ..
            } => format!(
                "{} since audit #{audit}",
                model::change_counts(*changed, *added, *removed)
            ),
            Happened::NewerRevision { version, .. } => {
                format!("Revision {version} of its DAT made it stale")
            }
        }
    }

    fn link(&self) -> String {
        match &self.moment.what {
            Happened::Audit { id, .. } => format!("/audits/{id}"),
            Happened::Repair { id, .. } | Happened::Ingest { repair: id, .. } => {
                format!("{}&plan={id}", tab_href(&self.set, PageTab::Repair))
            }
            Happened::Ingested { root, .. } => format!("/ingest/{}", api::segment(root)),
            Happened::Build { id, .. } => format!("/builds/{id}"),
            Happened::FilesChanged { audit, .. } => format!("/audits/{audit}"),
            Happened::NewerRevision { .. } => "/dats".to_string(),
        }
    }
}

impl ListRow for MomentRow {
    type Col = MomentCol;
    const COLUMNS: &'static [MomentCol] = &[MomentCol::When, MomentCol::What];

    fn column(col: MomentCol) -> Column {
        match col {
            MomentCol::When => Column::new("when", "When", Width::Fixed(15.0)).sortable(),
            MomentCol::What => Column::new(
                "what",
                "What",
                Width::Flex {
                    min: 13.0,
                    share: 1,
                },
            )
            .sortable(),
        }
    }

    fn key(&self) -> String {
        self.moment.key()
    }

    fn title(&self) -> String {
        self.what()
    }

    fn search(&self) -> String {
        format!("{} {}", self.what(), self.detail())
    }

    fn href(&self) -> Option<String> {
        Some(self.link())
    }

    fn cell(&self, col: MomentCol) -> AnyView {
        match col {
            MomentCol::When => when(&self.moment.at).into_any(),
            MomentCol::What => match self.cause() {
                Some(cause) => view! {
                    <StatusIcon status=cause tab_stop=false />
                    " "
                    {self.what()}
                }
                .into_any(),
                None => self.what().into_any(),
            },
        }
    }

    fn compare(&self, other: &Self, col: MomentCol) -> Ordering {
        match col {
            MomentCol::When => self.moment.at.cmp(&other.moment.at),
            MomentCol::What => self.what().cmp(&other.what()),
        }
    }
}

async fn history(name: String) -> Result<Vec<Moment>, String> {
    let base = format!("/source-sets/{}", api::segment(&name));
    let audits: Vec<AuditSummary> = api::get(&format!("{base}/history")).await?;
    let repairs: Vec<RepairPlan> = api::get(&format!("{base}/repairs")).await?;
    let ingests: Vec<IngestPlan> = api::get(&format!("{base}/ingests")).await?;
    let walks: Vec<StatWalk> = api::get(&format!("{base}/stat-walks")).await?;
    let followed: Vec<FollowedRevision> = api::get(&format!("{base}/followed-revisions")).await?;
    let folders: Vec<LibraryFolder> = api::get("/libraries").await?;
    let mut builds = Vec::new();
    for f in folders.into_iter().filter(|f| f.source_set == name) {
        let path = format!("{}: {}", f.root, f.path);
        let made: Vec<BuildSummary> = api::get(&format!("/libraries/{}/builds", f.id)).await?;
        builds.extend(made.into_iter().map(|b| (path.clone(), b)));
    }
    Ok(model::timeline(
        &audits, &repairs, &ingests, &builds, &walks, &followed,
    ))
}

/// One timeline of the source set's audits, repairs, ingests, builds and what made it stale, newest first.
#[component]
fn History(name: String) -> impl IntoView {
    let live = use_live();
    let name = StoredValue::new(name);
    let moments = LocalResource::new(move || {
        live.settled.track();
        history(name.get_value())
    });
    // A failed refresh keeps the rows last shown; the problem shows above them.
    let rows = last_good_rows(move || {
        moments.get().map(|loaded| {
            loaded.map(|moments| {
                moments
                    .into_iter()
                    .map(|moment| MomentRow {
                        moment,
                        set: name.get_value(),
                    })
                    .collect::<Vec<_>>()
            })
        })
    });
    view! {
        <Help summary="Newest first. Open one to see what it did.">
            <p>"A past audit opens read-only; a pruned audit keeps only its counts."</p>
        </Help>
        {move || match moments.get() {
            None => view! { <Loading what="its history" /> }.into_any(),
            Some(Err(message)) => view! { <Problem message /> }.into_any(),
            Some(Ok(_)) => ().into_any(),
        }}
        <List
            id="set-history"
            label="History"
            rows
            empty="Nothing has happened to this source set yet."
            placeholder="Filter by audit, repair, ingest, build, change or detail"
            row_actions=Callback::new(|row: MomentRow| vec![RowAction::go(Glyph::Eye, "Open", row.link())])
            detail=Callback::new(|row: MomentRow| {
                view! { <p>{row.detail()}</p> }.into_any()
            })
        />
    }
}
