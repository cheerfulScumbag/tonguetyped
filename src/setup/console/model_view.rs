//! Filtering and sorting for the Model tab's Speech models table.
//!
//! The catalog itself never changes: a view is an ordered list of indices
//! into [`crate::catalog::ENTRIES`], so the selected model, its download
//! state, and the detail pane keep using the same stable catalog index while
//! the table shows a subset in a chosen order.
//!
//! Every choice here is session state, not configuration: it only decides
//! which rows the setup console paints, never what gets saved.

use crate::catalog::{CatalogModel, ENTRIES};

/// Which installation states the table shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum InstallFilter {
    #[default]
    Any,
    Installed,
    Missing,
}

impl InstallFilter {
    /// The next state the `i` key cycles to: any, then installed, then
    /// missing, then back to any.
    pub(super) fn next(self) -> Self {
        match self {
            Self::Any => Self::Installed,
            Self::Installed => Self::Missing,
            Self::Missing => Self::Any,
        }
    }

    pub(super) fn matches(self, installed: bool) -> bool {
        match self {
            Self::Any => true,
            Self::Installed => installed,
            Self::Missing => !installed,
        }
    }

    /// The chip shown while this filter is active; `None` for `Any`.
    pub(super) fn label(self) -> Option<&'static str> {
        match self {
            Self::Any => None,
            Self::Installed => Some("installed"),
            Self::Missing => Some("not installed"),
        }
    }
}

/// The row filters shown above the model table.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct ModelFilter {
    pub(super) install: InstallFilter,
    /// [`crate::catalog::VariantInfo::family_label`] to show, or every family.
    pub(super) family: Option<&'static str>,
    /// The entry's published quantization label, or every quantization.
    pub(super) quant: Option<&'static str>,
}

impl ModelFilter {
    /// Whether anything is hiding rows. Sorting never hides rows.
    pub(super) fn is_active(&self) -> bool {
        *self != Self::default()
    }

    pub(super) fn matches(&self, entry: &CatalogModel, installed: bool) -> bool {
        self.install.matches(installed)
            && self
                .family
                .is_none_or(|family| entry.info().family_label() == family)
            && self.quant.is_none_or(|quant| entry.quant == quant)
    }

    /// The active filter values in display order, for the status line.
    pub(super) fn chips(&self) -> Vec<&'static str> {
        let mut chips = Vec::new();
        if let Some(label) = self.install.label() {
            chips.push(label);
        }
        if let Some(family) = self.family {
            chips.push(family);
        }
        if let Some(quant) = self.quant {
            chips.push(quant);
        }
        chips
    }

    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }
}

/// The order the table lists matching rows in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum ModelSort {
    /// Catalog order, as reviewed in `catalog.rs`.
    #[default]
    Catalog,
    SizeAsc,
    SizeDesc,
    Id,
    Quant,
    /// Streaming models first.
    Live,
}

impl ModelSort {
    /// The next order the `s` key cycles to.
    pub(super) fn next(self) -> Self {
        match self {
            Self::Catalog => Self::SizeAsc,
            Self::SizeAsc => Self::SizeDesc,
            Self::SizeDesc => Self::Id,
            Self::Id => Self::Quant,
            Self::Quant => Self::Live,
            Self::Live => Self::Catalog,
        }
    }

    /// The label the status line shows for a non-default order.
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Catalog => "catalog order",
            Self::SizeAsc => "size \u{2191}",
            Self::SizeDesc => "size \u{2193}",
            Self::Id => "id \u{2191}",
            Self::Quant => "quant \u{2191}",
            Self::Live => "live first",
        }
    }
}

/// Every family label present in the catalog, in catalog order, for the `f`
/// key to cycle through.
pub(super) fn families() -> Vec<&'static str> {
    let mut families: Vec<&'static str> = Vec::new();
    for entry in ENTRIES {
        let family = entry.info().family_label();
        if !families.contains(&family) {
            families.push(family);
        }
    }
    families
}

/// Every quantization label present in the catalog, in catalog order, for
/// the `t` key to cycle through.
pub(super) fn quants() -> Vec<&'static str> {
    let mut quants: Vec<&'static str> = Vec::new();
    for entry in ENTRIES {
        if !quants.contains(&entry.quant) {
            quants.push(entry.quant);
        }
    }
    quants
}

/// The next value in a cycle that starts at every (`None`) and wraps back to
/// it after the last option.
pub(super) fn cycle(
    options: &[&'static str],
    current: Option<&'static str>,
) -> Option<&'static str> {
    match current {
        None => options.first().copied(),
        Some(current) => options
            .iter()
            .position(|option| *option == current)
            .and_then(|index| options.get(index + 1))
            .copied(),
    }
}

/// The catalog indices matching `filter`, in `sort` order. `installed` is
/// indexed in parallel with [`ENTRIES`]. Sorts are stable, so rows that tie
/// on the sort key keep their catalog order.
pub(super) fn visible(filter: ModelFilter, sort: ModelSort, installed: &[bool]) -> Vec<usize> {
    let mut rows: Vec<usize> = ENTRIES
        .iter()
        .enumerate()
        .filter(|(index, entry)| {
            filter.matches(entry, installed.get(*index).copied().unwrap_or(false))
        })
        .map(|(index, _)| index)
        .collect();
    let entry = |index: &usize| &ENTRIES[*index];
    match sort {
        ModelSort::Catalog => {}
        ModelSort::SizeAsc => rows.sort_by_key(|index| entry(index).size_bytes),
        ModelSort::SizeDesc => rows.sort_by_key(|index| std::cmp::Reverse(entry(index).size_bytes)),
        ModelSort::Id => rows.sort_by_key(|index| entry(index).id),
        ModelSort::Quant => rows.sort_by_key(|index| entry(index).quant),
        ModelSort::Live => rows.sort_by_key(|index| !entry(index).info().supports_streaming()),
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index_of(id: &str) -> usize {
        ENTRIES
            .iter()
            .position(|entry| entry.id == id)
            .unwrap_or_else(|| panic!("catalog has no {id}"))
    }

    fn installed_only(ids: &[&str]) -> Vec<bool> {
        ENTRIES
            .iter()
            .map(|entry| ids.contains(&entry.id))
            .collect()
    }

    #[test]
    fn install_filter_matches_each_state() {
        let entry = &ENTRIES[index_of("whisper-small-q5_k_m")];
        assert!(InstallFilter::Any.matches(true));
        assert!(InstallFilter::Any.matches(false));
        assert!(InstallFilter::Installed.matches(true));
        assert!(!InstallFilter::Installed.matches(false));
        assert!(!InstallFilter::Missing.matches(true));
        assert!(InstallFilter::Missing.matches(false));
        assert!(ModelFilter::default().matches(entry, false));
        assert!(ModelFilter {
            install: InstallFilter::Installed,
            ..ModelFilter::default()
        }
        .matches(entry, true));
    }

    #[test]
    fn family_filter_keeps_only_that_family() {
        let filter = ModelFilter {
            family: Some("Whisper"),
            ..ModelFilter::default()
        };
        let rows = visible(filter, ModelSort::Catalog, &[]);
        assert!(!rows.is_empty());
        assert!(rows
            .iter()
            .all(|index| ENTRIES[*index].info().family_label() == "Whisper"));
        assert!(rows.contains(&index_of("whisper-small-q5_k_m")));
        assert!(!rows.contains(&index_of("parakeet-tdt-0.6b-v2-f16")));
    }

    #[test]
    fn quant_filter_keeps_only_that_quantization() {
        let filter = ModelFilter {
            quant: Some("Q5_K_M"),
            ..ModelFilter::default()
        };
        let rows = visible(filter, ModelSort::Catalog, &[]);
        assert!(!rows.is_empty());
        assert!(rows.iter().all(|index| ENTRIES[*index].quant == "Q5_K_M"));
        assert!(rows.contains(&index_of("whisper-small-q5_k_m")));
        assert!(!rows.contains(&index_of("whisper-small-f16")));
    }

    #[test]
    fn filters_combine_and_the_installed_one_follows_the_installed_list() {
        let installed = installed_only(&["whisper-small-q5_k_m", "whisper-tiny-q5_k_m"]);
        let filter = ModelFilter {
            install: InstallFilter::Installed,
            family: Some("Whisper"),
            quant: Some("Q5_K_M"),
        };
        let rows = visible(filter, ModelSort::Catalog, &installed);
        let ids: Vec<&str> = rows.iter().map(|index| ENTRIES[*index].id).collect();
        assert_eq!(ids, ["whisper-small-q5_k_m", "whisper-tiny-q5_k_m"]);
    }

    #[test]
    fn clear_restores_the_full_catalog() {
        let mut filter = ModelFilter {
            install: InstallFilter::Missing,
            family: Some("Canary"),
            quant: Some("F16"),
        };
        assert!(filter.is_active());
        filter.clear();
        assert!(!filter.is_active());
        assert_eq!(
            visible(filter, ModelSort::Catalog, &[]).len(),
            ENTRIES.len()
        );
    }

    #[test]
    fn cycles_visit_every_option_then_return_to_all() {
        let families = families();
        assert!(families.contains(&"Whisper"));
        let mut current = None;
        for family in &families {
            current = cycle(&families, current);
            assert_eq!(current, Some(*family));
        }
        assert_eq!(cycle(&families, current), None);

        let quants = quants();
        assert!(quants.contains(&"Q5_K_M"));
        let mut current = None;
        for quant in &quants {
            current = cycle(&quants, current);
            assert_eq!(current, Some(*quant));
        }
        assert_eq!(cycle(&quants, current), None);
    }

    #[test]
    fn size_sorts_are_ordered_and_stable() {
        for sort in [ModelSort::SizeAsc, ModelSort::SizeDesc] {
            let rows = visible(ModelFilter::default(), sort, &[]);
            assert_eq!(rows.len(), ENTRIES.len());
            for pair in rows.windows(2) {
                let first = ENTRIES[pair[0]].size_bytes;
                let second = ENTRIES[pair[1]].size_bytes;
                match sort {
                    ModelSort::SizeAsc => assert!(first <= second, "{first} > {second}"),
                    ModelSort::SizeDesc => assert!(first >= second, "{first} < {second}"),
                    _ => unreachable!(),
                }
            }
            // Equal sizes keep catalog order: stability, so re-sorting never
            // makes rows jump around.
            for pair in rows.windows(2) {
                if ENTRIES[pair[0]].size_bytes == ENTRIES[pair[1]].size_bytes {
                    assert!(pair[0] < pair[1], "equal sizes swapped: {pair:?}");
                }
            }
        }
    }

    #[test]
    fn id_and_quant_sorts_are_ordered_and_stable() {
        for sort in [ModelSort::Id, ModelSort::Quant] {
            let rows = visible(ModelFilter::default(), sort, &[]);
            assert_eq!(rows.len(), ENTRIES.len());
            for pair in rows.windows(2) {
                let first = &ENTRIES[pair[0]];
                let second = &ENTRIES[pair[1]];
                let (first, second) = match sort {
                    ModelSort::Id => (first.id, second.id),
                    _ => (first.quant, second.quant),
                };
                assert!(first <= second, "{first} > {second}");
                if first == second {
                    assert!(pair[0] < pair[1], "equal keys swapped: {pair:?}");
                }
            }
        }
    }

    #[test]
    fn live_sort_puts_streaming_models_first_and_keeps_catalog_order() {
        let rows = visible(ModelFilter::default(), ModelSort::Live, &[]);
        let first_offline = rows
            .iter()
            .position(|index| !ENTRIES[*index].info().supports_streaming())
            .unwrap_or(rows.len());
        assert!(rows[..first_offline]
            .iter()
            .all(|index| ENTRIES[*index].info().supports_streaming()));
        assert!(rows[first_offline..]
            .iter()
            .all(|index| !ENTRIES[*index].info().supports_streaming()));
        assert!(first_offline > 0 && first_offline < rows.len());
        for pair in rows[..first_offline].windows(2) {
            assert!(pair[0] < pair[1], "streaming rows left catalog order");
        }
    }
}
