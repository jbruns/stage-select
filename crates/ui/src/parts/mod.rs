//! The parts every screen builds on. `docs/agents/ui.md` says when to use each.
//!
//! - `icon`: `Icon`, `IconButton` and `LabelledIcon` over the vendored Lucide glyphs in `lucide`.
//! - `status`: the status vocabulary, and `StatusIcon` and `StatusBadge` to show a status.
//! - `legend`: the `Legend` strip under a list.
//! - `list`: `List`, the toolbar, checkable rows, quick actions and detail every list screen uses, over `list_model`.
//! - `plan`: `PlanReview`, the plan review's counts by kind, rows reading current → proposed and choices set in bulk,
//!   over `plan_model`.
//! - `message`: `Message`, a tone and a glyph beside an outcome, a problem or a notice, and `Problem`.
//! - `pending`: `ActionPending` and `Pending` for slow requests and their problems.
//! - `outcome`: `JobOutcome`, a job's outcome wherever you start one, linking onward once it ends, and `Reports`
//!   with `ShowReports` for what a screen's actions said.
//! - `meter`: `ProgressMeter` and `GamesMeter`, the segmented meter for "how much of a whole".
//! - `choice`: `Choice`, a radio button or checkbox with its name and the note under it.
//! - `help`: `Help`, a one-line summary with longer help folded beneath it.
//! - `overlay`: `Tooltip`, `Menu` and `Drawer`, hosted in a `<Portal>`, stacked by `layers`.
//! - `theme`: the light and dark themes and `ThemeToggle`.
//! - `url_state`: a list's filter, sort, selected row and tab in the query string.
//! - `layout`: `SplitView`, `DetailPane` and the `Crumbs` above a page below a list.
//! - `tabs`: `Tabs`, `RouteTabs` and `use_tab`, the one row of tabs, whose open tab is always in the URL.
//! - `folder_picker`: `FolderPicker`, a Browse… button beside a path field that opens a folder browser.

pub mod choice;
pub mod folder_picker;
pub mod help;
pub mod icon;
pub mod layers;
pub mod layout;
pub mod legend;
pub mod list;
pub mod list_model;
pub mod lucide;
pub mod message;
pub mod meter;
pub mod outcome;
pub mod overlay;
pub mod pending;
pub mod plan;
pub mod plan_model;
pub mod status;
pub mod tabs;
pub mod theme;
pub mod url_state;
