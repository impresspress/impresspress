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

use super::card::{is_data_name, with_data_attributes};

/// Where an action sits in the page's hierarchy, and so how it looks.
///
/// | Emphasis | Class | Look | For |
/// |---|---|---|---|
/// | `Primary` | `.btn--primary` | `--primary-button`, white text | the page's one main action |
/// | `Secondary` | `.btn--secondary` | `--surface-1`, 1px border | every other action |
/// | `Quiet` | `.btn--quiet` | transparent | in-row and in-card controls |
/// | `QuietDanger` | `.btn--quiet-danger` | transparent, danger text | an in-row destructive control |
/// | `Danger` | `.btn--danger` | danger fill, white text | a destructive confirm |
///
/// `Quiet` and `QuietDanger` draw the existing `.btn--quiet` /
/// `.btn--quiet-danger` rules, which hand-written in-row buttons also use.
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
            Emphasis::Quiet => "btn--quiet",
            Emphasis::QuietDanger => "btn--quiet-danger",
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
    /// The link tells a screen reader so ("(opens in a new tab)", visually
    /// hidden), since nothing else on it does.
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
    disabled: bool,
    autofocus: bool,
    title: Option<&'a str>,
    keyshortcuts: Option<&'a str>,
    data: Vec<(&'static str, &'a str)>,
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
            disabled: false,
            autofocus: false,
            title: None,
            keyshortcuts: None,
            data: Vec::new(),
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

    /// Render the button `disabled`, for an action a script enables once it
    /// can do something (the chat's Send, before a thread is open). A link
    /// has no disabled state, so only a button target takes one.
    pub fn disabled(mut self, disabled: bool) -> Self {
        assert!(
            !matches!(self.target, ActionTarget::Link(_) | ActionTarget::NewTab(_)),
            "a link cannot be disabled; Action::disabled needs a button target"
        );
        self.disabled = disabled;
        self
    }

    /// Render with `autofocus`, for the one action a swapped-in fragment
    /// hands focus to (htmx focuses it once the swap lands), so focus is not
    /// left on a control the swap removed.
    pub fn autofocus(mut self) -> Self {
        self.autofocus = true;
        self
    }

    /// The `title` tooltip (`"Save draft (Ctrl+S)"`). It is a hint only:
    /// the label stays the accessible name.
    pub fn title(mut self, title: &'a str) -> Self {
        self.title = Some(title);
        self
    }

    /// The `aria-keyshortcuts` the page's script binds to this action
    /// (`"Control+S Meta+S"`).
    pub fn keyshortcuts(mut self, keys: &'a str) -> Self {
        self.keyshortcuts = Some(keys);
        self
    }

    /// A `data-{name}` attribute: a parameter the page's script reads off
    /// the action (`.data("product-status", "archived")` beside
    /// `ActionTarget::Delegated("pm-set-status")`), or a marker it selects
    /// on. `name` is the part after `data-`, as for `Card::data`. The two
    /// names a target writes, `action` and `modal-target`, are refused: the
    /// target owns them.
    pub fn data(mut self, name: &'static str, value: &'a str) -> Self {
        assert!(
            is_data_name(name),
            "Action::data takes the part after `data-`, in lowercase ASCII letters, digits and \
             inner hyphens; got {name:?}"
        );
        assert!(
            !matches!(name, "action" | "modal-target"),
            "Action::data({name:?}) would repeat the attribute the ActionTarget writes"
        );
        self.data.push((name, value));
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
        let new_tab = matches!(self.target, ActionTarget::NewTab(_));
        let content = html! {
            @if let Some(icon) = self.icon { (icon) }
            (self.label)
            @if new_tab { span .sr-only { " (opens in a new tab)" } }
        };
        let (id, hidden, title, keys) = (self.id, self.hidden, self.title, self.keyshortcuts);
        let (disabled, autofocus) = (self.disabled, self.autofocus);
        let element = match self.target {
            ActionTarget::Link(href) => html! {
                a class=(class) id=[id] hidden[hidden] title=[title] aria-keyshortcuts=[keys] autofocus[autofocus]
                    href=(href)
                { (content) }
            },
            ActionTarget::NewTab(href) => html! {
                a class=(class) id=[id] hidden[hidden] title=[title] aria-keyshortcuts=[keys] autofocus[autofocus]
                    href=(href) target="_blank" rel="noopener"
                { (content) }
            },
            ActionTarget::Modal(modal) => html! {
                button class=(class) id=[id] hidden[hidden] title=[title] aria-keyshortcuts=[keys] autofocus[autofocus]
                    type="button" data-action="modal-open" data-modal-target=(modal) disabled[disabled]
                { (content) }
            },
            ActionTarget::Delegated(verb) => html! {
                button class=(class) id=[id] hidden[hidden] title=[title] aria-keyshortcuts=[keys] autofocus[autofocus]
                    type="button" data-action=(verb) disabled[disabled]
                { (content) }
            },
            ActionTarget::Submit { form } => html! {
                button class=(class) id=[id] hidden[hidden] title=[title] aria-keyshortcuts=[keys] autofocus[autofocus]
                    type="submit" form=[form] disabled[disabled]
                { (content) }
            },
            ActionTarget::HxGet { url, target } => html! {
                button class=(class) id=[id] hidden[hidden] title=[title] aria-keyshortcuts=[keys] autofocus[autofocus]
                    type="button" hx-get=(url) hx-target=(target) disabled[disabled]
                { (content) }
            },
        };
        with_data_attributes(element, &self.data)
    }
}

/// One action in the page header (`ui::shell::Topbar`): a label, an
/// optional icon and an [`ActionTarget`], plus the attributes a page script
/// needs (`id`, `hidden`, `data-*`, a shortcut).
///
/// It carries no emphasis and no size. The header's slot picks the emphasis
/// (`Topbar::secondary_actions` render as secondaries, `primary_action` as
/// the one primary, after them) and every header action keeps the control
/// height. So the header states the slot rules out cannot be written: two
/// header primaries, a primary that is not last, a quiet, destructive or
/// small header button.
#[must_use = "a HeaderAction renders nothing until it is put in a Topbar"]
pub struct HeaderAction<'a>(Action<'a>);

impl<'a> HeaderAction<'a> {
    pub fn new(label: &'a str, target: ActionTarget<'a>) -> Self {
        // The emphasis is a placeholder: the slot sets it when it renders.
        Self(Action::secondary(label, target))
    }

    /// A leading icon, as [`Action::icon`].
    pub fn icon(self, icon: Markup) -> Self {
        Self(self.0.icon(icon))
    }

    pub fn id(self, id: &'a str) -> Self {
        Self(self.0.id(id))
    }

    /// Render with the `hidden` attribute, for an action a script reveals.
    pub fn hidden(self, hidden: bool) -> Self {
        Self(self.0.hidden(hidden))
    }

    /// As [`Action::title`].
    pub fn title(self, title: &'a str) -> Self {
        Self(self.0.title(title))
    }

    /// As [`Action::keyshortcuts`].
    pub fn keyshortcuts(self, keys: &'a str) -> Self {
        Self(self.0.keyshortcuts(keys))
    }

    /// As [`Action::data`].
    pub fn data(self, name: &'static str, value: &'a str) -> Self {
        Self(self.0.data(name, value))
    }

    /// Render in the header's secondary slot.
    pub(in crate::ui) fn render_secondary(self) -> Markup {
        Action {
            emphasis: Emphasis::Secondary,
            ..self.0
        }
        .render()
    }

    /// Render as the header's one primary.
    pub(in crate::ui) fn render_primary(self) -> Markup {
        Action {
            emphasis: Emphasis::Primary,
            ..self.0
        }
        .render()
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
                "btn btn--quiet",
            ),
            (
                Action::quiet_danger("Go", ActionTarget::Link("/x")),
                "btn btn--quiet-danger",
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
            r#"<a class="btn btn--secondary" href="/b/legalpages/privacy" target="_blank" rel="noopener">Do<span class="sr-only"> (opens in a new tab)</span></a>"#
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
                r#"<button class="btn btn--quiet-danger btn--sm" id="remove-1" hidden type="button" data-action="remove-row"><svg"#
            ),
            "{s}"
        );
        assert!(s.ends_with("</svg>Remove</button>"), "{s}");
        assert!(!render(Action::secondary("x", ActionTarget::Link("/"))).contains("hidden"));
    }

    /// `autofocus` lands on the element whatever it is: a swapped-in link
    /// ("Sign in again") or a button.
    #[test]
    fn autofocus_marks_the_element_htmx_focuses() {
        assert_eq!(
            render(
                Action::primary("Sign in again", ActionTarget::Link("/b/auth/login")).autofocus()
            ),
            r#"<a class="btn btn--primary" autofocus href="/b/auth/login">Sign in again</a>"#
        );
        assert_eq!(
            render(Action::secondary("Go", ActionTarget::Submit { form: None }).autofocus()),
            r#"<button class="btn btn--secondary" autofocus type="submit">Go</button>"#
        );
        assert!(!render(Action::secondary("x", ActionTarget::Link("/"))).contains("autofocus"));
    }

    #[test]
    fn a_button_can_start_disabled_and_a_link_cannot() {
        assert_eq!(
            render(
                Action::primary("Send", ActionTarget::Submit { form: None })
                    .id("send-btn")
                    .disabled(true)
            ),
            r#"<button class="btn btn--primary" id="send-btn" type="submit" disabled>Send</button>"#
        );
        assert!(!render(
            Action::primary("Send", ActionTarget::Submit { form: None }).disabled(false)
        )
        .contains("disabled"));
        let link = std::panic::catch_unwind(|| {
            Action::secondary("Go", ActionTarget::Link("/")).disabled(true)
        });
        assert!(link.is_err(), "a link has no disabled state");
    }

    #[test]
    fn title_keyshortcuts_and_data_attributes_go_on_the_element() {
        let s = render(
            Action::secondary("Save draft", ActionTarget::Delegated("save"))
                .title("Save draft (Ctrl+S)")
                .keyshortcuts("Control+S Meta+S")
                .data("product-status", "a\"b")
                .data("marker", ""),
        );
        assert_eq!(
            s,
            r#"<button data-product-status="a&quot;b" data-marker="" class="btn btn--secondary" title="Save draft (Ctrl+S)" aria-keyshortcuts="Control+S Meta+S" type="button" data-action="save">Save draft</button>"#
        );
    }

    #[test]
    #[should_panic(expected = "Action::data takes the part after `data-`")]
    fn data_names_are_the_tail_of_a_data_attribute() {
        let _ = Action::secondary("x", ActionTarget::Link("/")).data("data-x", "1");
    }

    /// htmx reads `data-hx-post` as `hx-post`: a request goes through
    /// `ActionTarget::HxGet`, never through `data()`.
    #[test]
    #[should_panic(expected = "Action::data takes the part after `data-`")]
    fn data_refuses_an_htmx_attribute() {
        let _ = Action::secondary("x", ActionTarget::Link("/")).data("hx-post", "/b/x");
    }

    /// The target writes `data-action` / `data-modal-target`; a second copy
    /// from `data()` would be a duplicate attribute the browser ignores.
    #[test]
    fn data_refuses_the_names_a_target_writes() {
        for name in ["action", "modal-target"] {
            let refused = std::panic::catch_unwind(|| {
                Action::secondary("x", ActionTarget::Modal("m")).data(name, "y")
            });
            assert!(refused.is_err(), "{name} must be refused");
        }
    }

    /// A header action takes its emphasis from the slot it renders in, and
    /// keeps every attribute it was given.
    #[test]
    fn a_header_action_renders_as_its_slot_says() {
        let action = || {
            HeaderAction::new("Approve", ActionTarget::Delegated("moderate"))
                .icon(icons::plus())
                .id("approve")
                .hidden(true)
                .data("moderation-action", "approve")
        };
        let plus = icons::plus().into_string();
        assert_eq!(
            action().render_primary().into_string(),
            format!(
                r#"<button data-moderation-action="approve" class="btn btn--primary" id="approve" hidden type="button" data-action="moderate">{plus}Approve</button>"#
            )
        );
        assert_eq!(
            action().render_secondary().into_string(),
            format!(
                r#"<button data-moderation-action="approve" class="btn btn--secondary" id="approve" hidden type="button" data-action="moderate">{plus}Approve</button>"#
            )
        );
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
