# Workbench UI

The workbench (`crates/ui`) is icon-first: lists of rows, a detail pane, one legend per list. This page holds the
copy rules, the shape of a page and of a plan, and the shared parts in `crates/ui/src/parts/` that every screen builds on. The User-facing text rules in
`AGENTS.md` apply to every string below.

## Copy rules

- **A table cell is terse.** It holds an icon, a number, a name, or a status of three words or fewer. Never a reason.
- **Reasons live in the detail pane.** Why a row has its status, what a plan item will do, what went wrong.
- **Prose is the last resort.** Write it only where no icon, status or number conveys the meaning clearly. Longer
  help goes in `Help`, a collapsible `<details>` whose one-line summary stays in view.
- **Every icon is named.** It has a tooltip and an `aria-label`: use `Icon`, `IconButton`, `StatusIcon` or
  `StatusBadge`, which require the label. `LabelledIcon` is the one exception: its visible label names it.
- **Every tooltip is reachable from the keyboard.** A standalone `Icon` is a tab stop. Inside a link or button, draw
  the glyph with `Svg`, name the control, and wrap it in `Tooltip` so there is one tab stop, not two.
- **Destructive actions are labelled and confirmed.** A visible verb ("Set aside", "Delete plan"), never an icon
  alone, and a confirmation step before anything is moved or removed.
- **One legend strip per list.** It shows only the statuses present in that list.
- **Status labels** are sentence case, glossary terms lowercase ("Repair open", "Matches source set"). Hints are one
  sentence addressed to "you".

## Page shape

Every screen reads in this order. A reviewer checks a screen against it.

1. **Title, status badge, and at most one primary action.** The action is the usual next step ("Audit", "Plan
   build"); everything else is a plain button, a row action or a menu item.
2. **A strip of counts** by status or kind. Each count filters the list below.
3. **The list**, with reasons in the detail pane.
4. **Explanations in `Help`.** One line stays in view. No paragraph sits above the list.
5. **Rare and destructive actions last**, collapsed.

A screen leads with what needs a decision: integrity, unresolved review items and failures come before facts, and a
fact is shown once. Whatever starts a Job shows its state where it was started, and becomes a link to the result
when the Job ends; a failure shows first, with its count.

## Plan shape

Every plan (repair, build, cleanup, ingest, deleting kept files, setting aside) is a `PlanReview` in this order:

1. **One line saying what the plan will do**, in numbers ("Renames 12 entries, fills 3 games, sets aside 2 files"),
   from `plan_model::plan_line`.
2. **Counts by kind**, each a filter, with "apply to all" where the plan offers choices.
3. **Approve, above the list**, labelled with what it does ("Approve and make 17 changes"), with "Plan again" beside
   it once you change a choice or while the plan may be stale: an `ApproveBar`. What blocks it is listed just above it, each blocker with its item
   and what to do. Once approved, the plan's job takes its place.
4. **After it applies, counts by outcome** (`ItemOutcome`), failures first, each a filter, with the line from
   `plan_model::outcome_line` ("4 of 6 done: 1 failed, 1 not done.").
5. **The list**: kind, current → proposed, choice, and last the Outcome column, empty until the plan is applied.
6. What the plan binds, and how the storage policy shaped it, go in `Help`.

Approve is never below the list, and no paragraph sits above it.

## One of each

- **One list.** Every list of rows is a `List`: no other table component, and no bulleted list of rows with inline
  actions. A small table of settings (roots, ceilings) may be a plain `<table>`.
- **One kept-files view**, wherever kept files are shown: `screens::kept::KeptFiles { place?, scope?, busy?, refresh? }`
  on a source set's, a library folder's and an ingest folder's Kept tab, under each additions set, and (without
  `place`) on Activity. Its pure parts (rows, the count and size, the "set aside by" link, the deletion's summary)
  are in `kept_model`. Deleting is planned from the folder's list and reviewed in place (the plan's id in the URL's
  `kept-deletion`, or `{scope}-deletion`).
- **One tab mechanism.** The current tab is always in the URL, so a reload and a shared link keep it. Never hold a
  tab in a signal alone.
- **Drawer or page.** Creating or confirming something from a list opens a `Drawer`. Only the profile editor and
  the settings screens are pages of their own.

## Shared parts

All live under `crate::parts`. Pure logic in each module is host-tested (`cargo test -p stage-select-ui`); keep new
pure logic there too.

| Need                                   | Use                                                    | Module      |
| -------------------------------------- | ------------------------------------------------------ | ----------- |
| An icon that means something           | `Icon { glyph, label }`                                | `icon`      |
| An icon-only button                    | `IconButton { glyph, label, on_click, disabled? }`     | `icon`      |
| An icon beside its visible text        | `LabelledIcon { glyph, label }`                        | `icon`      |
| A row's status in a cell               | `StatusIcon { status, tab_stop? }`                     | `status`    |
| A status with its words (detail pane)  | `StatusBadge { status }`                               | `status`    |
| A count of one status (summary tile)   | `StatusCount { status, what?, children }` in `.counts` | `status`    |
| Counts by status that filter a list    | `KindCounts { counts, active, on_filter, label }` with `use_status_filter(key)` | `plan` |
| The key to a list's statuses           | `Legend { statuses, list }` with `present(rows)`       | `legend`    |
| A key to statuses of two vocabularies  | `LegendStrip { looks, list }`, `present_looks` each    | `legend`    |
| A job's progress, beside its counts    | `ProgressMeter { done, total, unit }`                  | `meter`     |
| A source set's games, beside its counts| `GamesMeter { games }`                                 | `meter`     |
| An outcome, a problem or a notice      | `Message { kind }` around its content (see Messages)   | `message`   |
| A problem from the Service             | `Problem { message }`                                  | `message`   |
| How the last request went              | `Said { said }`, an `Option<Result<String, String>>`   | `message`   |
| A job you started, until it ends       | `JobOutcome { job, seed?, cancel? }` (see Outcomes)    | `outcome`   |
| What starting jobs or a request made   | `ShowReports { reports }` of a `Reports`               | `outcome`   |
| A radio button or checkbox, with a note| `Choice { checked, on_change, radio?, note?, .. }` around its name | `choice` |
| Longer help under a one-line summary   | `Help { summary }` around the explanation              | `help`      |
| A slow request                         | `ActionPending { action, status }` around its content  | `pending`   |
| A slow load or any busy flag           | `Pending { pending, status, problem? }`                | `pending`   |
| A hover/focus hint on arbitrary content| `Tooltip { label, focusable?, clipped?, shown? }`      | `overlay`   |
| A row or toolbar menu                  | `Menu { glyph, label }` holding `MenuItem { disabled? }`| `overlay`  |
| A side panel over the page             | `Drawer { open, title }`                               | `overlay`   |
| A path field's folder browser          | `FolderPicker { field, value, within?, on_choose, disabled? }` in `.path-field` | `folder_picker` |
| Any list of rows                       | `List { id, label, rows, empty, detail, .. }`          | `list`      |
| Cell text that may not fit             | `Clip { text }`                                        | `list`      |
| A cell that shows less than it says    | `CellTip { label }` around its icon and short text     | `list`      |
| Filter, sort, open/checked rows in URL | `use_list_query()` (`List` uses it already)            | `url_state` |
| Tabs on a page, kept in the URL's `tab`| `Tabs { label, tabs }` of `Tab`s, `use_tab()`          | `tabs`      |
| A link to a tab from another page      | `tab_link(path, tab, state)`                           | `tabs`      |
| Tabs that are child routes             | `RouteTabs { label, tabs: Vec<(path, label)> }`        | `tabs`      |
| The trail to the pages above this one  | `Crumbs { trail: Vec<(href, label)>, here }`           | `layout`    |
| A plan's items, reviewed in bulk       | `PlanReview { id, label, rows, summary, detail, .. }`  | `plan`      |
| Current → proposed in a cell           | `Change { current, proposed }`                         | `plan`      |
| Approve and "Plan again" above a plan  | `ApproveBar { label, approval, on_approve, .. }`       | `plan`      |
| An item's choice, in its cell and detail | `plan::choice_select(label, offers, planned, now, ..)` | `plan`      |
| Light/dark override                    | `ThemeToggle` (already in the header)                  | `theme`     |

Two screen-level pieces are shared the same way. An audit's Games and Findings counts, wherever they show (the
source set overview, the audit page), are `screens::audit::AuditCounts { audit, lists, covered? }`, with its pure
logic in `summary_model`; with `covered`, its Games counts open the games with `covered=yes`. Proposing a repair and
opening its plan is `screens::source_sets::propose_repair(name)` then `repair_plan_href(name, id)`; as a button, use
`Proposal` with `RepairNow { proposal, name, title, secondary? }` and `ProposalStatus { proposal }`, which await the
plan before opening it and refuse a second click while it is made.

### Choices

A radio button or checkbox with a note is a `Choice`, its name the children, and so is any choice inside a `.form`,
whose other labels stack: `Choice` keeps the control and name on one line there. `radio` (the group's name) makes it
a radio button, otherwise it is a checkbox, with optional `disabled` and `indeterminate`. A note about one choice is
its `note`, never a loose `.note` beside or after it: the note sits on its own line under the name, lined up with
it, and the control's `aria-describedby` names it. A note about a whole group goes above the group. A list's row
checkbox is the list's own (`List`).

### Icons

`Glyph` (`parts/lucide.rs`) is generated from vendored Lucide icons; never edit it by hand. To add one, append its
Lucide name to `ICONS` in `tools/lucide/vendor.py` and run `python3 tools/lucide/vendor.py`. Icons are inline SVG;
nothing is fetched at runtime. The licence ships at `/licences/lucide.txt` and is listed on the About page.

### Status vocabulary

`parts::status` maps every status to a `Look { glyph, tone, label, hint }`. Colour and shape both differ, so no state
is told apart by colour alone. Vocabularies: `FindingStatus`, `GameStatus`, `SourceSetStatus`, `LibraryStatus`,
`PlanKind` (repair plans, `From<RepairKind>`), `BuildKind` (build plans, builds and cleanups, `From<ItemKind>`),
`IngestStatus`, `KeptDeletionKind` (a kept file in a deletion plan), `JobState`, `DatFileStatus`, `SupportFileStatus` (an arcade selection's support files), `StepStatus`
(a first-run step), `Outcome` (a game's selection outcome), `AvailabilityStatus` (what you have of a game, on claims or
verified, `of(status, claimed)`), `ChoiceStatus`, `SelectionFlag` (a group's flags), `Derived` (an audit entry),
`AdditionState`, `CheckFlag` (a folder check older than the build), `FileOutcome` (a folder check's file), build
`ItemState`, `ItemOutcome` (what became of a plan's item once applied), `CleanupKind` (a cleanup's candidates),
`Stream` (the live connection) and `PlanState` (where a page's plan stands, on its Plan or Repair tab).

- Never colour a status in CSS of its own: every coloured state renders through `StatusIcon`, `StatusBadge` or
  `StatusCount`. `.tag` is for neutral labels that are not a status ("Not in the DAT"). An error's text is a
  `Problem` (see Messages).

- A row with several facts takes the highest-ranked status: build `SourceSetFacts` / `LibraryFacts` and call `of()`,
  or call `highest(iter)` for any vocabulary.
- A status needs a look when its tone is `Review` or `Failed` (`needs_a_look`). The Source sets and Library folders
  lists open in that order, by rank then name (`by_attention`, through `source_set_model::checked_in` and
  `library_model::checked_in`), and their navigation badges count the same rows (`needing_a_look`). A sort in the URL
  still wins. The badges fetch again on settled Jobs and on `api::writes()`, which every accepted write bumps, since
  some writes change a status without a Job.
- `KindCounts` keeps the active kind in its strip at zero (`plan_model::with_active`), so a filter whose rows have all
  gone can still be turned off.
- To add a vocabulary, implement `Status` (`ALL` in precedence order, `look`). The tests in `status.rs` check label
  length, case, unique glyphs and labels; add the new type to them.
- In a cell that isn't a tab stop of its own (the shared list's rows, which the arrow keys move through), pass
  `tab_stop=false` to `StatusIcon` (or `Icon`); its label still names it.
- `Tone` is the colour: `Good`, `Review`, `Failed`, `Active`, `Neutral`, styled by `.tone-*` and `--tone-*`.

### Segmented meter

`parts::meter` draws "how much of a whole" as a row of equal segments (the style guide's "Segmented meter"), and
only two things get one: a running job's progress (`ProgressMeter`, done filled in the active tone, the rest
outlined) and a source set's games by game status (`GamesMeter`, complete filled in the good tone, incomplete
hatched in the review tone, missing outlined; nothing to check isn't drawn). It always sits beside the exact counts,
which stay as text, and is one `role="img"` whose label carries the numbers: no tab stop, no tooltip, no percentage.
`segments` shares the segments in proportion and gives every count above zero at least one; keep that rule there,
host-tested. Each segment takes its colour from its `Tone` class through `currentColor`, and nothing in it moves. The
source sets list's games cell keeps its icons and counts without a meter.

### Messages

A message is not a status: it has no vocabulary and no legend. `parts::message::Message` draws its kind's tone and
glyph beside its content, which may be text, paragraphs, a link or a list, and names the kind for assistive
technology, so it reads without colour. Use one of three kinds:

- `MessageKind::DONE`, an outcome: "Saved the storage policy."
- `MessageKind::PROBLEM`, something went wrong. `Problem { message }` is the short form, and `Pending` and
  `ActionPending` show their problems with it.
- `MessageKind::NOTICE`, something that needs a look before the Operator goes on.

How a message is announced is its `live` prop, not its kind. The kind gives the default: a problem is `Live::Alert`,
an outcome or a notice `Live::Polite`. Choose by what the message answers:

- `Live::Alert` (the problem default): a request the Operator just made failed. `Pending` and `ActionPending`
  problems, a failed `Said`, a queued selection that turned into a failure.
- `Live::Polite` (the outcome and notice default): the outcome of what the Operator just did, live progress, or a
  notice that appears because of it.
- `Live::Off`: what is stored, shown as the page loads or per row: a job's error, each failed or incomplete item in a
  list, a stored error on a record, a notice about the state of what is shown. Announcing these would read out the
  page's history as if it had just happened.

A part built on `Message` may make its own `MessageKind` (tone, glyph, label, live). A glyph means the same in every
message and every status vocabulary; `message.rs` tests that. Never colour a message in CSS of its own: the part's
own `.message` rules are the only ones that may set colour, border or background on its classes, and a test in
`message.rs` fails on any other rule that does.

### Outcomes

Wherever a job starts, `parts::outcome::JobOutcome` shows it until it ends, then links onward: never "follow it in
Activity" and never a hand-built link. Pass the started `Job` as `seed` so it shows at once; it follows the job
through the live book after that, and `cancel=true` offers cancelling a running build or repair. Without a seed it
fetches the job, and fetches it again when a resync drops it before its ending arrived (`needs_fetch`); a failed
fetch shows with "Try again". Mount it once, outside closures that read the live book, and keep `Reports` outside a
subtree that reloads when jobs settle, or the outcome is lost, or made again without knowing it watched the ending.

- Queued or running: the job's state, a `ProgressMeter` while it reports progress, and "Open the job", announced
  politely.
- Succeeded: a done message linking to what it made (`Outcome::of`: the audit, selection, build, or the repair on
  the source set's Repair tab), or else to what it worked on (`Subject::of`).
- Failed: a problem that says how far it got and what that left ("The build stopped at 412 of 620 files. Nothing was
  removed."), the error's first line, and "Open the job to see why." A build says what it removed only when its items
  account for every deletion: placing and returning delete files no item records, so then it says nothing about
  removal rather than claim none. The count includes files still being written. It is an alert only when the part watched the
  job end; one that had already failed shows with `Live::Off`.
- Interrupted or cancelled: a notice, how far it got, and the job's link.

Where one action starts several jobs or makes something without a job (a proposed repair), keep a `Reports` and show
it with `ShowReports`: `reports.started(result, doing)` adds a job's outcome or its refusal, `Report::Made` adds what
a request made with its link, and refusals lead. The pure parts (`shown`, `tally`, `Outcome::href`) are host-tested
in `outcome.rs`; add a job kind's wording and link there, never on a screen.

### Pending and problems

Write slow requests as `Action::new_local(|input| async { api::post(..).await })` returning `Result<_, String>`,
dispatch with `dispatch_local`, and wrap the content they refresh in `ActionPending`. While pending it shows a
spinner, the short status ("Making plan…") and the elapsed time, and keeps the previous content dimmed and inert. An
`Err` shows as a problem once the action settles. React to `Ok` with an `Effect` on `action.value()`; disable the
triggering button with `action.pending()`. `screens/repair.rs` (`RepairDesk`, propose and plan again) is the
reference: its action lives with the page, so the pending status outlives a change of tab.

`Pending` is the general form: give it any busy signal and optional problem. It also wraps its children in
`Transition` (busy while a resource reloads) and `ErrorBoundary`.

### Overlays

`Tooltip`, `Menu` and `Drawer` render through `Portal`, so they escape table and pane overflow. An overlay opens
above its anchor when only above has room, and lines up with the anchor's edge near the sides (`overlay::place`). Each open overlay is a layer on one stack (`parts::layers`): a
single window listener sends Escape to the topmost layer only. `Menu` supports arrow keys and closes on Tab. `Drawer`
is modal: Tab and Shift+Tab wrap over its tabbable elements only (`trap`, `tabbables`), and while any modal is open
the page's top-level elements are `inert`, except those holding the topmost modal and the layers above it. Each
element gets back its original inert state when the last modal closes; then focus returns to where it was. A new
overlay registers with `layers::layer` (or `open_layer`/`close_layer` when its cleanup order matters). `Tooltip { shown }`
shows it while a signal is true, kept within its anchor's width, for an anchor the keyboard reaches without focus.
A tooltip wraps anywhere, and is placed again for its drawn height, so a tall one that doesn't fit below goes above.

### Folder picker

Every path field the Operator registers sits in a `div.path-field` with a `FolderPicker` after its input: a
**Browse…** button (named "Browse for {field}") that opens a `Drawer` browsing `GET /folders`. Without `within` it
browses from `/` and chooses absolute paths; with `within` (`Within::new(root_path, root_name)`) its top is that
Root, it names folders from the Root's name, and it chooses the path relative to the Root, so it can't choose the
Root's own folder. Disable it while there is no Root to browse. In a `.form`, a `<label>` can't hold the button, so
wrap the label and the `.path-field` in a `div.path-group`. Keep any datalist of suggestions on the input.

The picker opens at the field's path, or at the top with a notice when the Service can't list it; that fallback
(`fell_back` in `FolderBrowser`) is where a "this folder doesn't exist yet" state belongs. Its folder list is a
`ul.folder-list` of buttons, not a `List`: each row is one action (open the folder), with no detail, checking, sorting
or filtering, so it is navigation, like the crumbs. It still moves with the arrow keys, Home, End, Page Up and Page Down
through `list_model::step`, and when it opens a folder focus goes to the first folder in it, or to "Choose this folder"
when there is none. Its pure logic (`start`, `crumbs`, `up`, `chosen`, `display`) is host-tested.

### Plan review

Every plan is reviewed with `parts::plan::PlanReview`, a `List` with a strip of counts by kind above it. Its rows
implement `PlanRow` (a `ListRow` with `type Kind: Status`): `PlanKind` for repairs, `BuildKind` for builds,
`CleanupKind` for cleanups, `IngestPlanKind` for ingests, `KeptDeletionKind` for deleting kept files. A row that also
does something counted on its own, such as a replacement setting the old file aside, returns that kind from `also`, so
the strip agrees with the plan's summary. Each kind's count is a filter (`kind` in the URL; add `kind` to the screen's
list state so leaving the list clears it). Pass the summary line in `summary`, Approve in `approve`, what blocks it in
`blockers` (`plan_model::PlanBlocker`: what, the item, why, what to do and where; `build_model::plan_blockers` for a
build or cleanup, `PlanBlocker::nothing_to_change` for a plan that changes nothing), and the plan's notes in `help`.
Never disable Approve without a blocker saying why. Pass `scope` for a review that shares its page with
another list; its URL keys, `kind` and `outcome` included, then take the scope's prefix.

The choice control is the same in every plan: `plan::choice_select` in the row's Choice cell and in its detail;
toolbar "Set to …" actions over the checked rows, or every row the filter shows, made with `plan::set_actions`; and
`apply` with `plan::apply_to_all` to put an "apply to all" control beside each count. Choices are staged in a
`plan_model::Picks` until you plan again, since approving approves the plan as shown: `Picks::set` counts what a bulk
choice did (show `BulkOutcome::message` in `message`, so a partial application says how many items don't offer the
choice), `Picks::changed` feeds `Approval`, and `ApproveBar` holds Approve while choices differ and offers "Plan
again". Set `Approval::stale` whenever the plan may no longer be approvable as it is (approving was refused, a blocker
may be fixed elsewhere, or the screen can't tell, as for every open repair), so "Plan again" is always the way out. A plan that offers no choices, such as an ingest plan approved whole, passes neither `apply` nor `actions`.

While a plan is approved or planned again, your choices are held as they are: pass the same `held` signal to
`choice_select`, `set_actions`, `apply_to_all` and `AdditionFields`, which disable their controls and ignore input
while it's true (`ToolAction::held_while` for any other toolbar action). Keep staged choices, addition fields and
the approving state out of the view a reload makes again: a screen whose page is rebuilt as each job settles keeps
them in a stable owner keyed by plan (library folders: `library_plan::provide_staging`, a `Stage` per plan or
cleanup draft; repairs: `RepairDesk`), and reconciles reloaded rows with `Picks::reconcile`, which drops only picks
an item no longer offers.

Once the plan is applied, each row's `PlanRow::outcome` fills the Outcome column (`plan::outcome_column`,
`outcome_cell`, `compare_outcome`) and the strip of counts by outcome (`outcome` in the URL). Take it from that row's
own job item (`build_model::built_outcome`, `cleanup_model::item_outcome`), with its error in `detail`: a row with no
item once the job ends is Uncertain, never Done. Rows show
`Change { current, proposed }`; reasons and evidence go in `detail`. Keep the plan's own choice rules pure and tested
(`repair_model`, `build_model::ConflictChoice`, `cleanup_model`). `screens/repair.rs` (`RepairTab`) is the reference;
`screens/library_plan.rs` reviews build and cleanup plans, and `screens/ingest_plan.rs` keeps its rows with
`last_good_rows`. A plan the server can't store until you choose something, such as a cleanup, starts as a draft in
the same shape (`CleanupDraft` on the Drift tab): every candidate left as it is, and "Plan the cleanup" in Approve's
place until you choose.

### Lists

Every list screen uses `parts::list::List`; its pure logic (windowing, cursor steps, `Selection`, filter and sort,
toolbar fit, column template) is in `parts::list_model`. `screens/dats.rs` is the reference.

1. **A row type.** Implement `ListRow` for the row (`Clone + PartialEq + Send + Sync + 'static`; equality decides whether a
   reload redraws the open row's detail):
   - `type Col` (a `Copy + Eq` enum) and `const COLUMNS: &[Col]`, in display order.
   - `column(col) -> Column`: `Column::new(id, header, width)`, then `.sortable()`, `.numeric()` (right-aligned, data
     font) and `.hint(text)` (the header's tooltip). `id` names the column in the URL's `sort`. `width` is
     `Width::Fixed(rem)` or `Width::Flex { min: rem, share }` (`minmax(min, share fr)`). Give text columns a `Flex`
     with a `min`, and wrap their text in `Clip { text }`: it is cut off with an ellipsis and its tooltip shows the
     whole text only when cut off: on hover, and while the keyboard cursor is on its row (the grid keeps focus, so
     a cell's own focus never fires). The columns' minimums are the grid's minimum width; past it the list scrolls
     sideways, so keep the sum under about 50rem (the list's width at 1400px with a detail pane open). A cell that
     abbreviates, such as an icon and a short label, wraps them in `CellTip { label }` instead: its tooltip always
     shows the whole of it, the same way.
   - `key()`: stable and unique, it names the row in the URL (`row`, `sel`) and keys its element.
   - `title()`: the detail's title and the accessible name of the row's checkbox and actions.
   - `search()`: the text the filter matches; it is lowercased once per change of `rows`.
   - `cell(col) -> AnyView`: terse (copy rules). A status cell is `StatusIcon { status, tab_stop: false }`.
   - `compare(other, col)` for sortable columns, and `href()` when the row has its own page (the detail links to it
     as "Open").
2. **The list.** `<List id label rows empty detail actions? row_actions? quick? placeholder? filters? keep? legend? scope? />`:
   - `id: &'static str` prefixes element IDs; `label` names the grid and toolbar; `empty` shows when there are no
     rows at all.
   - `rows: Signal<Arc<Vec<T>>>`: every row, in their default order. A reload replaces the `Arc`. It must reflect
     successful loads only: rows missing from it are gone, so an empty list from a failed or pending load would
     close the open row. Build it with `last_good_rows(move || resource.get())`, which keeps the last rows loaded,
     and show the error separately (`Problem`).
   - `detail: Callback<T, AnyView>`: the open row's summary, under its actions in the `DetailPane` (or the drawer
     below 1100px). Its title is `title()`. It follows the row's data across reloads, and closes if the row leaves
     `rows`.
   - `actions: Vec<ToolAction<T>>`: toolbar buttons, icon and label. `ToolAction::page(glyph, label, || ..)` acts on
     the page; `ToolAction::checked(glyph, label, |chosen: Selected<T>| ..)` acts on the checked rows and is
     disabled while none are. `Selected { rows, matching, left_out }`: `rows` are the checked rows in list order;
     with every match checked, `matching` holds the filter and `left_out` the keys unchecked since, so a Service
     action can apply to the filter rather than to a list of keys. Buttons that don't fit go under a ⋯ menu.
   - `row_actions: Callback<T, Vec<RowAction>>` and `quick` (1 or 2, default 1): the first `quick` actions are
     icons at the row's end, shown on hover, on the keyboard cursor and on the open row; the rest are under the
     row's ⋯ menu. The detail shows them all, labelled. `RowAction::go(glyph, label, href)`,
     `RowAction::run(glyph, label, || ..)`, `RowAction::download(glyph, label, api::url(..))` (a plain link the
     browser saves, not a route) or `RowAction::unavailable(glyph, label)` (disabled; its label says why). Labels are full ("Open its latest audit, #12"). A destructive action is
     `RowAction::destructive(glyph, label, || ..)`: never a quick icon (it, and any action after it, stays under ⋯),
     and its `run` only opens a confirmation, such as a `Drawer`, never acts at once ("Delete revision" on DATs).
   - `filters: ViewFn` adds controls beside the filter text; `keep: Signal<Keep<T>>` is the matching predicate
     (`Option<Arc<dyn Fn(&T) -> bool>>`), applied with the filter text. Changing it clears an "every match"
     selection.
   - `legend: ViewFn`: the strip under the list, usually `Legend { statuses: present(rows), list: id }`.
   - `scope` (any string): for a second list on one page, the prefix of its URL keys (`changes-q`, `changes-row`,
     …), so each list keeps its own filter, sort, open row and checks. One list per page goes unscoped. Give each
     list on a page its own `id` too: the `id` prefixes its element ids.
   - A filter control beside the text keeps its value in the URL too: build it with `use_query_edit`, as
     `screens::audit::url_select` does, and derive `keep` from the query, so a link can open the list filtered
     (`/audits/7?finding=misnamed`).
3. **What it does for you.** Filter text (`q`), sort (`sort`), the open row (`row`) and the checked rows (`sel`,
   `all`) live in the URL. Rows are keyed and virtualized: only the rows in view plus a few more are in the DOM, all
   40px tall, so a list of 100,000 filters within the ADR 0005 budget (`crates/ui/bench/filter.py`, over the Audit's Entries). The
   header checkbox checks every row matching the filter (stored as `all=1`, not as keys), even when some already
   are, and clears only when all are; changing the
   filter text clears such a selection but keeps rows checked one by one. The toolbar shows "N selected", "Select
   all N" and a clear button. Keyboard: the grid is one tab stop; arrows, Page Up/Down, Home and End move the
   cursor; Space checks (Shift+Space checks the range from the last one); Enter opens; Escape closes the open row.
   Clicking a row opens it, except on its links, buttons and checkbox. `p.shown` reads "N of M" and carries
   `data-query` and `data-shown` for the bench.

### URL state

`use_list_query()` returns the current `ListQuery { filter, sort, selected, tab, checked }` and a setter that
replaces the URL without scrolling. Keys: `q`, `sort` (`-name` for descending), `row` (the open row's key), `tab`,
`sel` (checked rows' keys, comma-separated, `\` escapes) and `all=1` (every row matching the filter is checked; `sel`
then lists the rows unchecked since). Defaults are omitted and other query parameters are kept. Use
`ListQuery::sorted_by(column)` for a header click. `checked` is a `list_model::Selection`. `use_list_query_in(Some(scope))`
reads and writes a scoped list's keys (`{scope}-q`, …); `tab` always stays the page's.

### Layout and routes

`List` needs no route for its detail: it renders the open row's `DetailPane` itself, beside the list, and as a
`Drawer` below 1100px (`list::NARROW`). The open row is in the URL's `row`.

Every row of tabs is the one part in `parts::tabs`. Tabs are links in a `nav` with `aria-current="page"` on the open
one, which carries the current edge; the open tab is the row's one tab stop, the arrow keys, Home and End move focus,
and Enter opens. A tab may carry a status icon (`Tab::new(href, label, current).status(..)`) for something waiting on
you there. Two forms:

- **On a page**: `use_tab()` reads the URL's `tab` (`current()`, `None` for the default tab) and builds each tab's
  link (`href(key)`, `None` for the default). A tab's link keeps the page's other query values and drops the list
  state of the tab you leave (`q`, `sort`, `row`, `sel`, `all`, `kind`, a scoped list's `{scope}-q` and the like, and
  the filters in `parts::tabs::TAB_STATE`) and `plan`. A new list filter key goes in `TAB_STATE`. Changing only the query keeps the
  page mounted, so a draft survives a change of tab; don't read the whole query in a closure that renders the page.
- **A second row on a page**: `use_tab_keyed(key)` reads and links its own key, as the profile page's `uses` beside
  the editor's `tab`. Its links keep the other row's tab and drop only the scoped lists' state, so opening one leaves
  the other row, and a draft in it, in place.
- **Child routes**: `RouteTabs` over a `ParentRoute`, as on Activity and Settings.

A page below a list starts with `Crumbs`: the list, any page between, then this page's name. A page head with no
trail above it takes the class `top`, and a link that is the page's primary action takes `button primary`.

### Theme and colour

The theme follows `prefers-color-scheme`; `ThemeToggle` cycles system → light → dark and stores the override in
localStorage (`stage-select.theme`), applied before load by `assets/boot.js`. The colours, type and focus come from
the brand kit's `docs/brand-kit/stage-select-tokens.css`, which `assets/` links to and `style/main.css` imports;
`main.css` declares no colours of its own. Change a colour in the tokens file, in both themes, and keep the contrast
tests in `theme.rs` passing: every tone, text, secondary text and action colour at least 4.5:1 on canvas, surface,
hover and checked rows, each surface on its own; the state marks (cursor, open and current edges, focus, the
control border) at least 3:1 on the same surfaces; and the primary button's label on its fill and hover fill.

Each interaction state has its own look, from the style guide's "Interaction states", so a row can be hovered,
checked, under the cursor and open at once and show all four:

| State | Look | Where |
| --- | --- | --- |
| Hover | `--color-hover` surface; on a checked row, a ring in `--color-control-border` | `:hover` |
| Checked | `--color-checked` wash, and its checkbox | `.list-row.checked` |
| Keyboard cursor | Four corner brackets (`--cursor-brackets-image`, `-position`, `-size`), only while the grid has keyboard focus | `.list-row.cursor::after` |
| Open | `--color-open-edge` inset on the leading side, and weight 600 | `.list-row.open` |
| Current | `--current-edge`, an edge along the bottom | `aria-current`, a selected tab, a pressed filter |
| Focus | A 2 px outline, 3 px off | `:focus-visible` |

The open edge and the hover ring are layers of one `box-shadow` (`--open-mark`, `--hover-mark`), so set those
properties rather than `box-shadow` on a row. Never mark a state with a colour alone.

`button.primary` is the action fill (`--color-action-fill`, hovering `--color-action-fill-hover`), as the brand
kit's style guide says. The fonts are Chakra Petch 700 for `h1` and `h2` (`--font-display`), and Stage Sans 400 and
600 and Stage Mono 400, IBM Plex Sans and IBM Plex Mono renamed because a subset of a font with a reserved name must
not keep it. They are subset WOFF2 files in `assets/fonts/` with their licences beside them, all served by the
Service. Use weight 600, never 700, outside headings: no other weight ships. To change the fonts or their coverage,
edit and rerun `tools/fonts/subset.py`, which also checks that no renamed font still names Plex; never hand-edit the
files.

### Screenshots

A PR that changes a screen shows each affected screen before and after, in both themes, wide and below 1100px.
`tools/screenshots/shoot.py` takes them over a synthetic store (its docstring says what it seeds and what not):

```sh
python3 -m venv .venv && .venv/bin/pip install playwright && .venv/bin/playwright install chromium   # once
.venv/bin/python tools/screenshots/shoot.py --list                                # the screens it knows
.venv/bin/python tools/screenshots/shoot.py --ref main --screens source-sets,activity   # before
.venv/bin/python tools/screenshots/shoot.py --screens source-sets,activity              # after
```

Each run writes `target/screenshots/<label>/<screen>-<theme>-<width>.png`, light and dark at 1440 and 1000px; the
label is `after`, or the ref's name with `--ref`. `--route name=/path` adds a screen. `--empty` seeds nothing, to show
a first run (`--screens welcome --empty --label after-empty`), and leaves out the screens that show what the seed made. The `github-pr-media` skill
uploads the files into the PR body; never commit them.
