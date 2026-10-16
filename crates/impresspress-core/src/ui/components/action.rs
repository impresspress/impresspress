//! Action: a button or link drawn as a `.btn`, with its place in the page's
//! hierarchy chosen by its [`Emphasis`].
//!
//! [`Action`] is meant to be the only writer of `.btn--primary`, the class
//! the page's one-primary rule is about: a page shows at most one visible
//! primary (one per `<dialog>`, which is its own context). Pages that still
//! hand-write `.btn--primary` move to `Action` as each one is redesigned.
//! Hand-written secondary, quiet and danger `.btn` markup stays legitimate —
//! an in-row control with its own `hx-*` wiring, a modal footer — because
//! the rule constrains only the primary.

use maud::{html, Markup};

/// Where an action sits in the page's hierarchy, and so how it looks.
///
/// | Emphasis | Class | Look | For |
/// |---|---|---|---|
/// | `Primary` | `.btn--primary` | `--primary-button`, white text | the page's one main action |
/// | `Secondary` | `.btn--secondary` | `--surface-1`, 1px border | every other action |
/// | `Quiet` | `.btn--ghost` | transparent | in-row and in-card controls |
/// | `QuietDanger` | `.btn--ghost-danger` | transparent, danger text | an in-row destructive control |
/// | `Danger` | `.btn--danger` | danger fill, white text | a destructive confirm |
///
/// `Quiet` and `QuietDanger` draw the existing `.btn--ghost` /
/// `.btn--ghost-danger` rules, which hand-written in-row buttons also use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Emphasis {
    Primary,
    Secondary,
    Quiet,
    QuietDanger,
    Danger,
}

impl Emphasis {
    fn class(self) -> &'static str {
        match self {
            Emphasis::Primary => "btn--primary",
            Emphasis::Secondary => "btn--secondary",
            Emphasis::Quiet => "btn--ghost",
            Emphasis::QuietDanger => "btn--ghost-danger",
            Emphasis::Danger => "btn--danger",
        }
    }
}

/// What an action does when pressed. Each target picks the element: the two
/// navigations are an `<a>`, the rest a `<button>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionTarget<'a> {
    /// `a href`: go to a page.
    Link(&'a str),
    /// `a href target="_blank" rel="noopener"`: open a page in a new tab.
    NewTab(&'a str),
    /// Open the `components::modal` with this id (`chrome.js` binds every
    /// `data-action="modal-open"` trigger).
    Modal(&'a str),
    /// A `data-action="<verb>"` button a page script handles through the
    /// shared delegated listener (`chrome.js`), e.g. `"open-upload"`.
    Delegated(&'a str),
    /// Submit a form: the enclosing one, or the one with this id.
    Submit { form: Option<&'a str> },
    /// Fetch `url` with htmx and swap it into `target`.
    HxGet { url: &'a str, target: &'a str },
}

/// One action, rendered as a `.btn`.
///
/// ```ignore
/// Action::primary("New bucket", ActionTarget::Modal("new-bucket"))
///     .icon(icons::plus())
///     .render()
/// ```
#[must_use = "an Action renders nothing until `render` is called"]
pub struct Action<'a> {
    emphasis: Emphasis,
    label: &'a str,
    target: ActionTarget<'a>,
    icon: Option<Markup>,
    small: bool,
    id: Option<&'a str>,
    hidden: bool,
}

impl<'a> Action<'a> {
    fn new(emphasis: Emphasis, label: &'a str, target: ActionTarget<'a>) -> Self {
        Self {
            emphasis,
            label,
            target,
            icon: None,
            small: false,
            id: None,
            hidden: false,
        }
    }

    /// The page's one main action. A page shows at most one visible primary.
    pub fn primary(label: &'a str, target: ActionTarget<'a>) -> Self {
        Self::new(Emphasis::Primary, label, target)
    }

    /// Any action that is not the page's main one.
    pub fn secondary(label: &'a str, target: ActionTarget<'a>) -> Self {
        Self::new(Emphasis::Secondary, label, target)
    }

    /// An in-row or in-card control. Never a page-header action.
    pub fn quiet(label: &'a str, target: ActionTarget<'a>) -> Self {
        Self::new(Emphasis::Quiet, label, target)
    }

    /// An in-row destructive control, quiet until pointed at; the confirm
    /// step it leads to is the real guard.
    pub fn quiet_danger(label: &'a str, target: ActionTarget<'a>) -> Self {
        Self::new(Emphasis::QuietDanger, label, target)
    }

    /// The button that confirms a destructive step.
    pub fn danger(label: &'a str, target: ActionTarget<'a>) -> Self {
        Self::new(Emphasis::Danger, label, target)
    }

    /// A leading icon (`icons::plus()` for a create action). The label stays
    /// the action's accessible name.
    pub fn icon(mut self, icon: Markup) -> Self {
        self.icon = Some(icon);
        self
    }

    /// The compact size (`.btn--sm`), for a control inside a table row only.
    /// Page-header and card-head actions keep the control height.
    pub fn small(mut self) -> Self {
        self.small = true;
        self
    }

    pub fn id(mut self, id: &'a str) -> Self {
        self.id = Some(id);
        self
    }

    /// Render with the `hidden` attribute, for an action a script reveals.
    pub fn hidden(mut self, hidden: bool) -> Self {
        self.hidden = hidden;
        self
    }

    pub(super) fn emphasis(&self) -> Emphasis {
        self.emphasis
    }

    pub fn render(self) -> Markup {
        let class = if self.small {
            format!("btn {} btn--sm", self.emphasis.class())
        } else {
            format!("btn {}", self.emphasis.class())
        };
        let content = html! {
            @if let Some(icon) = self.icon { (icon) }
            (self.label)
        };
        let (id, hidden) = (self.id, self.hidden);
        match self.target {
            ActionTarget::Link(href) => html! {
                a class=(class) id=[id] hidden[hidden] href=(href) { (content) }
            },
            ActionTarget::NewTab(href) => html! {
                a class=(class) id=[id] hidden[hidden] href=(href) target="_blank" rel="noopener" { (content) }
            },
            ActionTarget::Modal(modal) => html! {
                button class=(class) id=[id] hidden[hidden] type="button"
                    data-action="modal-open" data-modal-target=(modal)
                { (content) }
            },
            ActionTarget::Delegated(verb) => html! {
                button class=(class) id=[id] hidden[hidden] type="button" data-action=(verb) { (content) }
            },
            ActionTarget::Submit { form } => html! {
                button class=(class) id=[id] hidden[hidden] type="submit" form=[form] { (content) }
            },
            ActionTarget::HxGet { url, target } => html! {
                button class=(class) id=[id] hidden[hidden] type="button" hx-get=(url) hx-target=(target)
                { (content) }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{test_support::dom, ui::icons};

    fn render(action: Action<'_>) -> String {
        action.render().into_string()
    }

    #[test]
    fn each_emphasis_draws_its_own_variant_class() {
        let cases = [
            (
                Action::primary("Go", ActionTarget::Link("/x")),
                "btn btn--primary",
            ),
            (
                Action::secondary("Go", ActionTarget::Link("/x")),
                "btn btn--secondary",
            ),
            (
                Action::quiet("Go", ActionTarget::Link("/x")),
                "btn btn--ghost",
            ),
            (
                Action::quiet_danger("Go", ActionTarget::Link("/x")),
                "btn btn--ghost-danger",
            ),
            (
                Action::danger("Go", ActionTarget::Link("/x")),
                "btn btn--danger",
            ),
        ];
        for (action, class) in cases {
            assert_eq!(
                render(action),
                format!(r#"<a class="{class}" href="/x">Go</a>"#)
            );
        }
    }

    #[test]
    fn each_target_renders_its_element_and_wiring() {
        let s = |t| render(Action::secondary("Do", t));
        assert_eq!(
            s(ActionTarget::Link("/b/a?x=1&y=2")),
            r#"<a class="btn btn--secondary" href="/b/a?x=1&amp;y=2">Do</a>"#
        );
        assert_eq!(
            s(ActionTarget::NewTab("/b/legalpages/privacy")),
            r#"<a class="btn btn--secondary" href="/b/legalpages/privacy" target="_blank" rel="noopener">Do</a>"#
        );
        assert_eq!(
            s(ActionTarget::Modal("new-bucket")),
            r#"<button class="btn btn--secondary" type="button" data-action="modal-open" data-modal-target="new-bucket">Do</button>"#
        );
        assert_eq!(
            s(ActionTarget::Delegated("open-upload")),
            r#"<button class="btn btn--secondary" type="button" data-action="open-upload">Do</button>"#
        );
        assert_eq!(
            s(ActionTarget::Submit { form: None }),
            r#"<button class="btn btn--secondary" type="submit">Do</button>"#
        );
        assert_eq!(
            s(ActionTarget::Submit {
                form: Some("settings-form")
            }),
            r#"<button class="btn btn--secondary" type="submit" form="settings-form">Do</button>"#
        );
        assert_eq!(
            s(ActionTarget::HxGet {
                url: "/b/admin/logs?tab=audit",
                target: "#content"
            }),
            r##"<button class="btn btn--secondary" type="button" hx-get="/b/admin/logs?tab=audit" hx-target="#content">Do</button>"##
        );
    }

    #[test]
    fn icon_small_id_and_hidden_modify_the_button() {
        let s = render(
            Action::quiet_danger("Remove", ActionTarget::Delegated("remove-row"))
                .icon(icons::trash())
                .small()
                .id("remove-1")
                .hidden(true),
        );
        assert!(
            s.starts_with(
                r#"<button class="btn btn--ghost-danger btn--sm" id="remove-1" hidden type="button" data-action="remove-row"><svg"#
            ),
            "{s}"
        );
        assert!(s.ends_with("</svg>Remove</button>"), "{s}");
        assert!(!render(Action::secondary("x", ActionTarget::Link("/"))).contains("hidden"));
    }

    #[test]
    fn the_label_is_escaped() {
        assert_eq!(
            render(Action::secondary("<b>&", ActionTarget::Link("/"))),
            r#"<a class="btn btn--secondary" href="/">&lt;b&gt;&amp;</a>"#
        );
    }

    /// The walker the page guards use sees a primary only when it is visible:
    /// a hidden primary (a script's later reveal) does not count.
    #[test]
    fn a_hidden_primary_is_not_a_visible_one() {
        let shown = render(Action::primary(
            "Publish",
            ActionTarget::Submit { form: None },
        ));
        let hidden =
            render(Action::primary("Publish", ActionTarget::Submit { form: None }).hidden(true));
        assert_eq!(dom::visible_primaries(&shown).0.len(), 1);
        assert!(dom::visible_primaries(&hidden).0.is_empty());
    }
}
