//! A choice: a radio button or a checkbox with its name, and an optional note on its own line under the name.

use std::sync::atomic::{AtomicUsize, Ordering};

use leptos::prelude::*;

static NOTES: AtomicUsize = AtomicUsize::new(0);

/// A fresh id for a choice's note, unique on the page, for the control's `aria-describedby`.
pub fn note_id() -> String {
    format!("choice-note-{}", NOTES.fetch_add(1, Ordering::Relaxed) + 1)
}

/// A control and its name (`children`), with `note` on its own line beneath, indented to line up with the name and
/// tied to the control by `aria-describedby`. With `radio`, the group's name, it is a radio button; otherwise a
/// checkbox. `on_change` gets whether the control is now checked.
#[component]
pub fn Choice(
    #[prop(optional)] radio: Option<&'static str>,
    #[prop(into)] checked: Signal<bool>,
    on_change: Callback<bool>,
    #[prop(optional, into)] disabled: Option<Signal<bool>>,
    #[prop(optional, into)] indeterminate: Option<Signal<bool>>,
    #[prop(optional, into)] note: Option<ViewFn>,
    children: Children,
) -> impl IntoView {
    let id = note.as_ref().map(|_| note_id());
    let disabled = move || disabled.is_some_and(|d| d.get());
    let indeterminate = move || indeterminate.is_some_and(|i| i.get());
    view! {
        <div class="choice-option">
            <label class="inline">
                <input
                    type=if radio.is_some() { "radio" } else { "checkbox" }
                    name=radio
                    aria-describedby=id.clone()
                    disabled=disabled
                    prop:checked=checked
                    prop:indeterminate=indeterminate
                    on:change=move |ev| on_change.run(event_target_checked(&ev))
                />
                <span>{children()}</span>
            </label>
            {note.map(|note| view! { <p class="choice-option-note note small" id=id>{note.run()}</p> })}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_note_gets_its_own_id() {
        let (a, b) = (note_id(), note_id());
        assert_ne!(a, b);
        assert!(a.starts_with("choice-note-"), "{a}");
    }
}
