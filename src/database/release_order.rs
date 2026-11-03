//! The user's manual order of a title's releases: a float weight on each `title_releases`
//! row (lower first, NULL = default order), dragged on the title page. A move places the
//! release halfway between its new neighbours; when that gap gets too small the title's
//! weights are renumbered (STEP apart). The weight is on the link row, so it moves with the
//! release when titles are merged or decoupled.

use sqlx::mysql::MySqlPool;

use super::error::DatabaseError;

/// Spacing of fresh / renumbered weights.
pub const STEP: f64 = 1024.0;
/// Below this gap between neighbours the title is renumbered.
pub const MIN_GAP: f64 = 1e-6;

/// The weights after moving `release_id` right after `after` (None: to the top), given
/// the title's releases as displayed now (weighted ones first, by weight). Releases without
/// a weight are placed after the weighted ones in their displayed order. Returns every
/// release's weight in the new order.
pub fn plan_move(current: &[(u64, Option<f64>)], release_id: u64, after: Option<u64>) -> Result<Vec<(u64, f64)>, String> {
    if !current.iter().any(|(id, _)| *id == release_id) {
        return Err(format!("release {release_id} is not on this title"));
    }
    if after == Some(release_id) {
        return Err("a release can't go after itself".to_string());
    }
    if let Some(a) = after {
        if !current.iter().any(|(id, _)| *id == a) {
            return Err(format!("release {a} is not on this title"));
        }
    }
    // Everyone weighted: the unweighted continue after the last weight.
    let mut last = current.iter().filter_map(|(_, w)| *w).fold(0.0_f64, f64::max);
    let mut list: Vec<(u64, f64)> = current
        .iter()
        .map(|(id, w)| {
            (*id, w.unwrap_or_else(|| {
                last += STEP;
                last
            }))
        })
        .collect();
    list.retain(|(id, _)| *id != release_id);
    let at = match after {
        None => 0,
        Some(a) => list.iter().position(|(id, _)| *id == a).map(|i| i + 1).unwrap_or(list.len()),
    };
    let prev = at.checked_sub(1).map(|i| list[i].1);
    let next = list.get(at).map(|x| x.1);
    let weight = match (prev, next) {
        (None, None) => Some(STEP),
        (None, Some(n)) => Some(n - STEP),
        (Some(p), None) => Some(p + STEP),
        (Some(p), Some(n)) if n - p >= 2.0 * MIN_GAP => Some((p + n) / 2.0),
        _ => None,
    };
    list.insert(at, (release_id, weight.unwrap_or(0.0)));
    if weight.is_none() {
        // Standardize: renumber the whole title.
        for (i, item) in list.iter_mut().enumerate() {
            item.1 = STEP * (i as f64 + 1.0);
        }
    }
    Ok(list)
}

/// Writes a title's weights (`plan_move`'s result).
pub async fn write_weights(pool: &MySqlPool, title_id: u64, weights: &[(u64, f64)]) -> Result<(), DatabaseError> {
    let mut tx = pool.begin().await?;
    for (release_id, weight) in weights {
        sqlx::query("UPDATE title_releases SET sort_weight = ?, sort_set_at = NOW() WHERE title_id = ? AND release_id = ?").bind(weight).bind(title_id).bind(release_id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Back to the default order for these titles (None: every title). Returns titles reset.
pub async fn clear(pool: &MySqlPool, title_ids: Option<&[u64]>) -> Result<u64, DatabaseError> {
    let filter = match title_ids {
        Some([]) => return Ok(0),
        Some(ids) => format!(" AND title_id IN ({})", ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",")),
        None => String::new(),
    };
    let titles: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(DISTINCT title_id) FROM title_releases WHERE sort_weight IS NOT NULL{filter}"))).fetch_one(pool).await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE title_releases SET sort_weight = NULL, sort_set_at = NULL WHERE sort_weight IS NOT NULL{filter}"))).execute(pool).await?;
    Ok(titles as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(w: &[(u64, f64)]) -> Vec<u64> {
        let mut v = w.to_vec();
        v.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
        v.into_iter().map(|x| x.0).collect()
    }

    #[test]
    fn first_move_seeds_the_displayed_order() {
        let now = [(1, None), (2, None), (3, None)];
        assert_eq!(order(&plan_move(&now, 3, None).unwrap()), vec![3, 1, 2]);
        assert_eq!(order(&plan_move(&now, 1, Some(3)).unwrap()), vec![2, 3, 1]);
        assert_eq!(order(&plan_move(&now, 1, Some(2)).unwrap()), vec![2, 1, 3]);
    }

    #[test]
    fn moves_land_between_neighbours() {
        let now = [(1, Some(1024.0)), (2, Some(2048.0)), (3, Some(3072.0)), (4, None)];
        let w = plan_move(&now, 3, Some(1)).unwrap();
        assert_eq!(order(&w), vec![1, 3, 2, 4]);
        assert_eq!(w.iter().find(|x| x.0 == 3).unwrap().1, 1536.0);
        assert!(plan_move(&now, 9, None).is_err());
        assert!(plan_move(&now, 2, Some(2)).is_err());
    }

    #[test]
    fn collapsing_gaps_renumber_the_title() {
        let mut now: Vec<(u64, Option<f64>)> = vec![(1, Some(1024.0)), (2, Some(2048.0)), (3, Some(3072.0))];
        // Keep dropping the last release right after the first one: the gap halves each time.
        for i in 0..80 {
            let moving = now.last().unwrap().0;
            let w = plan_move(&now, moving, Some(1)).unwrap();
            let mut sorted = w.clone();
            sorted.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
            for pair in sorted.windows(2) {
                assert!(pair[1].1 - pair[0].1 >= MIN_GAP, "gap collapsed at step {i}");
            }
            now = sorted.into_iter().map(|(id, w)| (id, Some(w))).collect();
        }
        assert!(now.iter().all(|(_, w)| w.unwrap() <= STEP * 4.0), "renumbering keeps weights small");
    }
}
