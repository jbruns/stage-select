# 27. An arcade selector beside the Retool port, chosen by DAT kind

Date: 2026-11-20

## Status

Accepted. Amends ADR 0015 and ADR 0021.

## Context

ADR 0015 made the Retool port the only Selection engine. Retool declines MAME, and its model, release names tagged
by region in No-Intro's style, doesn't fit arcade machines, whose region is free text in a description, if anywhere.
Research 0105 found that the criteria arcade tools converge on are cheap to express: drop non-games and broken
drivers, then elect one member per family by an ordered chain. On MAME 0.289 that takes 50,368 machines to about 3,200
games, 97% of whose families match the curated "Working Arcade Clean" list. Category data (catver) makes the
difference between a usable default and a noisy one; its files carry no licence.

## Decision

- **Two selectors, chosen by DAT kind.** An Arcade DAT, a MAME ListXML or later a FinalBurn Neo arcade DAT, goes to
  the arcade selector; every other DAT to the Retool port. The XML root doesn't decide: FinalBurn Neo's DATs are
  Logiqx. The arcade selector's output is the existing Selection, its groups and Rules, with stages named for its
  steps, plus the Dependency role (ADR 0025).
- **Every signal is optional.** The selector works on a machine model whose fields a DAT may lack. A filter whose
  signal a DAT lacks is shown as unavailable and drops nothing.
- **Default filters**, each overridable in the Profile: drop BIOS, devices and non-runnable machines (always);
  mechanical, screenless and software-list machines; catver's non-arcade, gambling, redemption, electromechanical,
  Tabletop, Board Game and Quiz categories, and Mature; and drivers below imperfect. Machines that need Disks or
  samples stay in. Variants (bootlegs, hacks, prototypes), year, players, controls and orientation are offered, off.
- **Election order**: the Operator's choice; driver status, good over imperfect; region; language; parent over clone;
  revision; not a bootleg or hack; shortname. Driver status first lets a working clone win a broken parent's family;
  region before parent makes the region order mean something, and since most parents are World releases the parent
  still wins most families. Each step is one Rule.
- **Support files are optional.** catver, languages and bestgames are fetched at run time (ADR 0012's amendment). A
  Selection records the revision of each that it used. Without catver, or for a machine catver lacks, a ListXML-only
  heuristic decides category, and the Selection says so; Selection is never refused for want of them.
- **Lists.** A Profile's arcade settings take include and exclude lists with the existing matchers. The pinned
  `curated-mame-arcade-romset` (Unlicense) is an optional list, off by default: on, it keeps only the families it
  names and prefers the Game it names, with the rule "named by" the list, linked. bestgames gives an optional minimum
  rating, off by default, applied to a whole family through its parent; an unrated family is kept unless the Operator
  drops unrated ones.
- **The Profile carries an optional `arcade` block**, globally and per system, beside the Retool settings.
- **Software lists aren't selected in M6.** A software-list Library Folder's Selection is everything its Source Set
  holds, with one Rule saying so.
- **MAME Source Sets have no Coverage.** Regions don't describe arcade machines; trimming what a Play Library holds is
  the selector's job. A MAME Source Set covers every Game, and the workbench says why it offers no Coverage.
- **A newer MAME** changes nothing here: a Selection pins its ListXML and support-file revisions, and an Operator choice
  whose shortname is gone becomes Needs Review, never dropped or moved.

## Considered Options

- **Extending the Retool port**: rejected; its naming, region and clone models are No-Intro's, and #3 forbids inventing
  an arcade 1G1R policy inside it.
- **Choosing by system name**: rejected; DAT kind is what the selector depends on.
- **Pinning catver in the image**: rejected without a licence; it is fetched, and attributed, instead.

## Consequences

- The About page credits progetto-SNAPS and the curated list's author, as it credits Retool.
- FinalBurn Neo is a follow-up: its DAT Provider, telling its devices from BIOSes, and how far its shortnames match
  catver's.
