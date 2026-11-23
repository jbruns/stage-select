//! A Library Folder's RomM tab: the latest comparison of RomM's view of the folder with what its Builds made (#143),
//! comparing it again, and asking RomM to scan a flagged ROM again. Stage Select never changes what RomM matched.

use std::cmp::Ordering;

use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use stage_select_api::romm::{
    RommFlag, RommReconcileRow, RommReconciliation, RommRescan, RommRescanRequest,
};

use crate::api;
use crate::app::Problem;
use crate::events::use_live;
use crate::model::when;
use crate::parts::icon::Glyph;
use crate::parts::legend::{Legend, present};
use crate::parts::list::{Clip, Column, List, ListRow, RowAction, Width, last_good_rows};
use crate::parts::pending::ActionPending;
use crate::parts::status::{StatusBadge, StatusCount, StatusIcon, highest};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RommCol {
    Status,
    File,
    Game,
    Romm,
}

/// A file a build made that RomM sees otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RommRow(RommReconcileRow);

impl RommRow {
    fn flag(&self) -> RommFlag {
        highest(self.0.flags.iter().copied()).unwrap_or(RommFlag::HashMismatch)
    }
}

impl ListRow for RommRow {
    type Col = RommCol;
    const COLUMNS: &'static [RommCol] =
        &[RommCol::Status, RommCol::File, RommCol::Game, RommCol::Romm];

    fn column(col: RommCol) -> Column {
        match col {
            RommCol::Status => Column::new("status", "", Width::Fixed(2.5)).sortable(),
            RommCol::File => Column::new(
                "file",
                "File",
                Width::Flex {
                    min: 10.0,
                    share: 3,
                },
            )
            .sortable(),
            RommCol::Game => Column::new("game", "Game", Width::Flex { min: 8.0, share: 2 })
                .sortable()
                .hint("The game the build made it for"),
            RommCol::Romm => Column::new("romm", "RomM's name", Width::Flex { min: 8.0, share: 2 })
                .sortable()
                .hint("The name RomM matched it to, if any"),
        }
    }

    fn key(&self) -> String {
        self.0.target.clone()
    }

    fn title(&self) -> String {
        self.0.target.clone()
    }

    fn search(&self) -> String {
        format!(
            "{} {} {}",
            self.0.target,
            self.0.game,
            self.0.romm_name.as_deref().unwrap_or_default()
        )
    }

    fn cell(&self, col: RommCol) -> AnyView {
        match col {
            RommCol::Status => {
                view! { <StatusIcon status=self.flag() tab_stop=false /> }.into_any()
            }
            RommCol::File => view! { <Clip text=self.0.target.clone() /> }.into_any(),
            RommCol::Game => view! { <Clip text=self.0.game.clone() /> }.into_any(),
            RommCol::Romm => {
                view! { <Clip text=self.0.romm_name.clone().unwrap_or_default() /> }.into_any()
            }
        }
    }

    fn compare(&self, other: &Self, col: RommCol) -> Ordering {
        match col {
            RommCol::Status => self.0.flags.cmp(&other.0.flags),
            RommCol::File => self.0.target.cmp(&other.0.target),
            RommCol::Game => self.0.game.cmp(&other.0.game),
            RommCol::Romm => self.0.romm_name.cmp(&other.0.romm_name),
        }
    }
}

fn detail(row: RommRow) -> AnyView {
    let r = row.0;
    view! {
        <ul class="plain">
            {r.flags.iter().map(|&flag| view! { <li><StatusBadge status=flag /></li> }).collect_view()}
        </ul>
        {r.detail.map(|d| view! { <p>{d}</p> })}
        <dl>
            <dt>"Game"</dt>
            <dd>{r.game}</dd>
            <dt>"RomM's name"</dt>
            <dd>{r.romm_name.unwrap_or_else(|| "None".into())}</dd>
            <dt>"RomM's ROM"</dt>
            <dd>{r.rom_id.map(|id| format!("#{id}")).unwrap_or_else(|| "None listed".into())}</dd>
        </dl>
    }
    .into_any()
}

/// The folder's latest comparison with RomM: how many files agree, each one that doesn't, and why.
#[component]
pub fn RommTab(folder: i64) -> impl IntoView {
    let live = use_live();
    let path = format!("/libraries/{folder}/romm");
    let compare = Action::new_local(move |_: &()| async move {
        api::post::<_, RommReconciliation>(
            &format!("/libraries/{folder}/romm/reconciliations"),
            &(),
        )
        .await
    });
    let rescan = Action::new_local(move |rom_id: &i64| {
        let body = RommRescanRequest { rom_id: *rom_id };
        async move {
            api::post::<_, RommRescan>(&format!("/libraries/{folder}/romm/rescans"), &body).await
        }
    });
    let reconciled = Memo::new(move |_| live.romm.with(|m| m.get(&folder).copied()));
    let loaded = LocalResource::new(move || {
        live.settled.track();
        reconciled.track();
        compare.version().track();
        let path = path.clone();
        async move { Ok::<_, String>(api::get::<RommReconciliation>(&path).await.ok()) }
    });
    let latest = Signal::derive(move || loaded.get().and_then(Result::ok).flatten());
    let rows = last_good_rows(move || {
        loaded.get().map(|r| {
            r.map(|latest| {
                latest
                    .map(|l| l.rows.into_iter().map(RommRow).collect())
                    .unwrap_or_default()
            })
        })
    });
    let statuses = Signal::derive(move || {
        present(rows.with(|r| r.iter().map(RommRow::flag).collect::<Vec<_>>()))
    });
    let summary = move || {
        latest.get().map(|l| {
            let c = l.counts;
            view! {
                <p class="note">
                    {format!("Compared with RomM's platform {} {}", l.fs_slug, when(&l.reconciled_at))}
                    {l.job_id.map(|job| format!(", once the scan job #{job} asked for finished"))}
                    "."
                </p>
                {l.error.map(|message| view! { <Problem message /> })}
                <ul class="counts">
                    <li class="count">
                        <span class="n">{c.files}</span>
                        <span class="what">"Files compared"</span>
                    </li>
                    <li class="count">
                        <span class="n">{c.clean}</span>
                        <span class="what">"Agree"</span>
                    </li>
                    <StatusCount status=RommFlag::Unidentified>{c.unidentified}</StatusCount>
                    <StatusCount status=RommFlag::TitleMismatch>{c.title_mismatch}</StatusCount>
                    <StatusCount status=RommFlag::HashMismatch>{c.hash_mismatch}</StatusCount>
                </ul>
            }
        })
    };
    let rescanned = move || {
        match rescan.value().get() {
        Some(Ok(r)) => Some(
            view! {
                <p class="note">
                    {format!(
                        "RomM is scanning ROM #{} again ({} scan, task {}). Compare again once it ends.",
                        r.rom_id,
                        r.scan_type,
                        r.task_id,
                    )}
                </p>
            }
            .into_any(),
        ),
        Some(Err(message)) => Some(view! { <Problem message /> }.into_any()),
        None => None,
    }
    };
    view! {
        <section aria-labelledby="romm-title">
            <h2 id="romm-title">"RomM"</h2>
            <p class="lede">
                "Once RomM finishes the scan a build asks for, its view of this folder is compared with the files your \
                builds made. RomM's matches are never changed here: fix them in RomM, or ask it to scan a file again."
            </p>
            <div class="form">
                <button type="button" disabled=move || compare.pending().get() on:click=move |_| { compare.dispatch(()); }>
                    "Compare again"
                </button>
            </div>
            <ActionPending action=compare status="Comparing with RomM…">
                {summary}
                {move || {
                    (loaded.get().is_some() && latest.get().is_none())
                        .then(|| view! { <p class="note">"RomM's view of this folder wasn't compared yet."</p> })
                }}
                <List
                    id="romm"
                    label="Files RomM sees otherwise"
                    rows
                    empty="RomM agrees with every file your builds made."
                    placeholder="Filter by file, game or RomM's name"
                    row_actions=Callback::new(move |row: RommRow| {
                        row.0
                            .rom_id
                            .map(|id| {
                                let label = format!("Ask RomM to scan {} again", row.0.target);
                                vec![RowAction::run(Glyph::RefreshCw, label, move || { rescan.dispatch(id); })]
                            })
                            .unwrap_or_default()
                    })
                    detail=Callback::new(detail)
                    legend=move || view! { <Legend statuses list="romm" /> }
                />
            </ActionPending>
            {rescanned}
        </section>
    }
}
