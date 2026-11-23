//! Layouts over nested routes. A list page is a `ParentRoute` whose view puts its list in a `SplitView`; a child
//! route, such as `:id`, renders into the split view's `<Outlet/>` as a `DetailPane`, and a `""` child leaves it
//! empty.

use leptos::prelude::*;
use leptos_router::components::{A, Outlet};

use super::icon::{Glyph, Svg};
use super::overlay::Tooltip;

/// A list beside the detail pane its nested route renders. On a narrow screen the pane stacks over the list as a
/// drawer.
#[component]
pub fn SplitView(children: Children) -> impl IntoView {
    view! {
        <div class="split">
            <div class="split-list">{children()}</div>
            <Outlet />
        </div>
    }
}

/// The detail of the row a nested route selected, titled `title`. `close` is where its close link goes: the list's
/// own path, with its query string.
#[component]
pub fn DetailPane(
    #[prop(into)] title: Signal<String>,
    #[prop(into)] close: Signal<String>,
    children: Children,
) -> impl IntoView {
    view! {
        <aside class="detail-pane" aria-label=move || title.get()>
            <header class="detail-head">
                <h2>{move || title.get()}</h2>
                <Tooltip label="Close the detail">
                    <A href=move || close.get() attr:class="icon-button" attr:aria-label="Close the detail">
                        <Svg glyph=Glyph::X />
                    </A>
                </Tooltip>
            </header>
            <div class="detail-body">{children()}</div>
        </aside>
    }
}

/// A trail of links to the pages above this one, `(href, label)` each, ending with `here`, the page itself. Every page
/// below a list has one.
#[component]
pub fn Crumbs(trail: Vec<(String, String)>, #[prop(into)] here: String) -> impl IntoView {
    view! {
        <nav class="crumbs" aria-label="Breadcrumb">
            <ol>
                {trail
                    .into_iter()
                    .map(|(href, label)| {
                        view! {
                            <li>
                                <A href=href>{label}</A>
                                <Svg glyph=Glyph::ChevronRight />
                            </li>
                        }
                    })
                    .collect_view()}
                <li aria-current="page">{here}</li>
            </ol>
        </nav>
    }
}
