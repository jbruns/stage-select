//! Messages: an outcome ("Saved the storage policy."), a problem, or a notice that needs a look. A message is not a
//! status: it has no vocabulary and no legend. Its kind is a tone and a glyph beside the text, so it reads without
//! colour, and a word that names the kind for assistive technology. A problem is announced as an alert, the others
//! as a polite status.

use leptos::prelude::*;

use super::icon::{Glyph, Svg};
use super::status::Tone;

/// How a message is announced as it appears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Live {
    /// `role="status"`: read out when assistive technology is next idle.
    Polite,
    /// `role="alert"`: read out at once.
    Alert,
}

impl Live {
    pub fn role(self) -> &'static str {
        match self {
            Live::Polite => "status",
            Live::Alert => "alert",
        }
    }
}

/// A message's kind: its tone, its glyph, the word that names it for assistive technology, and how it is announced.
/// Use one of the three below; a part built on `Message`, such as a job's outcome, may make its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageKind {
    pub tone: Tone,
    pub glyph: Glyph,
    pub label: &'static str,
    pub live: Live,
}

impl MessageKind {
    /// Something you asked for is done.
    pub const DONE: MessageKind = MessageKind {
        tone: Tone::Good,
        glyph: Glyph::CircleCheck,
        label: "Done",
        live: Live::Polite,
    };
    /// Something went wrong.
    pub const PROBLEM: MessageKind = MessageKind {
        tone: Tone::Failed,
        glyph: Glyph::CircleX,
        label: "Problem",
        live: Live::Alert,
    };
    /// Something needs a look before you go on.
    pub const NOTICE: MessageKind = MessageKind {
        tone: Tone::Review,
        glyph: Glyph::TriangleAlert,
        label: "Needs a look",
        live: Live::Polite,
    };

    /// The three kinds every screen uses.
    pub const ALL: [MessageKind; 3] = [Self::DONE, Self::PROBLEM, Self::NOTICE];

    /// The classes of a message of this kind.
    pub fn class(self) -> String {
        format!("message {}", self.tone.class())
    }
}

/// A message of `kind`: its glyph beside `children`, which may be text, a paragraph or several, a link or a list.
#[component]
pub fn Message(kind: MessageKind, children: Children) -> impl IntoView {
    view! {
        <div class=kind.class() role=kind.live.role()>
            <Svg glyph=kind.glyph />
            <div class="message-text">
                <span class="visually-hidden">{format!("{}: ", kind.label)}</span>
                {children()}
            </div>
        </div>
    }
}

/// A problem from the Service, announced as it appears.
#[component]
pub fn Problem(#[prop(into)] message: String) -> impl IntoView {
    view! { <Message kind=MessageKind::PROBLEM>{message}</Message> }
}

/// How the last request went, if you made one: done, or a problem.
#[component]
pub fn Said(#[prop(into)] said: Signal<Option<Result<String, String>>>) -> impl IntoView {
    move || {
        said.get().map(|said| match said {
            Ok(text) => view! { <Message kind=MessageKind::DONE>{text}</Message> }.into_any(),
            Err(message) => view! { <Problem message /> }.into_any(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::parts::status::tests::every_vocabulary;

    #[test]
    fn each_kind_differs_from_the_others_in_glyph_tone_and_word() {
        let kinds = MessageKind::ALL;
        let distinct =
            |f: fn(&MessageKind) -> String| kinds.iter().map(f).collect::<HashSet<_>>().len();
        assert_eq!(distinct(|k| format!("{:?}", k.glyph)), kinds.len());
        assert_eq!(distinct(|k| format!("{:?}", k.tone)), kinds.len());
        assert_eq!(distinct(|k| k.label.to_string()), kinds.len());
    }

    #[test]
    fn only_a_problem_is_an_alert() {
        for kind in MessageKind::ALL {
            let alert = kind == MessageKind::PROBLEM;
            assert_eq!(
                kind.live.role(),
                if alert { "alert" } else { "status" },
                "{}",
                kind.label
            );
        }
    }

    /// The hand-written message styles are gone, so nothing colours a message but `.message` and its tone.
    #[test]
    fn no_rule_styles_a_message_by_the_old_classes() {
        let mut css = include_str!("../../style/main.css").to_string();
        while let Some(start) = css.find("/*") {
            let end = css[start..].find("*/").expect("comments close") + start + 2;
            css.replace_range(start..end, "");
        }
        let selectors = css
            .split('}')
            .filter_map(|rule| rule.split_once('{').map(|(selector, _)| selector));
        for selector in selectors {
            let classes = selector.split('.').skip(1).map(|c| {
                c.split(|ch: char| !(ch.is_alphanumeric() || ch == '-' || ch == '_'))
                    .next()
                    .unwrap_or("")
            });
            for class in classes {
                assert!(
                    !["done", "failed", "problem", "notice", "review"].contains(&class),
                    "{} styles .{class}",
                    selector.trim()
                );
            }
        }
    }

    /// Whether a tone reads as going well (`Some(true)`), as trouble (`Some(false)`), or as neither.
    fn sense(tone: Tone) -> Option<bool> {
        match tone {
            Tone::Good => Some(true),
            Tone::Failed | Tone::Review => Some(false),
            Tone::Active | Tone::Neutral => None,
        }
    }

    /// A status may soften a message's glyph (a RomM scan that failed is for you to review, not a failure of the
    /// build), but a check never marks trouble, and a cross or a warning never marks success.
    #[test]
    fn a_message_glyph_means_the_same_in_every_status_vocabulary() {
        for kind in MessageKind::ALL {
            for (name, looks) in every_vocabulary() {
                for look in looks.iter().filter(|l| l.glyph == kind.glyph) {
                    let sense = sense(look.tone);
                    assert!(
                        sense.is_none() || sense == self::sense(kind.tone),
                        "{name}: {:?} draws the {} glyph in the {:?} tone",
                        look.label,
                        kind.label,
                        look.tone
                    );
                }
            }
        }
    }
}
