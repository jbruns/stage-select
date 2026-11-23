//! One tab mechanism: every tab set is a row of links, and the current tab is always in the URL, so a reload and a
//! shared link keep it. A page's tabs are its child routes (`RouteTabs`, as on Activity and Settings), or the query
//! string's `tab` (`use_tab`, as on a source set's page). `Tabs` draws both, with the arrow keys moving between them.

use leptos::html;
use leptos::prelude::*;
use leptos_router::components::Outlet;
use leptos_router::hooks::{use_location, use_query_map};
use wasm_bindgen::JsCast;

use super::icon::Icon;
use super::status::{Look, Status};
use super::url_state::{edited, query_string};

/// The query string's keys that belong to one tab's content, such as its list's filter: a link to another tab leaves
/// them behind. Besides the shared list's own, `finding`, `state`, `algorithm` (an audit's Entries), `status`,
/// `availability` and `flag` (a selection's groups, an ingest folder's Items) and `outcome` (a plan's items once
/// applied) filter one tab's list, and `plan` names the plan one tab shows; no page keeps any of them across its tabs.
const TAB_STATE: [&str; 15] = [
    "tab",
    "q",
    "sort",
    "row",
    "sel",
    "all",
    "kind",
    "plan",
    "finding",
    "state",
    "algorithm",
    "status",
    "availability",
    "flag",
    "outcome",
];

/// A scoped list's own keys, such as `changes-q`, which a second list on a page uses, and the kind its review filters
/// by and the deletion it shows: they go with the tab too.
const LIST_KEYS: [&str; 8] = [
    "q", "sort", "row", "sel", "all", "kind", "outcome", "deletion",
];

fn is_tab_state(key: &str) -> bool {
    TAB_STATE.contains(&key)
        || key
            .rsplit_once('-')
            .is_some_and(|(scope, k)| !scope.is_empty() && LIST_KEYS.contains(&k))
}

/// The link to the tab `tab` of the page at `path` whose query string is `pairs`: the page's other state is kept, and
/// what belongs to the tab open now is left behind. `None` is the page's first tab, with no `tab` in the URL.
pub fn tab_href<'a>(
    path: &str,
    pairs: impl IntoIterator<Item = (&'a str, &'a str)>,
    tab: Option<&str>,
) -> String {
    let kept = pairs.into_iter().filter(|(k, _)| !is_tab_state(k));
    let pairs = edited(kept, &[("tab", tab.map(str::to_string))]);
    format!("{path}{}", query_string(&pairs))
}

/// A link from elsewhere to the tab `tab` of the page at `path` (its segments already encoded), showing what `state` names there, such as the plan
/// a repair carries out: `[("plan", "9")]`. `None` is the page's first tab.
pub fn tab_link(path: &str, tab: Option<&str>, state: &[(&str, &str)]) -> String {
    let pairs: Vec<(String, String)> = tab
        .map(|t| ("tab".to_string(), t.to_string()))
        .into_iter()
        .chain(state.iter().map(|(k, v)| (k.to_string(), v.to_string())))
        .collect();
    format!("{path}{}", query_string(&pairs))
}

/// Where a key moves the focus from the tab at `at` of `len`: the arrow keys to the one before or after, wrapping
/// around, and Home and End to the first and last. `None` for any other key.
pub fn step(at: usize, len: usize, key: &str) -> Option<usize> {
    if len == 0 {
        return None;
    }
    match key {
        "ArrowRight" => Some((at + 1) % len),
        "ArrowLeft" => Some((at + len - 1) % len),
        "Home" => Some(0),
        "End" => Some(len - 1),
        _ => None,
    }
}

/// One tab: where it links, its label, whether it is open, and the status it carries, if any, such as a plan waiting
/// on your review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tab {
    pub href: String,
    pub label: String,
    pub current: bool,
    pub status: Option<Look>,
}

impl Tab {
    pub fn new(href: impl Into<String>, label: impl Into<String>, current: bool) -> Self {
        Tab {
            href: href.into(),
            label: label.into(),
            current,
            status: None,
        }
    }

    /// The tab with `status`'s icon before its label.
    pub fn status<S: Status>(mut self, status: Option<S>) -> Self {
        self.status = status.map(Status::look);
        self
    }
}

/// A row of tabs, named `label` for assistive technology. The open tab is the row's one tab stop; the arrow keys,
/// Home and End move between the tabs, and Enter opens one.
#[component]
pub fn Tabs(label: &'static str, #[prop(into)] tabs: Signal<Vec<Tab>>) -> impl IntoView {
    let nav = NodeRef::<html::Nav>::new();
    let focus = move |at: usize| {
        let link = nav
            .get_untracked()
            .and_then(|n| n.query_selector_all("a").ok())
            .and_then(|links| links.item(at as u32))
            .and_then(|l| l.dyn_into::<web_sys::HtmlElement>().ok());
        if let Some(link) = link {
            let _ = link.focus();
        }
    };
    view! {
        <nav class="tabs" aria-label=label node_ref=nav>
            {move || {
                let tabs = tabs.get();
                let len = tabs.len();
                let stop = tabs.iter().position(|t| t.current).unwrap_or(0);
                tabs.into_iter()
                    .enumerate()
                    .map(|(at, t)| {
                        let on_keydown = move |ev: web_sys::KeyboardEvent| {
                            if let Some(to) = step(at, len, &ev.key()) {
                                ev.prevent_default();
                                focus(to);
                            }
                        };
                        view! {
                            <a
                                href=t.href
                                aria-current=t.current.then_some("page")
                                tabindex=if at == stop { "0" } else { "-1" }
                                on:keydown=on_keydown
                            >
                                {t.status.map(|look| view! {
                                    <span class=format!("status {}", look.tone.class())>
                                        <Icon glyph=look.glyph label=look.label tab_stop=false />
                                    </span>
                                })}
                                {t.label}
                            </a>
                        }
                    })
                    .collect_view()
            }}
        </nav>
    }
}

/// A page whose tabs are its child routes, `(path, label)` each, over the route the open tab renders.
#[component]
pub fn RouteTabs(label: &'static str, tabs: Vec<(&'static str, &'static str)>) -> impl IntoView {
    let path = use_location().pathname;
    let tabs = Signal::derive(move || {
        let path = path.get();
        let here = path.trim_end_matches('/');
        tabs.iter()
            .map(|&(href, label)| Tab::new(href, label, href == here))
            .collect::<Vec<_>>()
    });
    view! {
        <Tabs label tabs />
        <Outlet />
    }
}

/// The page's tab in the query string's `tab`, and links to its other tabs.
#[derive(Clone, Copy)]
pub struct UrlTab {
    query: Memo<leptos_router::params::ParamsMap>,
    path: Memo<String>,
}

/// The current page's tab, from the URL.
pub fn use_tab() -> UrlTab {
    UrlTab {
        query: use_query_map(),
        path: use_location().pathname,
    }
}

impl UrlTab {
    /// The open tab's name; `None` for the page's first tab.
    pub fn current(self) -> Option<String> {
        self.query.read().get("tab").filter(|t| !t.is_empty())
    }

    /// The link to the tab `tab`, as [`tab_href`] makes it.
    pub fn href(self, tab: Option<&str>) -> String {
        let path = self.path.get();
        self.query
            .with(|q| tab_href(&path, q.latest_values().map(|(k, v)| (k.as_ref(), v)), tab))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tab_link_keeps_the_page_state_and_leaves_the_open_tab_state_behind() {
        let pairs = [
            ("tab", "games"),
            ("q", "mario"),
            ("sort", "-name"),
            ("row", "3"),
            ("plan", "7"),
            ("saved", "2"),
        ];
        assert_eq!(
            tab_href("/source-sets/NES", pairs, Some("repair")),
            "/source-sets/NES?saved=2&tab=repair"
        );
        assert_eq!(
            tab_href("/source-sets/NES", pairs, None),
            "/source-sets/NES?saved=2"
        );
        assert_eq!(
            tab_href("/profiles/Mine", [], Some("Nintendo - NES & Famicom")),
            "/profiles/Mine?tab=Nintendo%20-%20NES%20%26%20Famicom"
        );
    }

    #[test]
    fn a_link_to_a_tab_from_elsewhere_carries_what_the_tab_shows() {
        assert_eq!(
            tab_link(
                "/source-sets/Atari%202600",
                Some("repair"),
                &[("plan", "9")]
            ),
            "/source-sets/Atari%202600?tab=repair&plan=9"
        );
        assert_eq!(
            tab_link("/libraries/7", Some("builds"), &[]),
            "/libraries/7?tab=builds"
        );
        assert_eq!(tab_link("/libraries/7", None, &[]), "/libraries/7");
    }

    #[test]
    fn a_tab_link_leaves_the_open_tabs_filters_behind() {
        let pairs = [
            ("finding", "missing"),
            ("status", "unmatched"),
            ("saved", "2"),
        ];
        assert_eq!(
            tab_href("/source-sets/NES", pairs, Some("games")),
            "/source-sets/NES?saved=2&tab=games"
        );
    }

    #[test]
    fn a_tab_link_leaves_every_list_filter_and_scoped_list_behind() {
        let pairs = [
            ("state", "unmatched"),
            ("algorithm", "sha1"),
            ("availability", "missing"),
            ("flag", "review"),
            ("changes-q", "mario"),
            ("previews-sort", "-made"),
            ("versions-row", "2"),
            ("stale-sel", "a,b"),
            ("drift-all", "1"),
            ("kept-deletion", "4"),
            ("kept-7-kind", "left"),
            ("kept-7-deletion", "5"),
            ("saved", "2"),
        ];
        assert_eq!(
            tab_href("/profiles/Mine", pairs, Some("Nintendo - NES")),
            "/profiles/Mine?saved=2&tab=Nintendo%20-%20NES"
        );
    }

    #[test]
    fn the_arrow_keys_move_between_tabs_and_wrap_around() {
        assert_eq!(step(0, 3, "ArrowRight"), Some(1));
        assert_eq!(step(2, 3, "ArrowRight"), Some(0));
        assert_eq!(step(0, 3, "ArrowLeft"), Some(2));
        assert_eq!(step(1, 3, "Home"), Some(0));
        assert_eq!(step(1, 3, "End"), Some(2));
        assert_eq!(step(1, 3, "Enter"), None);
        assert_eq!(step(0, 0, "ArrowRight"), None);
    }
}
