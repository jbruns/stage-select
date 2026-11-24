//! A folder browser beside a path field: `FolderPicker` puts a **Browse…** button there that opens a drawer listing
//! the folders the Service can see (`GET /folders`), from `/` or from within a Root, and returns the folder you
//! choose into the field. The pure parts (where it starts, the trail of crumbs, going up, what it returns) are
//! host-tested here.
//!
//! When the field's path can't be listed, the picker says why and starts at the top: that is where a folder that
//! doesn't exist yet would be offered to be made.

use leptos::html;
use leptos::prelude::*;
use stage_select_api::roots::{Folder, FolderListing};
use wasm_bindgen::JsCast;

use super::icon::{Glyph, Svg};
use super::list_model::{Step, step};
use super::message::{Live, Message, MessageKind, Problem};
use super::overlay::Drawer;
use crate::api;

/// The folder a picker is bound to: a Root's, named `label`. The picker goes no higher than `path`, and returns
/// paths relative to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Within {
    pub path: String,
    pub label: String,
}

impl Within {
    pub fn new(path: impl Into<String>, label: impl Into<String>) -> Within {
        Within {
            path: tidy(&path.into()),
            label: label.into(),
        }
    }
}

/// One crumb of the trail to the folder shown: its name and the folder it opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crumb {
    pub label: String,
    pub path: String,
}

/// `path` without repeated, `.` or trailing slashes; `/` stays `/`.
pub fn tidy(path: &str) -> String {
    let parts: Vec<&str> = path
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    format!("/{}", parts.join("/"))
}

/// Whether the absolute `path` is `base` or a folder in it.
fn inside(path: &str, base: &str) -> bool {
    base == "/" || path == base || path.strip_prefix(base).is_some_and(|r| r.starts_with('/'))
}

/// The folder the picker opens at first: the field's `value` when it names a folder you could choose (absolute from
/// `/`, or relative within a Root), else where it would fall back to, [`top`].
pub fn start(value: &str, within: Option<&Within>) -> String {
    let value = value.trim();
    if value.is_empty() || value.split('/').any(|p| p == "..") {
        return top(within);
    }
    match within {
        None if value.starts_with('/') => tidy(value),
        Some(w) if !value.starts_with('/') => tidy(&format!("{}/{value}", w.path)),
        _ => top(within),
    }
}

/// The highest folder the picker shows: the Root's, or `/`.
pub fn top(within: Option<&Within>) -> String {
    within.map_or_else(|| "/".to_owned(), |w| w.path.clone())
}

/// The trail from the top to `path`, each crumb opening its folder. Within a Root, the first crumb is the Root's.
pub fn crumbs(path: &str, within: Option<&Within>) -> Vec<Crumb> {
    let (mut trail, base) = match within {
        Some(w) if inside(path, &w.path) => (
            vec![Crumb {
                label: w.label.clone(),
                path: w.path.clone(),
            }],
            w.path.as_str(),
        ),
        _ => (
            vec![Crumb {
                label: "/".into(),
                path: "/".into(),
            }],
            "/",
        ),
    };
    let mut at = base.trim_end_matches('/').to_owned();
    for part in path[base.len()..].split('/').filter(|p| !p.is_empty()) {
        at = format!("{at}/{part}");
        trail.push(Crumb {
            label: part.into(),
            path: at.clone(),
        });
    }
    trail
}

/// The folder above `path`, unless it is the top.
pub fn up(path: &str, within: Option<&Within>) -> Option<String> {
    if path == top(within) || path == "/" {
        return None;
    }
    let parent = path.rsplit_once('/').map(|(p, _)| p)?;
    Some(if parent.is_empty() { "/" } else { parent }.to_owned())
}

/// What choosing `path` puts in the field: the path itself, or within a Root the path relative to it. None at a
/// Root's own folder, which is no path in it.
pub fn chosen(path: &str, within: Option<&Within>) -> Option<String> {
    match within {
        None => Some(path.to_owned()),
        Some(w) => path
            .strip_prefix(w.path.trim_end_matches('/'))
            .and_then(|r| r.strip_prefix('/'))
            .filter(|r| !r.is_empty())
            .map(str::to_owned),
    }
}

/// A folder's accessible name: its name, then what marks it.
pub fn folder_label(folder: &Folder) -> String {
    let mut label = folder.name.clone();
    if folder.link {
        label.push_str(", a link");
    }
    if !folder.writable {
        label.push_str(", read-only");
    }
    if let Some(root) = &folder.root {
        label.push_str(&format!(", in the root {root}"));
    }
    label
}

/// How many folders `path` holds, for the live line under the trail.
pub fn count_line(listing: &FolderListing) -> String {
    match listing.folders.len() {
        0 => format!("No folders in {}", listing.path),
        1 => format!("1 folder in {}", listing.path),
        n if listing.more => format!("The first {n} folders in {}", listing.path),
        n => format!("{n} folders in {}", listing.path),
    }
}

/// What the picker shows: the folders it listed, or why it can't list them.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Shown {
    Loading,
    Listed(FolderListing),
    Refused(String),
}

/// A **Browse…** button for a path field, named for `field` ("the root's path"). It opens a drawer at the field's
/// `value`, else at the top: `/`, or within `within`'s Root, where it returns a path relative to the Root.
/// `on_choose` gets the folder you choose.
#[component]
pub fn FolderPicker(
    #[prop(into)] field: String,
    #[prop(into)] value: Signal<String>,
    #[prop(optional, into)] within: Signal<Option<Within>>,
    #[prop(into)] on_choose: Callback<String>,
    #[prop(optional, into)] disabled: MaybeProp<bool>,
) -> impl IntoView {
    let open = RwSignal::new(false);
    let label = format!("Browse for {field}");
    let choose = Callback::new(move |path: String| {
        on_choose.run(path);
        open.set(false);
    });
    view! {
        <button
            type="button"
            class="browse"
            aria-haspopup="dialog"
            aria-label=label
            disabled=move || disabled.get().unwrap_or(false)
            on:click=move |_| open.set(true)
        >
            "Browse…"
        </button>
        <Drawer open title=Signal::stored("Choose a folder".to_owned())>
            <FolderBrowser
                start=start(&value.get_untracked(), within.get_untracked().as_ref())
                within=within.get_untracked()
                choose
            />
        </Drawer>
    }
}

#[component]
fn FolderBrowser(start: String, within: Option<Within>, choose: Callback<String>) -> impl IntoView {
    let within = StoredValue::new(within);
    let fallback = within.with_value(|w| top(w.as_ref()));
    let at = RwSignal::new(start);
    let hidden = RwSignal::new(false);
    // Where the field's path led when it couldn't be listed, and why; the picker then shows the top instead.
    let fell_back = RwSignal::new(None::<(String, String)>);
    let listed_once = RwSignal::new(false);
    let focus_list = RwSignal::new(false);
    let listing = LocalResource::new(move || {
        let path = at.get();
        let hidden = hidden.get();
        async move {
            let query = format!(
                "/folders?path={}{}",
                api::segment(&path),
                if hidden { "&hidden=true" } else { "" }
            );
            api::get::<FolderListing>(&query).await
        }
    });
    let shown = Memo::new(move |_| match listing.get() {
        None => Shown::Loading,
        Some(Ok(l)) => Shown::Listed(l),
        Some(Err(why)) => Shown::Refused(why),
    });
    let fallback_at = fallback.clone();
    Effect::new(move || match shown.get() {
        Shown::Listed(_) => listed_once.set(true),
        Shown::Refused(why) if !listed_once.get_untracked() => {
            let tried = at.get_untracked();
            if tried != fallback_at {
                fell_back.set(Some((tried, why)));
                at.set(fallback_at.clone());
            }
        }
        _ => {}
    });
    let go = move |path: String| {
        focus_list.set(true);
        at.set(path);
    };

    let list = NodeRef::<html::Ul>::new();
    let choose_button = NodeRef::<html::Button>::new();
    let buttons = move || -> Vec<web_sys::HtmlElement> {
        let Some(list) = list.get_untracked() else {
            return Vec::new();
        };
        let Ok(found) = list.query_selector_all("button") else {
            return Vec::new();
        };
        (0..found.length())
            .filter_map(|i| found.item(i))
            .filter_map(|n| n.dyn_into::<web_sys::HtmlElement>().ok())
            .collect()
    };
    // After opening a folder, focus goes to its first folder, or to Choose when it holds none.
    Effect::new(move || {
        if !matches!(shown.get(), Shown::Listed(_)) || !focus_list.get_untracked() {
            return;
        }
        focus_list.set(false);
        request_animation_frame(move || match buttons().first() {
            Some(first) => {
                let _ = first.focus();
            }
            None => {
                if let Some(b) = choose_button.get_untracked() {
                    let _ = b.focus();
                }
            }
        });
    });
    let on_keydown = move |ev: web_sys::KeyboardEvent| {
        let Some(key) = Step::of(&ev.key()) else {
            return;
        };
        let items = buttons();
        let active = document().active_element();
        let now = items
            .iter()
            .position(|i| active.as_ref() == Some(i.unchecked_ref()));
        if let Some(to) = step(now, key, items.len(), 10) {
            ev.prevent_default();
            let _ = items[to].focus();
        }
    };

    let trail = move || within.with_value(|w| crumbs(&at.get(), w.as_ref()));
    let above = move || within.with_value(|w| up(&at.get(), w.as_ref()));
    let picked = move || within.with_value(|w| chosen(&at.get(), w.as_ref()));
    let listed = move || matches!(shown.get(), Shown::Listed(_));

    view! {
        <div class="folder-picker">
            <nav class="crumbs" aria-label="Folder path">
                <ol>
                    {move || {
                        let trail = trail();
                        let last = trail.len() - 1;
                        trail
                            .into_iter()
                            .enumerate()
                            .map(|(i, crumb)| {
                                if i == last {
                                    view! { <li aria-current="location"><strong>{crumb.label}</strong></li> }
                                        .into_any()
                                } else {
                                    let path = crumb.path.clone();
                                    view! {
                                        <li>
                                            <button
                                                type="button"
                                                class="link-button"
                                                on:click=move |_| go(path.clone())
                                            >
                                                {crumb.label}
                                            </button>
                                            <Svg glyph=Glyph::ChevronRight />
                                        </li>
                                    }
                                        .into_any()
                                }
                            })
                            .collect_view()
                    }}
                </ol>
            </nav>
            <div class="folder-choose">
                <button
                    type="button"
                    class="primary"
                    node_ref=choose_button
                    disabled=move || !listed() || picked().is_none()
                    on:click=move |_| {
                        if let Some(path) = picked() {
                            choose.run(path);
                        }
                    }
                >
                    "Choose this folder"
                </button>
                {move || match picked() {
                    Some(path) => view! { <code class="path">{path}</code> }.into_any(),
                    None => view! {
                        <span class="note small">"Open a folder in the root to choose it."</span>
                    }
                    .into_any(),
                }}
            </div>
            <div class="folder-tools">
                <button
                    type="button"
                    class="small"
                    disabled=move || above().is_none()
                    on:click=move |_| {
                        if let Some(path) = above() {
                            go(path);
                        }
                    }
                >
                    <Svg glyph=Glyph::CornerLeftUp />
                    "Up a folder"
                </button>
                <label class="inline">
                    <input
                        type="checkbox"
                        prop:checked=hidden
                        on:change=move |ev| hidden.set(event_target_checked(&ev))
                    />
                    "Show hidden folders"
                </label>
            </div>
            {move || {
                fell_back
                    .get()
                    .map(|(tried, why)| {
                        let top = fallback.clone();
                        view! {
                            <Message kind=MessageKind::NOTICE>
                                {format!("The service can't list {tried} ({why}), so this starts at {top}.")}
                            </Message>
                        }
                    })
            }}
            <p class="note" aria-live="polite">
                {move || match shown.get() {
                    Shown::Loading => format!("Listing the folders in {}…", at.get()),
                    Shown::Listed(l) => count_line(&l),
                    Shown::Refused(_) => String::new(),
                }}
            </p>
            {move || match shown.get() {
                Shown::Refused(message) => view! { <Problem message /> }.into_any(),
                _ => ().into_any(),
            }}
            <ul
                class="folder-list"
                node_ref=list
                aria-label=move || format!("Folders in {}", at.get())
                on:keydown=on_keydown
            >
                {move || match shown.get() {
                    Shown::Listed(l) => l
                        .folders
                        .into_iter()
                        .map(|folder| {
                            let label = folder_label(&folder);
                            let glyph = if folder.link { Glyph::FolderSymlink } else { Glyph::Folder };
                            let path = folder.path.clone();
                            view! {
                                <li>
                                    <button
                                        type="button"
                                        class="folder"
                                        aria-label=label
                                        on:click=move |_| go(path.clone())
                                    >
                                        <Svg glyph />
                                        <span class="name">{folder.name}</span>
                                        {folder.link.then(|| view! { <span class="tag">"link"</span> })}
                                        {(!folder.writable).then(|| view! { <span class="tag">"read-only"</span> })}
                                        {folder.root.map(|r| view! { <span class="tag">{format!("root {r}")}</span> })}
                                    </button>
                                </li>
                            }
                        })
                        .collect_view()
                        .into_any(),
                    _ => ().into_any(),
                }}
            </ul>
            {move || match shown.get() {
                Shown::Listed(l) if l.more => view! {
                    <Message kind=MessageKind::NOTICE live=Live::Off>
                        "This folder holds more folders than the list shows. If yours isn't here, type its path."
                    </Message>
                }
                .into_any(),
                _ => ().into_any(),
            }}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roms() -> Within {
        Within::new("/mnt/roms/", "roms")
    }

    #[test]
    fn a_path_is_tidied() {
        assert_eq!(tidy("/mnt//roms/./"), "/mnt/roms");
        assert_eq!(tidy("/"), "/");
        assert_eq!(tidy(""), "/");
        assert_eq!(roms().path, "/mnt/roms");
    }

    #[test]
    fn it_starts_at_the_fields_path_when_that_could_be_chosen_else_at_the_top() {
        assert_eq!(start("", None), "/");
        assert_eq!(start(" /srv/roms/ ", None), "/srv/roms");
        assert_eq!(start("srv/roms", None), "/", "a root's path is absolute");
        assert_eq!(start("/srv/../etc", None), "/");
        let w = roms();
        assert_eq!(start("", Some(&w)), "/mnt/roms");
        assert_eq!(
            start("No-Intro/Atari", Some(&w)),
            "/mnt/roms/No-Intro/Atari"
        );
        assert_eq!(
            start("/elsewhere", Some(&w)),
            "/mnt/roms",
            "a path in a root is relative"
        );
        assert_eq!(start("../up", Some(&w)), "/mnt/roms");
    }

    #[test]
    fn the_trail_runs_from_the_top_to_the_folder() {
        let labels = |c: Vec<Crumb>| c.into_iter().map(|c| (c.label, c.path)).collect::<Vec<_>>();
        assert_eq!(labels(crumbs("/", None)), [("/".into(), "/".into())]);
        assert_eq!(
            labels(crumbs("/mnt/roms", None)),
            [
                ("/".to_owned(), "/".to_owned()),
                ("mnt".into(), "/mnt".into()),
                ("roms".into(), "/mnt/roms".into())
            ]
        );
        assert_eq!(
            labels(crumbs("/mnt/roms/No-Intro", Some(&roms()))),
            [
                ("roms".to_owned(), "/mnt/roms".to_owned()),
                ("No-Intro".into(), "/mnt/roms/No-Intro".into())
            ],
            "within a root, the trail starts at the root"
        );
        assert_eq!(
            labels(crumbs("/mnt/romsets", Some(&roms())))[0].0,
            "/",
            "a folder beside the root is not in it"
        );
    }

    #[test]
    fn going_up_stops_at_the_top() {
        assert_eq!(up("/", None), None);
        assert_eq!(up("/mnt", None), Some("/".into()));
        assert_eq!(up("/mnt/roms", None), Some("/mnt".into()));
        assert_eq!(up("/mnt/roms", Some(&roms())), None);
        assert_eq!(
            up("/mnt/roms/a/b", Some(&roms())),
            Some("/mnt/roms/a".into())
        );
    }

    #[test]
    fn choosing_returns_the_path_or_within_a_root_the_path_in_it() {
        assert_eq!(chosen("/", None), Some("/".into()));
        assert_eq!(chosen("/mnt/roms", None), Some("/mnt/roms".into()));
        let w = roms();
        assert_eq!(
            chosen("/mnt/roms", Some(&w)),
            None,
            "the root itself is no path in it"
        );
        assert_eq!(chosen("/mnt/roms/a/b", Some(&w)), Some("a/b".into()));
        assert_eq!(chosen("/mnt/romsets", Some(&w)), None);
        let top = Within::new("/", "everything");
        assert_eq!(chosen("/srv", Some(&top)), Some("srv".into()));
    }

    #[test]
    fn a_folder_is_named_with_what_marks_it() {
        let folder = Folder {
            name: "roms".into(),
            path: "/mnt/roms".into(),
            writable: false,
            root: Some("sets".into()),
            link: true,
        };
        assert_eq!(
            folder_label(&folder),
            "roms, a link, read-only, in the root sets"
        );
        let plain = Folder {
            writable: true,
            root: None,
            link: false,
            ..folder
        };
        assert_eq!(folder_label(&plain), "roms");
    }

    #[test]
    fn the_count_says_how_many_folders_it_shows() {
        let mut listing = FolderListing {
            path: "/mnt".into(),
            ..FolderListing::default()
        };
        assert_eq!(count_line(&listing), "No folders in /mnt");
        let folder = Folder {
            name: "a".into(),
            path: "/mnt/a".into(),
            writable: true,
            root: None,
            link: false,
        };
        listing.folders = vec![folder.clone()];
        assert_eq!(count_line(&listing), "1 folder in /mnt");
        listing.folders.push(folder);
        assert_eq!(count_line(&listing), "2 folders in /mnt");
        listing.more = true;
        assert_eq!(count_line(&listing), "The first 2 folders in /mnt");
    }
}
