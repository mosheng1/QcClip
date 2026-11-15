use std::collections::HashSet;

use crate::domain::clipboard::HistoryItemId;

/// Find the item nearest to `index` that still exists in `visible_ids`.
/// Forward neighbours are preferred at equal distance, then earlier ones.
pub(crate) fn nearest_survivor(
    previous_order: &[HistoryItemId],
    index: usize,
    visible_ids: &HashSet<HistoryItemId>,
) -> Option<HistoryItemId> {
    (0..previous_order.len()).find_map(|distance| {
        previous_order
            .get(index + distance)
            .filter(|id| visible_ids.contains(id))
            .or_else(|| {
                index
                    .checked_sub(distance)
                    .and_then(|candidate| previous_order.get(candidate))
                    .filter(|id| visible_ids.contains(id))
            })
            .copied()
    })
}
