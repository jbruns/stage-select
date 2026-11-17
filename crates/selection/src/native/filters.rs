//! The language and region filters (`modules/title_selection/filter_languages.py`, `filter_regions.py`).

use super::node::{COVERAGE_REMOVES, Groups, Run, Titles};
use super::py::PyResult;

/// `filter_languages`.
pub(crate) fn filter_languages(run: &mut Run, groups: &mut Groups) -> PyResult<()> {
    run.note("• Removing titles without specified languages... ");
    if !groups.is_empty() {
        run.trace.warn("\n\nStage: Filter languages");
    }
    let s = &run.s;
    let language_order: Vec<String> = match &s.system_language_order_user {
        Some(system) => {
            let mut order = system.clone();
            if !s.system_languages_user_found {
                for x in &s.language_order_user {
                    if !order.contains(x) {
                        order.push(x.clone());
                    }
                }
                for x in s.languages.values() {
                    if !order.contains(x) {
                        order.push(x.clone());
                    }
                }
            }
            order
        }
        None => s.language_order_user.clone(),
    };
    let names: Vec<String> = groups.keys().cloned().collect();
    for group in names {
        run.limits.check()?;
        let titles: Vec<usize> = groups[&group].iter().copied().collect();
        let mut supported = Titles::new();
        for &title in &titles {
            if run.n(title).languages.is_empty() {
                supported.insert(title);
                continue;
            }
            for code in &language_order {
                let languages = run.n(title).languages.clone();
                for language in &languages {
                    if run.search(code, language) {
                        supported.insert(title);
                    }
                }
            }
        }
        let report: Vec<usize> = titles
            .iter()
            .copied()
            .filter(|t| !supported.contains(t))
            .collect();
        if !report.is_empty() {
            run.trace_title("REF0049", &[language_order.join(", ")], &report, false);
        }
        for &title in &report {
            run.m(title).exclude_reason = "Language remove".into();
            run.remove_into("language_removes", title);
        }
        let kept: Titles = titles
            .iter()
            .copied()
            .filter(|t| supported.contains(t))
            .collect();
        if kept.is_empty() {
            groups.shift_remove(&group);
        } else {
            groups.insert(group, kept);
        }
    }
    run.note("• Removing titles without specified languages... done.");
    Ok(())
}

/// Excludes every title the Source Set's Coverage doesn't cover (ADR 0021), from `groups` and from the Operator's
/// includes, which can't bring one back. Not part of Retool.
pub(crate) fn filter_coverage(
    run: &mut Run,
    groups: &mut Groups,
    coverage: &[String],
) -> PyResult<()> {
    for id in 0..run.nodes.len() {
        if !title_select_api::coverage::covered(&run.n(id).regions, Some(coverage)) {
            run.uncovered.insert(id);
        }
    }
    let names: Vec<String> = groups.keys().cloned().collect();
    for group in names {
        run.limits.check()?;
        let mut kept = Titles::new();
        for &title in &groups[&group] {
            if run.uncovered.contains(&title) {
                run.m(title).exclude_reason = title_select_api::selection::COVERAGE_MESSAGE.into();
                run.remove_into(COVERAGE_REMOVES, title);
            } else {
                kept.insert(title);
            }
        }
        if kept.is_empty() {
            groups.shift_remove(&group);
        } else {
            groups.insert(group, kept);
        }
    }
    Ok(())
}

/// `filter_regions`.
pub(crate) fn filter_regions(run: &mut Run, groups: &mut Groups) -> PyResult<()> {
    run.note("• Removing titles without specified regions... ");
    let region_order = run.region_order();
    let names: Vec<String> = groups.keys().cloned().collect();
    for group in names {
        run.limits.check()?;
        let titles: Vec<usize> = groups[&group].iter().copied().collect();
        let mut kept = Titles::new();
        for &title in &titles {
            if region_order
                .iter()
                .any(|r| run.n(title).regions.contains(r))
            {
                kept.insert(title);
            } else {
                run.m(title).exclude_reason = "Region remove".into();
                run.remove_into("region_removes", title);
            }
        }
        if kept.is_empty() {
            groups.shift_remove(&group);
        } else {
            groups.insert(group, kept);
        }
    }
    run.note("• Removing titles without specified regions... done.");
    Ok(())
}
