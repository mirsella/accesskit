use accesskit::Role;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DomRole {
    pub tag: &'static str,
    pub aria_role: Option<&'static str>,
    pub input_type: Option<&'static str>,
}

const fn element(tag: &'static str, aria_role: Option<&'static str>) -> DomRole {
    DomRole {
        tag,
        aria_role,
        input_type: None,
    }
}

const fn input(input_type: &'static str, aria_role: Option<&'static str>) -> DomRole {
    DomRole {
        tag: "input",
        aria_role,
        input_type: Some(input_type),
    }
}

pub(crate) fn dom_role(role: Role) -> DomRole {
    match role {
        Role::Button | Role::DefaultButton => element("button", None),
        Role::CheckBox => input("checkbox", None),
        Role::RadioButton => input("radio", None),
        Role::Slider => input("range", None),
        Role::TextInput => input("text", None),
        Role::SearchInput => input("search", None),
        // Email inputs use text + inputmode so browser selection APIs remain available.
        Role::EmailInput => input("text", None),
        Role::PasswordInput => input("password", None),
        Role::NumberInput => input("number", None),
        Role::PhoneNumberInput => input("tel", None),
        Role::UrlInput => input("url", None),
        Role::DateInput => input("date", None),
        Role::DateTimeInput => input("datetime-local", None),
        Role::WeekInput => input("week", None),
        Role::MonthInput => input("month", None),
        Role::TimeInput => input("time", None),
        Role::MultilineTextInput => element("textarea", None),
        Role::Link => element("a", Some("link")),
        Role::Label | Role::TextRun => element("span", None),
        Role::Alert => element("div", Some("alert")),
        Role::AlertDialog => element("div", Some("alertdialog")),
        Role::Dialog => element("div", Some("dialog")),
        Role::Status => element("div", Some("status")),
        Role::Heading => element("div", Some("heading")),
        Role::Image => element("div", Some("img")),
        Role::ProgressIndicator => element("div", Some("progressbar")),
        Role::List => element("div", Some("list")),
        Role::ListItem => element("div", Some("listitem")),
        Role::ListBox => element("div", Some("listbox")),
        Role::ListBoxOption | Role::MenuListOption => element("div", Some("option")),
        Role::Menu => element("div", Some("menu")),
        Role::MenuBar => element("div", Some("menubar")),
        Role::MenuItem => element("div", Some("menuitem")),
        Role::MenuItemCheckBox => element("div", Some("menuitemcheckbox")),
        Role::MenuItemRadio => element("div", Some("menuitemradio")),
        Role::RadioGroup => element("div", Some("radiogroup")),
        Role::Tab => element("div", Some("tab")),
        Role::TabList => element("div", Some("tablist")),
        Role::TabPanel => element("div", Some("tabpanel")),
        Role::Table => element("div", Some("table")),
        Role::Grid => element("div", Some("grid")),
        Role::TreeGrid => element("div", Some("treegrid")),
        Role::Row => element("div", Some("row")),
        Role::RowGroup => element("div", Some("rowgroup")),
        Role::Cell => element("div", Some("cell")),
        Role::GridCell => element("div", Some("gridcell")),
        Role::ColumnHeader => element("div", Some("columnheader")),
        Role::RowHeader => element("div", Some("rowheader")),
        Role::Tree => element("div", Some("tree")),
        Role::TreeItem => element("div", Some("treeitem")),
        Role::Group => element("div", Some("group")),
        Role::Log => element("div", Some("log")),
        Role::Marquee => element("div", Some("marquee")),
        Role::Meter => element("div", Some("meter")),
        Role::Splitter => element("div", Some("separator")),
        Role::Switch => element("div", Some("switch")),
        Role::ComboBox | Role::EditableComboBox => element("div", Some("combobox")),
        Role::SpinButton => element("div", Some("spinbutton")),
        Role::ScrollBar => element("div", Some("scrollbar")),
        Role::Application => element("div", Some("application")),
        Role::Article => element("article", None),
        Role::Banner | Role::Header => element("header", None),
        Role::Complementary => element("aside", None),
        Role::ContentInfo | Role::Footer => element("footer", None),
        Role::Form => element("div", Some("form")),
        Role::Main => element("main", None),
        Role::Navigation => element("nav", None),
        Role::Paragraph => element("p", None),
        Role::Region => element("div", Some("region")),
        Role::Search => element("div", Some("search")),
        Role::Timer => element("div", Some("timer")),
        Role::Toolbar => element("div", Some("toolbar")),
        Role::Tooltip => element("div", Some("tooltip")),
        Role::RootWebArea | Role::Document => element("div", Some("document")),
        _ => element("div", None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_native_controls_where_browser_behavior_matches() {
        assert_eq!(dom_role(Role::Button), element("button", None));
        assert_eq!(dom_role(Role::EmailInput), input("text", None));
        assert_eq!(dom_role(Role::PasswordInput), input("password", None));
        assert_eq!(dom_role(Role::Dialog), element("div", Some("dialog")));
        assert_eq!(dom_role(Role::Table), element("div", Some("table")));
        assert_eq!(dom_role(Role::TreeGrid), element("div", Some("treegrid")));
    }
}
