//! Messages: an outcome ("Saved the storage policy."), a problem, or a notice that needs a look. A message is not a
//! status: it has no vocabulary and no legend. Its kind is a tone and a glyph beside the text, so it reads without
//! colour, and a word that names the kind for assistive technology. How it is announced is set apart from its kind:
//! by default a problem is an alert and the others a polite status, and a message that shows what is stored (a job's
//! error, a failed item in a list) rather than what you just did is not announced at all.

use leptos::prelude::*;

use super::icon::{Glyph, Svg};
use super::status::Tone;

/// How a message is announced as it appears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Live {
    /// No live role: shown, not announced. For what is stored rather than the result of what you just did.
    Off,
    /// `role="status"`: read out when assistive technology is next idle.
    Polite,
    /// `role="alert"`: read out at once.
    Alert,
}

impl Live {
    pub fn role(self) -> Option<&'static str> {
        match self {
            Live::Off => None,
            Live::Polite => Some("status"),
            Live::Alert => Some("alert"),
        }
    }
}

/// A message's kind: its tone, its glyph, the word that names it for assistive technology, and how it is announced
/// unless the message says otherwise.
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

    /// The live role of a message of this kind: `live` if given, else the kind's own.
    pub fn role(self, live: Option<Live>) -> Option<&'static str> {
        live.unwrap_or(self.live).role()
    }

    /// The classes of a message of this kind.
    pub fn class(self) -> String {
        format!("message {}", self.tone.class())
    }
}

/// A message of `kind`: its glyph beside `children`, which may be text, a paragraph or several, a link or a list.
/// `live` overrides how the kind is announced.
#[component]
pub fn Message(
    kind: MessageKind,
    #[prop(optional)] live: Option<Live>,
    children: Children,
) -> impl IntoView {
    view! {
        <div class=kind.class() role=kind.role(live)>
            <Svg glyph=kind.glyph />
            <div class="message-text">
                <span class="visually-hidden">{format!("{}: ", kind.label)}</span>
                {children()}
            </div>
        </div>
    }
}

/// A problem from the Service, announced as an alert unless `live` says otherwise.
#[component]
pub fn Problem(
    #[prop(into)] message: String,
    #[prop(optional)] live: Option<Live>,
) -> impl IntoView {
    let kind = MessageKind::PROBLEM;
    view! { <Message kind live=live.unwrap_or(kind.live)>{message}</Message> }
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
    fn by_default_only_a_problem_is_an_alert() {
        for kind in MessageKind::ALL {
            let alert = kind == MessageKind::PROBLEM;
            assert_eq!(
                kind.role(None),
                Some(if alert { "alert" } else { "status" }),
                "{}",
                kind.label
            );
        }
    }

    /// A problem stored with a job, a build item or an addition is shown, not announced: a list of them read out at
    /// once would drown what you just did.
    #[test]
    fn a_stored_problem_shown_with_live_off_has_no_live_role() {
        assert_eq!(MessageKind::PROBLEM.role(Some(Live::Off)), None);
    }

    #[test]
    fn how_a_message_is_announced_does_not_depend_on_its_kind() {
        for kind in MessageKind::ALL {
            assert_eq!(kind.role(Some(Live::Off)), None, "{}", kind.label);
            assert_eq!(
                kind.role(Some(Live::Polite)),
                Some("status"),
                "{}",
                kind.label
            );
            assert_eq!(
                kind.role(Some(Live::Alert)),
                Some("alert"),
                "{}",
                kind.label
            );
        }
    }

    /// Strips `/* … */` comments.
    fn uncommented(css: &str) -> String {
        let mut css = css.to_string();
        while let Some(start) = css.find("/*") {
            let end = css[start..].find("*/").expect("comments close") + start + 2;
            css.replace_range(start..end, "");
        }
        css
    }

    /// Every style rule as (selector list, declarations), with the rules inside `@media` and the like flattened.
    fn rules(css: &str) -> Vec<(String, String)> {
        fn walk(css: &str, out: &mut Vec<(String, String)>) {
            let mut rest = css;
            while let Some(open) = rest.find('{') {
                let selector = rest[..open].rsplit([';', '}']).next().unwrap_or("").trim();
                let mut depth = 0;
                let mut close = open;
                for (i, c) in rest[open..].char_indices() {
                    match c {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                close = open + i;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                assert!(close > open, "a rule closes after {selector:?}");
                let body = &rest[open + 1..close];
                if selector.starts_with('@') {
                    walk(body, out);
                } else {
                    out.push((selector.to_string(), body.to_string()));
                }
                rest = &rest[close + 1..];
            }
        }
        let mut out = Vec::new();
        walk(&uncommented(css), &mut out);
        out
    }

    /// The class names a selector names.
    fn classes(selector: &str) -> impl Iterator<Item = &str> {
        selector.split('.').skip(1).map(|c| {
            c.split(|ch: char| !(ch.is_alphanumeric() || ch == '-' || ch == '_'))
                .next()
                .unwrap_or("")
        })
    }

    /// The part's own rules: the only ones that may colour a message.
    const OWN: [&str; 4] = [
        ".message",
        ".message > .icon",
        ".message-text",
        ".message-text > :last-child",
    ];

    /// A declaration that colours what it applies to.
    fn colours(property: &str) -> bool {
        let property = property.trim();
        property == "color"
            || property == "fill"
            || property == "stroke"
            || property.starts_with("background")
            || (property.starts_with("border")
                && !["radius", "width", "style", "collapse", "spacing", "image"]
                    .iter()
                    .any(|part| property.contains(part)))
    }

    /// The part's own rules found in `css`, and every other rule that colours a message.
    fn message_colouring(css: &str) -> (Vec<String>, Vec<String>) {
        let (mut own, mut outside) = (Vec::new(), Vec::new());
        for (list, body) in rules(css) {
            for selector in list.split(',').map(str::trim) {
                if OWN.contains(&selector) {
                    own.push(selector.to_string());
                    continue;
                }
                let names_message =
                    classes(selector).any(|c| c == "message" || c.starts_with("message-"));
                let coloured = body
                    .split(';')
                    .filter_map(|d| d.split_once(':'))
                    .map(|(property, _)| property)
                    .find(|property| colours(property));
                if let (true, Some(property)) = (names_message, coloured) {
                    outside.push(format!("{selector} sets {}", property.trim()));
                }
            }
        }
        (own, outside)
    }

    /// The hand-written message styles are gone.
    #[test]
    fn no_rule_styles_a_message_by_the_old_classes() {
        for (list, _) in rules(include_str!("../../style/main.css")) {
            for class in classes(&list) {
                assert!(
                    !["done", "failed", "problem", "notice", "review"].contains(&class),
                    "{} styles .{class}",
                    list.trim()
                );
            }
        }
    }

    /// The part's own rules are in the stylesheet, and no other rule colours a message.
    #[test]
    fn only_the_parts_own_rules_colour_a_message() {
        let (own, outside) = message_colouring(include_str!("../../style/main.css"));
        for selector in OWN {
            assert!(
                own.iter().any(|s| s == selector),
                "main.css has no {selector} rule"
            );
        }
        assert_eq!(outside, Vec::<String>::new());
    }

    #[test]
    fn a_rule_outside_the_part_that_colours_a_message_is_caught() {
        let own = ".message { color: var(--color-text); border: 1px solid var(--tone); }\n\
                   .message > .icon { color: var(--tone); }";
        assert_eq!(message_colouring(own).1, Vec::<String>::new());
        for (css, caught) in [
            ("#jobs .message { color: red }", "#jobs .message sets color"),
            (
                ".message.tone-failed { background: pink; }",
                ".message.tone-failed sets background",
            ),
            (
                "@media (max-width: 1100px) { .list .message-text { color: red } }",
                ".list .message-text sets color",
            ),
            (
                "p, .drawer .message { border-color: red }",
                ".drawer .message sets border-color",
            ),
            (
                "/* .message */ .x .message > .icon { background-color: red }",
                ".x .message > .icon sets background-color",
            ),
        ] {
            assert_eq!(
                message_colouring(&format!("{own}\n{css}")).1,
                vec![caught.to_string()],
                "{css}"
            );
        }
        assert_eq!(
            message_colouring(".message { margin: 0 } #jobs .message { margin: 0 }").1,
            Vec::<String>::new(),
            "spacing isn't colour"
        );
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
