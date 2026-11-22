//! The storage policy: how source sets and play libraries store each game, as TorrentZip, loose, a loose folder or
//! CHD, set globally and for each system, as a selection profile's settings are; and how a disc system's play
//! libraries lay out its discs, whether they put SBI files beside them, and whether builds place games under single
//! copy rather than copying them; and where a MAME system's source sets keep a family's shared ROMs, its set layout.

use std::collections::BTreeSet;

use leptos::prelude::*;
use title_select_api::storage::{
    DiscLayout, ScopePolicy, SetLayout, StorageFormat, StoragePolicy, StorageScope, SystemStorage,
    is_disc_system, is_mame_system,
};
use title_select_api::{Dat, DatFormat};

use crate::api;
use crate::app::{Loading, Problem};

const SCOPES: [(StorageScope, &str); 2] = [
    (StorageScope::SourceSets, "Source sets"),
    (StorageScope::PlayLibraries, "Play libraries"),
];

const DELETE_WARNING: &str = "With this on, a conversion deletes each original as soon as its converted file is verified, instead of keeping it in kept files. You can't get a deleted original back from Title Select. An original that changed, or whose converted file doesn't verify, is always kept.";

const CHD_NOTE: &str = "As CHD, a build makes each disc's CHD with chdman, extracts it once and checks every track against the DAT before it puts it in place. A disc whose CHD doesn't give back the DAT's files is never put in place. A disc that isn't one .cue with its .bin tracks, or one .iso, stays TorrentZip, with every other disc of its game.";

const LAYOUT_NOTE: &str = "In the ES-DE layout, a disc stored as one file, a CHD or an ISO, goes directly in the system folder; a disc stored as a .cue with its tracks goes in a folder named exactly for its .cue; and a game on several discs goes in a folder named for its .m3u, which is named for the game without its disc number and lists each disc in order, with every file of every disc beside it. In the flat layout every file goes directly in the system folder, with no .m3u. A disc stored as TorrentZip is one ZIP in the system folder in either layout.";

const SBI_NOTE: &str = "With this on, a build puts each disc's SBI file, from the system's latest SBI pack, beside its .cue or .chd, named for it. Emulators read it to play discs whose copy protection they otherwise can't.";

const SINGLE_COPY_NOTE: &str = "With this on, a build places each selected game in the library folder that holds your source set's placed games instead of copying it there: each game is kept once, in your library folder, and taken out of your source set. Where your source set and that folder share a filesystem, a game is renamed into place; otherwise it is copied, read back and its original deleted. Its file in your library folder is then the game's only copy: deleting it by hand loses the game, and your own backups are the only other copy. Turning this off later makes the next plan return every placed game to your source set. For MAME's ListXML, it places a game's ZIP only from a non-merged source set: a split or merged ZIP holds more than one game's ROMs, so under those layouts every game is copied. A game's disks are always copied, never placed.";

const PLACE_CONVERTED_NOTE: &str = "With this and single copy on, a game your play library stores as CHD, a format your source set doesn't hold, is placed too, instead of copied. The CHD is then the game's only copy, so a full audit extracts it to check each track, which takes longer than reading a file. Title Select keeps the disc's .cue sheet, exactly as the DAT lists it, and writes it back when the game returns to your source set.";

const DAMAGED_PLACED_NOTE: &str = "Because the CHD is your only copy, a damaged one can't be fixed where it is: a repair sets it aside in your source set's kept files, where you can delete it, and the game is missing. With a good copy in an ingest folder, the next repair after the audit that repair queues rebuilds it in your source set. A copy of a healthy disc found in an ingest folder while its CHD was placed counts as already held, so scan that folder again after the audit. A game of several discs is set aside whole, every disc's CHD with it, never one disc alone.";

/// What placing converted games means, with what a repair does with a damaged CHD, its only copy, folded beneath.
#[component]
fn PlaceConvertedNote() -> impl IntoView {
    view! {
        <p class="note small">{PLACE_CONVERTED_NOTE}</p>
        <details class="note small">
            <summary>"If a placed CHD is damaged"</summary>
            <p>{DAMAGED_PLACED_NOTE}</p>
        </details>
    }
}

const SET_LAYOUT_NOTE: &str = "The set layout says where a MAME source set keeps the ROMs a family shares. Non-merged, the default, gives each game one ZIP holding everything it needs, its BIOS and device ROMs included: it takes the most space, and every ZIP can be copied alone. Split keeps what a clone shares with its parent in the parent's ZIP, and BIOS and device ROMs in their own ZIPs. Merged keeps a whole family in its parent's ZIP, and a clone has no ZIP of its own. An audit accepts any of them. The next repair proposes repacking each family into the layout you choose, and sets a ZIP aside only once every copy written from it is verified. Disks stay in a folder named for the family's parent whatever the layout. A build copies a non-merged ZIP as it is into your play library, and repacks a split or merged family into each selected game's own ZIP, with its disks copied into a folder named for it. It applies to arcade DATs, MAME's ListXML, not to software lists.";

fn set_layout_label(layout: SetLayout) -> &'static str {
    match layout {
        SetLayout::NonMerged => "Non-merged: each ZIP holds everything its game needs",
        SetLayout::Split => "Split: a clone's ZIP holds only what it doesn't share",
        SetLayout::Merged => "Merged: a family in its parent's ZIP",
    }
}

const LAYOUTS: [DiscLayout; 2] = [DiscLayout::EsDe, DiscLayout::Flat];

fn layout_label(layout: DiscLayout) -> &'static str {
    match layout {
        DiscLayout::EsDe => "ES-DE: .cue and .m3u folders",
        DiscLayout::Flat => "Flat: every file in the system folder",
    }
}

fn scope_mut(policy: &mut StoragePolicy, scope: StorageScope) -> &mut ScopePolicy {
    match scope {
        StorageScope::SourceSets => &mut policy.source_sets,
        StorageScope::PlayLibraries => &mut policy.play_libraries,
    }
}

fn format_label(format: StorageFormat) -> &'static str {
    match format {
        StorageFormat::TorrentZip => "TorrentZip",
        StorageFormat::Loose => "Loose, for games with one ROM",
        StorageFormat::LooseFolder => "Loose folder: a folder per game",
        StorageFormat::Chd => "CHD: one per disc",
    }
}

/// The formats you can choose for folders of `scope`, globally or for `system`: only a disc system's play libraries
/// store a disc's loose files, laid out as their disc layout says, or CHD.
fn formats(scope: StorageScope, system: Option<&str>) -> &'static [StorageFormat] {
    match scope {
        StorageScope::SourceSets => &[
            StorageFormat::TorrentZip,
            StorageFormat::Loose,
            StorageFormat::LooseFolder,
        ],
        StorageScope::PlayLibraries if system.is_some_and(is_disc_system) => &[
            StorageFormat::TorrentZip,
            StorageFormat::Loose,
            StorageFormat::LooseFolder,
            StorageFormat::Chd,
        ],
        StorageScope::PlayLibraries => &[StorageFormat::TorrentZip, StorageFormat::Loose],
    }
}

/// How `format` reads for folders of `scope` bound to `system`: a disc system's play libraries lay a disc's loose
/// files out as their disc layout says, not in a folder per game.
fn format_label_for(scope: StorageScope, system: &str, format: StorageFormat) -> &'static str {
    match format {
        StorageFormat::LooseFolder
            if scope == StorageScope::PlayLibraries && is_disc_system(system) =>
        {
            "Loose files: each disc's .cue and tracks, as the disc layout says"
        }
        _ => format_label(format),
    }
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

/// Changes `system`'s overrides in `scope`, removing them once they override nothing.
fn update_own(
    policy: RwSignal<StoragePolicy>,
    scope: StorageScope,
    system: &str,
    f: impl FnOnce(&mut SystemStorage),
) {
    policy.update(|p| {
        let systems = &mut scope_mut(p, scope).systems;
        f(systems.entry(system.to_owned()).or_default());
        if systems.get(system).is_some_and(SystemStorage::is_empty) {
            systems.remove(system);
        }
    });
}

#[component]
pub fn StoragePolicyScreen() -> impl IntoView {
    let loaded = LocalResource::new(|| async {
        let policy = api::get::<StoragePolicy>("/storage-policy").await?;
        let dats = api::get::<Vec<Dat>>("/dats").await?;
        Ok::<_, String>((policy, dats))
    });
    view! {
        <h1>"Storage policy"</h1>
        <p class="lede">
            "The storage policy says how each folder stores a game: as one TorrentZip per game; loose, as the ROM itself under the name the DAT gives it; as a loose folder, a folder named for the game holding each of its ROMs loose; or, for a disc system's play libraries, as one CHD per disc. A disc system's play libraries lay out its discs' files as their disc layout says. Source sets and play libraries each have a global setting, and you can set a system differently."
        </p>
        {move || match loaded.get() {
            None => view! { <Loading what="the storage policy" /> }.into_any(),
            Some(Err(message)) => view! { <Problem message /> }.into_any(),
            Some(Ok((policy, dats))) => {
                let arcade: BTreeSet<String> = dats
                    .iter()
                    .filter(|d| d.format == DatFormat::Listxml && is_mame_system(&d.dat))
                    .map(|d| d.dat.clone())
                    .collect();
                let systems = dats.into_iter().map(|d| d.dat).collect::<Vec<_>>();
                view! { <Editor initial=policy systems arcade /> }.into_any()
            }
        }}
    }
}

#[component]
fn Editor(initial: StoragePolicy, systems: Vec<String>, arcade: BTreeSet<String>) -> impl IntoView {
    let arcade = StoredValue::new(arcade);
    let saved = RwSignal::new(initial.clone());
    let policy = RwSignal::new(initial);
    let dirty = Memo::new(move |_| policy.with(|p| saved.with(|s| p != s)));
    let systems = Memo::new(move |_| {
        let mut all: BTreeSet<String> = systems.iter().cloned().collect();
        policy.with(|p| {
            all.extend(p.source_sets.systems.keys().cloned());
            all.extend(p.play_libraries.systems.keys().cloned());
        });
        all.into_iter().collect::<Vec<_>>()
    });
    let tab = RwSignal::new(None::<String>);
    let outcome = RwSignal::new(None::<Result<(), String>>);
    let saving = RwSignal::new(false);
    let save = move |_| {
        let body = policy.get_untracked();
        saving.set(true);
        leptos::task::spawn_local(async move {
            let result = api::put::<_, StoragePolicy>("/storage-policy", &body).await;
            saving.set(false);
            match result {
                Ok(done) => {
                    saved.set(done.clone());
                    policy.set(done);
                    outcome.set(Some(Ok(())));
                }
                Err(message) => outcome.set(Some(Err(message))),
            }
        });
    };
    view! {
        <div class="tabs" role="tablist" aria-label="Storage settings">
            <Tab tab which=None label="Global settings".to_string() />
            {move || {
                systems
                    .get()
                    .into_iter()
                    .map(|system| view! { <Tab tab which=Some(system.clone()) label=system /> })
                    .collect_view()
            }}
        </div>
        {move || match tab.get() {
            None => view! { <GlobalSettings policy /> }.into_any(),
            Some(system) => {
                let arcade = arcade.with_value(|a| a.contains(&system));
                view! { <SystemSettings policy system arcade /> }.into_any()
            }
        }}
        <section class="staged" aria-labelledby="save-title">
            <h2 id="save-title">"Save"</h2>
            <p class="note">
                "Saving changes no file. The next plan for each folder whose format changed proposes converting its games, and the next plan for each folder whose single copy setting changed proposes placing or returning them, and the next repair of each MAME source set whose set layout changed proposes repacking its families. Nothing is converted, repacked, placed or returned until you approve that plan. Each original goes to kept files unless delete originals after conversion is on."
            </p>
            <div class="actions">
                <button
                    type="button"
                    class="primary"
                    disabled=move || !dirty.get() || saving.get()
                    on:click=save
                >
                    {move || if saving.get() { "Saving…" } else { "Save the storage policy" }}
                </button>
                <button
                    type="button"
                    disabled=move || !dirty.get() || saving.get()
                    on:click=move |_| policy.set(saved.get_untracked())
                >
                    "Discard changes"
                </button>
            </div>
            {move || match outcome.get() {
                Some(Ok(())) if !dirty.get() => {
                    view! { <p class="done" role="status">"Saved the storage policy."</p> }.into_any()
                }
                Some(Err(message)) => view! { <Problem message /> }.into_any(),
                _ => ().into_any(),
            }}
        </section>
    }
}

#[component]
fn Tab(tab: RwSignal<Option<String>>, which: Option<String>, label: String) -> impl IntoView {
    let which = StoredValue::new(which);
    let selected = move || tab.with(|t| *t == which.get_value());
    view! {
        <button
            type="button"
            role="tab"
            aria-selected=move || selected().to_string()
            on:click=move |_| tab.set(which.get_value())
        >
            {label}
        </button>
    }
}

#[component]
fn GlobalSettings(policy: RwSignal<StoragePolicy>) -> impl IntoView {
    view! {
        <div class="settings" role="tabpanel" aria-label="Global storage settings">
            <p class="lede">
                "These apply to every system you haven't set differently. Stored loose, a game with more than one ROM stays TorrentZip, and a plan says so. Source sets of disc systems, those whose DAT Redump publishes, are stored as loose folders unless you set the system differently."
            </p>
            {SCOPES
                .iter()
                .map(|&(scope, legend)| {
                    let id = scope.as_str();
                    view! {
                        <fieldset class="setting">
                            <legend>{legend}</legend>
                            <label class="stacked" for=format!("g-{id}-format")>
                                "Format"
                            </label>
                            <select
                                id=format!("g-{id}-format")
                                prop:value=move || policy.with(|p| p.scope(scope).format.as_str())
                                on:change=move |ev| {
                                    if let Some(f) = StorageFormat::parse(&event_target_value(&ev)) {
                                        policy.update(|p| scope_mut(p, scope).format = f);
                                    }
                                }
                            >
                                {formats(scope, None)
                                    .iter()
                                    .map(|&f| view! { <option value=f.as_str()>{format_label(f)}</option> })
                                    .collect_view()}
                            </select>
                            <p>
                                <label class="inline">
                                    <input
                                        type="checkbox"
                                        prop:checked=move || policy.with(|p| p.scope(scope).delete_originals)
                                        on:change=move |ev| {
                                            let on = event_target_checked(&ev);
                                            policy.update(|p| scope_mut(p, scope).delete_originals = on);
                                        }
                                    />
                                    "Delete originals after conversion"
                                </label>
                            </p>
                            <p class="note small">{DELETE_WARNING}</p>
                            {(scope == StorageScope::SourceSets)
                                .then(|| {
                                    view! {
                                        <label class="stacked" for="g-source_sets-set-layout">
                                            "Set layout, for MAME systems"
                                        </label>
                                        <select
                                            id="g-source_sets-set-layout"
                                            prop:value=move || policy.with(|p| p.source_sets.set_layout.as_str())
                                            on:change=move |ev| {
                                                if let Some(l) = SetLayout::parse(&event_target_value(&ev)) {
                                                    policy.update(|p| p.source_sets.set_layout = l);
                                                }
                                            }
                                        >
                                            {SetLayout::ALL
                                                .iter()
                                                .map(|&l| view! { <option value=l.as_str()>{set_layout_label(l)}</option> })
                                                .collect_view()}
                                        </select>
                                        <p class="note small">{SET_LAYOUT_NOTE}</p>
                                    }
                                })}
                            {(scope == StorageScope::PlayLibraries).then(|| view! { <GlobalSingleCopy policy /> })}
                            {(scope == StorageScope::PlayLibraries)
                                .then(|| {
                                    view! {
                                        <label class="stacked" for="g-play_libraries-layout">
                                            "Disc layout, for disc systems"
                                        </label>
                                        <select
                                            id="g-play_libraries-layout"
                                            prop:value=move || policy.with(|p| p.play_libraries.layout.as_str())
                                            on:change=move |ev| {
                                                if let Some(l) = DiscLayout::parse(&event_target_value(&ev)) {
                                                    policy.update(|p| p.play_libraries.layout = l);
                                                }
                                            }
                                        >
                                            {LAYOUTS
                                                .iter()
                                                .map(|&l| view! { <option value=l.as_str()>{layout_label(l)}</option> })
                                                .collect_view()}
                                        </select>
                                        <p class="note small">{LAYOUT_NOTE}</p>
                                    }
                                })}
                        </fieldset>
                    }
                })
                .collect_view()}
        </div>
    }
}

#[component]
fn SystemSettings(policy: RwSignal<StoragePolicy>, system: String, arcade: bool) -> impl IntoView {
    let s = StoredValue::new(system.clone());
    let has_own = move || {
        policy.with(|p| {
            SCOPES
                .iter()
                .any(|(scope, _)| p.scope(*scope).systems.contains_key(&s.get_value()))
        })
    };
    view! {
        <div class="settings" role="tabpanel" aria-label=format!("Storage settings for {system}")>
            <p class="lede">
                {move || {
                    if has_own() {
                        "Some settings are set for this system; the rest come from your global settings."
                    } else {
                        "Every setting comes from your global settings. Set one for this system to change it here only."
                    }
                }}
            </p>
            <div class="actions">
                <button
                    type="button"
                    disabled=move || !has_own()
                    on:click=move |_| {
                        policy.update(|p| {
                            for (scope, _) in SCOPES {
                                scope_mut(p, scope).systems.remove(&s.get_value());
                            }
                        })
                    }
                >
                    "Use global settings for this system"
                </button>
            </div>
            {SCOPES
                .iter()
                .map(|&(scope, legend)| {
                    let id = scope.as_str();
                    let own = move || {
                        policy.with(|p| p.scope(scope).systems.get(&s.get_value()).cloned().unwrap_or_default())
                    };
                    // A disc system's source sets follow the loose folder default, not the global format.
                    let global = move || {
                        policy.with(|p| {
                            let format = match scope {
                                StorageScope::SourceSets if is_disc_system(&s.get_value()) => {
                                    StorageFormat::LooseFolder
                                }
                                _ => p.scope(scope).format,
                            };
                            (format, p.scope(scope).delete_originals)
                        })
                    };
                    view! {
                        <fieldset class="setting">
                            <legend>{legend}</legend>
                            <label class="stacked" for=format!("s-{id}-format")>
                                "Format"
                            </label>
                            <select
                                id=format!("s-{id}-format")
                                prop:value=move || own().format.map_or("global", StorageFormat::as_str)
                                on:change=move |ev| {
                                    let f = StorageFormat::parse(&event_target_value(&ev));
                                    update_own(policy, scope, &s.get_value(), |o| o.format = f);
                                }
                            >
                                <option value="global">
                                    {move || {
                                        if scope == StorageScope::SourceSets && is_disc_system(&s.get_value()) {
                                            format!("Disc system default: {}", format_label(global().0))
                                        } else {
                                            format!("Global setting: {}", format_label(global().0))
                                        }
                                    }}
                                </option>
                                {formats(scope, Some(&s.get_value()))
                                    .iter()
                                    .map(|&f| {
                                        view! {
                                            <option value=f.as_str()>
                                                {format_label_for(scope, &s.get_value(), f)}
                                            </option>
                                        }
                                    })
                                    .collect_view()}
                            </select>
                            {(scope == StorageScope::PlayLibraries && is_disc_system(&s.get_value()))
                                .then(|| view! { <p class="note small">{CHD_NOTE}</p> })}
                            <label class="stacked" for=format!("s-{id}-delete")>
                                "Delete originals after conversion"
                            </label>
                            <select
                                id=format!("s-{id}-delete")
                                prop:value=move || match own().delete_originals {
                                    None => "global",
                                    Some(true) => "on",
                                    Some(false) => "off",
                                }
                                on:change=move |ev| {
                                    let v = match event_target_value(&ev).as_str() {
                                        "on" => Some(true),
                                        "off" => Some(false),
                                        _ => None,
                                    };
                                    update_own(policy, scope, &s.get_value(), |o| o.delete_originals = v);
                                }
                            >
                                <option value="global">
                                    {move || format!("Global setting: {}", on_off(global().1))}
                                </option>
                                <option value="off">"Off: keep each original in kept files"</option>
                                <option value="on">"On: delete each original once its converted file is verified"</option>
                            </select>
                            <p class="note small">{DELETE_WARNING}</p>
                            // One saved for a system whose DAT isn't an arcade DAT shows too, so you can clear it.
                            {move || {
                                let set = own().set_layout.is_some();
                                (scope == StorageScope::SourceSets && (arcade || set))
                                    .then(|| view! { <SystemSetLayout policy system=s.get_value() /> })
                            }}
                            {(scope == StorageScope::PlayLibraries)
                                .then(|| view! { <SystemSingleCopy policy system=s.get_value() /> })}
                            {(scope == StorageScope::PlayLibraries && is_disc_system(&s.get_value()))
                                .then(|| view! { <DiscSettings policy system=s.get_value() /> })}
                        </fieldset>
                    }
                })
                .collect_view()}
        </div>
    }
}

/// A disc system's play library settings: its disc layout, and whether SBI files go beside its discs.
#[component]
fn DiscSettings(policy: RwSignal<StoragePolicy>, system: String) -> impl IntoView {
    let s = StoredValue::new(system);
    let scope = StorageScope::PlayLibraries;
    let own = move || {
        policy.with(|p| {
            p.play_libraries
                .systems
                .get(&s.get_value())
                .cloned()
                .unwrap_or_default()
        })
    };
    view! {
        <label class="stacked" for="s-play_libraries-layout">
            "Disc layout"
        </label>
        <select
            id="s-play_libraries-layout"
            prop:value=move || own().layout.map_or("global", DiscLayout::as_str)
            on:change=move |ev| {
                let l = DiscLayout::parse(&event_target_value(&ev));
                update_own(policy, scope, &s.get_value(), |o| o.layout = l);
            }
        >
            <option value="global">
                {move || {
                    format!(
                        "Global setting: {}",
                        layout_label(policy.with(|p| p.play_libraries.layout)),
                    )
                }}
            </option>
            {LAYOUTS
                .iter()
                .map(|&l| view! { <option value=l.as_str()>{layout_label(l)}</option> })
                .collect_view()}
        </select>
        <p class="note small">{LAYOUT_NOTE}</p>
        <p>
            <label class="inline">
                <input
                    type="checkbox"
                    prop:checked=move || own().sbi == Some(true)
                    on:change=move |ev| {
                        let on = event_target_checked(&ev).then_some(true);
                        update_own(policy, scope, &s.get_value(), |o| o.sbi = on);
                    }
                />
                "Put SBI files beside discs"
            </label>
        </p>
        <p class="note small">{SBI_NOTE}</p>
    }
}

/// A MAME system's own set layout for its source sets: shown only for a system whose DAT is an arcade DAT.
#[component]
fn SystemSetLayout(policy: RwSignal<StoragePolicy>, system: String) -> impl IntoView {
    let s = StoredValue::new(system);
    let scope = StorageScope::SourceSets;
    let own = move || {
        policy.with(|p| {
            p.source_sets
                .systems
                .get(&s.get_value())
                .and_then(|o| o.set_layout)
        })
    };
    view! {
        <label class="stacked" for="s-source_sets-set-layout">
            "Set layout"
        </label>
        <select
            id="s-source_sets-set-layout"
            prop:value=move || own().map_or("global", SetLayout::as_str)
            on:change=move |ev| {
                let l = SetLayout::parse(&event_target_value(&ev));
                update_own(policy, scope, &s.get_value(), |o| o.set_layout = l);
            }
        >
            <option value="global">
                {move || {
                    format!(
                        "Global setting: {}",
                        set_layout_label(policy.with(|p| p.source_sets.set_layout)),
                    )
                }}
            </option>
            {SetLayout::ALL
                .iter()
                .map(|&l| view! { <option value=l.as_str()>{set_layout_label(l)}</option> })
                .collect_view()}
        </select>
        <p class="note small">{SET_LAYOUT_NOTE}</p>
    }
}

/// The play libraries' global single copy settings: whether builds place games, and games stored as CHD too.
#[component]
fn GlobalSingleCopy(policy: RwSignal<StoragePolicy>) -> impl IntoView {
    let single = move || policy.with(|p| p.play_libraries.single_copy);
    view! {
        <p>
            <label class="inline">
                <input
                    type="checkbox"
                    prop:checked=single
                    on:change=move |ev| {
                        let on = event_target_checked(&ev);
                        policy.update(|p| p.play_libraries.single_copy = on);
                    }
                />
                "Single copy"
            </label>
        </p>
        <p class="note small">{SINGLE_COPY_NOTE}</p>
        <p>
            <label class="inline">
                <input
                    type="checkbox"
                    prop:checked=move || policy.with(|p| p.play_libraries.place_converted)
                    on:change=move |ev| {
                        let on = event_target_checked(&ev);
                        policy.update(|p| p.play_libraries.place_converted = on);
                    }
                />
                "Place converted games"
            </label>
        </p>
        <PlaceConvertedNote/>
    }
}

/// Why single copy places nothing for MAME `system`'s play libraries under `policy`: its source sets use the split or
/// merged set layout, whose ZIPs each hold more than one game's ROMs. None where it can place games.
fn single_copy_unavailable(policy: &StoragePolicy, system: &str) -> Option<String> {
    let layout = policy
        .of(StorageScope::SourceSets, system)
        .set_layout
        .filter(|l| !l.is_self_contained())?;
    Some(format!(
        "Single copy has no effect for this system: its source sets use the {} set layout, whose ZIPs each hold more \
         than one game's ROMs, so every game is copied. Choose non-merged for its source sets to place its games.",
        layout.label()
    ))
}

/// A system's own single copy settings for its play libraries; placing converted games only for a disc system, the
/// only ones stored as CHD.
#[component]
fn SystemSingleCopy(policy: RwSignal<StoragePolicy>, system: String) -> impl IntoView {
    let s = StoredValue::new(system);
    let scope = StorageScope::PlayLibraries;
    let own = move || {
        policy.with(|p| {
            p.play_libraries
                .systems
                .get(&s.get_value())
                .cloned()
                .unwrap_or_default()
        })
    };
    let global = move || {
        policy.with(|p| {
            (
                p.play_libraries.single_copy,
                p.play_libraries.place_converted,
            )
        })
    };
    let choice = |v: Option<bool>| match v {
        None => "global",
        Some(true) => "on",
        Some(false) => "off",
    };
    let chosen = |ev: leptos::ev::Event| match event_target_value(&ev).as_str() {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    };
    view! {
        <label class="stacked" for="s-play_libraries-single-copy">
            "Single copy"
        </label>
        <select
            id="s-play_libraries-single-copy"
            prop:value=move || choice(own().single_copy)
            on:change=move |ev| {
                let v = chosen(ev);
                update_own(policy, scope, &s.get_value(), |o| o.single_copy = v);
            }
        >
            <option value="global">{move || format!("Global setting: {}", on_off(global().0))}</option>
            <option value="off">"Off: copy each game"</option>
            <option value="on">"On: place each game as its only copy"</option>
        </select>
        <p class="note small">
            {move || {
                policy
                    .with(|p| single_copy_unavailable(p, &s.get_value()))
                    .unwrap_or_else(|| SINGLE_COPY_NOTE.to_string())
            }}
        </p>
        {is_disc_system(&s.get_value())
            .then(|| {
                view! {
                    <label class="stacked" for="s-play_libraries-place-converted">
                        "Place converted games"
                    </label>
                    <select
                        id="s-play_libraries-place-converted"
                        prop:value=move || choice(own().place_converted)
                        on:change=move |ev| {
                            let v = chosen(ev);
                            update_own(policy, scope, &s.get_value(), |o| o.place_converted = v);
                        }
                    >
                        <option value="global">{move || format!("Global setting: {}", on_off(global().1))}</option>
                        <option value="off">"Off: copy games stored as CHD"</option>
                        <option value="on">"On: place games stored as CHD too"</option>
                    </select>
                    <PlaceConvertedNote/>
                }
            })}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The set layout note names each layout as you choose it, and says when a repair sets a ZIP aside.
    #[test]
    fn the_set_layout_note_names_each_layout_and_when_a_zip_is_set_aside() {
        for said in [
            "Non-merged, the default",
            "Split keeps",
            "Merged keeps",
            "An audit accepts any of them",
            "sets a ZIP aside only once every copy written from it is verified",
            "not to software lists",
        ] {
            assert!(SET_LAYOUT_NOTE.contains(said), "{said}");
        }
        for layout in SetLayout::ALL {
            let label = set_layout_label(layout).to_lowercase();
            assert!(label.starts_with(layout.label()), "{label}");
        }
    }

    /// Single copy says which MAME source sets it places games from, and that disks are always copied.
    #[test]
    fn the_single_copy_note_says_it_places_mame_games_only_from_a_non_merged_source_set() {
        for said in [
            "only from a non-merged source set",
            "under those layouts every game is copied",
            "disks are always copied, never placed",
        ] {
            assert!(SINGLE_COPY_NOTE.contains(said), "{said}");
        }
    }

    /// A MAME system's single copy has no effect, and says why, while its source sets aren't non-merged.
    #[test]
    fn single_copy_has_no_effect_for_mame_unless_its_source_sets_are_non_merged() {
        let mut policy = StoragePolicy::default();
        assert_eq!(single_copy_unavailable(&policy, "MAME/MAME"), None);
        policy.source_sets.set_layout = SetLayout::Merged;
        let why = single_copy_unavailable(&policy, "MAME/MAME").unwrap();
        assert!(why.contains("use the merged set layout"), "{why}");
        assert_eq!(
            single_copy_unavailable(&policy, "No-Intro/Atari - Atari 2600"),
            None
        );
    }

    /// Placing converted games makes a CHD the only copy: the note says what a repair does with a damaged one.
    #[test]
    fn the_place_converted_note_states_the_only_copy_risk() {
        for said in [
            "the game's only copy",
            "a damaged one can't be fixed where it is",
            "sets it aside in your source set's kept files",
            "With a good copy in an ingest folder, the next repair after the audit that repair queues rebuilds it",
            "every disc's CHD with it",
            "scan that folder again after the audit",
        ] {
            assert!(
                PLACE_CONVERTED_NOTE.contains(said) || DAMAGED_PLACED_NOTE.contains(said),
                "{said}"
            );
        }
    }
}
