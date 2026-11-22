//! Collapsible help: a one-line summary always shown, with the longer explanation folded beneath it.

use leptos::prelude::*;

/// Help that keeps `summary`, one line, in view, and folds `children`, the longer explanation, beneath it until you
/// open it.
#[component]
pub fn Help(#[prop(into)] summary: String, children: Children) -> impl IntoView {
    view! {
        <details class="help note small">
            <summary>{summary}</summary>
            {children()}
        </details>
    }
}
