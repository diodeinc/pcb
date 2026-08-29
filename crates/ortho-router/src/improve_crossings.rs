//! Improve crossings algorithm.
//!
//! This module implements libavoid's `improveCrossings()` algorithm to detect
//! and fix overlapping routes. The key insight is that crossing penalties should
//! only be applied during a rerouting phase, not during initial routing.
//!
//! ## Algorithm
//!
//! 1. Initial routing: Route all connectors without crossing penalty
//! 2. Detect overlapping pairs: Find segments from different nets that overlap
//! 3. Group overlapping connectors: Connectors that transitively overlap form groups
//! 4. Select connectors to reroute: Greedily select connectors with most overlaps
//! 5. Reroute with penalty: Clear selected routes and reroute with overlap penalty

use crate::config::RouterConfig;
use crate::pathfinder::{NetAwareContext, Pathfinder};
use crate::segment::{BendPointRegistry, Segment, SegmentRegistry};
use crate::types::{Connector, ExistingRouteSegment, RoutedPath};
use crate::visibility::VisibilityGraph;
use std::collections::{HashMap, HashSet};

/// Information about crossing/overlapping connectors.
#[derive(Debug, Default)]
pub struct CrossingConnectorsInfo {
    /// Groups of connectors that have crossings with each other.
    /// Each group is a map from connector index to the set of connector indices it crosses.
    groups: Vec<HashMap<usize, HashSet<usize>>>,
}

impl CrossingConnectorsInfo {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a crossing between two connectors.
    pub fn add_crossing(&mut self, conn1: usize, conn2: usize) {
        // Find which groups (if any) contain these connectors
        let group1_idx = self.find_group_containing(conn1);
        let group2_idx = self.find_group_containing(conn2);

        match (group1_idx, group2_idx) {
            (None, None) => {
                // Neither connector is in a group - create a new one
                let mut group = HashMap::new();
                group.insert(conn1, HashSet::from([conn2]));
                group.insert(conn2, HashSet::from([conn1]));
                self.groups.push(group);
            }
            (Some(idx), None) => {
                // conn1 is in a group, add conn2 to it
                let group = &mut self.groups[idx];
                group.entry(conn1).or_default().insert(conn2);
                group.insert(conn2, HashSet::from([conn1]));
            }
            (None, Some(idx)) => {
                // conn2 is in a group, add conn1 to it
                let group = &mut self.groups[idx];
                group.entry(conn2).or_default().insert(conn1);
                group.insert(conn1, HashSet::from([conn2]));
            }
            (Some(idx1), Some(idx2)) if idx1 == idx2 => {
                // Both in same group - just add the crossing
                let group = &mut self.groups[idx1];
                group.entry(conn1).or_default().insert(conn2);
                group.entry(conn2).or_default().insert(conn1);
            }
            (Some(idx1), Some(idx2)) => {
                // In different groups - merge them
                // Remove the second group first (to not invalidate idx1)
                let (idx_keep, idx_remove) = if idx1 < idx2 {
                    (idx1, idx2)
                } else {
                    (idx2, idx1)
                };
                let group_remove = self.groups.remove(idx_remove);

                // Merge into the kept group
                let group_keep = &mut self.groups[idx_keep];
                for (conn, crossings) in group_remove {
                    group_keep.entry(conn).or_default().extend(crossings);
                }

                // Add the new crossing
                group_keep.entry(conn1).or_default().insert(conn2);
                group_keep.entry(conn2).or_default().insert(conn1);
            }
        }
    }

    /// Find the index of the group containing the given connector.
    fn find_group_containing(&self, conn: usize) -> Option<usize> {
        self.groups.iter().position(|g| g.contains_key(&conn))
    }

    /// Get all groups.
    pub fn groups(&self) -> &[HashMap<usize, HashSet<usize>>] {
        &self.groups
    }

    /// Check if there are any crossings.
    pub fn has_crossings(&self) -> bool {
        !self.groups.is_empty()
    }
}

/// Select connectors to reroute from a crossing group.
///
/// Returns ALL connectors in the group, sorted by crossing count (ascending).
/// This ensures we try rerouting each connector, giving the ones with fewer
/// crossings priority (they route first, constraining later ones).
fn select_connectors_to_reroute(
    group: &HashMap<usize, HashSet<usize>>,
    _paths: &[RoutedPath],
) -> Vec<usize> {
    let mut result: Vec<usize> = group.keys().copied().collect();

    // Sort by crossing count (ascending) - connectors with fewer crossings first
    // This gives them priority during rerouting
    result.sort_by(|a, b| {
        let count_a = group.get(a).map(|c| c.len()).unwrap_or(0);
        let count_b = group.get(b).map(|c| c.len()).unwrap_or(0);
        count_a.cmp(&count_b)
    });

    result
}

/// Detect overlapping segments between paths from different nets.
fn detect_overlapping_pairs(paths: &[RoutedPath], net_ids: &[String]) -> CrossingConnectorsInfo {
    let mut info = CrossingConnectorsInfo::new();

    // Build segments for each path
    let path_segments: Vec<Vec<Segment>> = paths
        .iter()
        .map(|path| {
            let mut segments = Vec::new();
            for i in 1..path.points.len() {
                if let Some(seg) = Segment::from_points(&path.points[i - 1], &path.points[i]) {
                    segments.push(seg);
                }
            }
            segments
        })
        .collect();

    // Check each pair of paths
    for i in 0..paths.len() {
        for j in (i + 1)..paths.len() {
            // Skip if same net
            if net_ids[i] == net_ids[j] {
                continue;
            }

            // Check if any segments overlap
            let has_overlap = path_segments[i]
                .iter()
                .any(|seg_i| path_segments[j].iter().any(|seg_j| seg_i.overlaps(seg_j)));

            if has_overlap {
                info.add_crossing(i, j);
            }
        }
    }

    info
}

/// Improve crossings by rerouting overlapping connectors.
///
/// This implements libavoid's two-phase approach:
/// 1. Detect which connectors have overlapping segments
/// 2. Group overlapping connectors
/// 3. Select minimal set to reroute
/// 4. Reroute with crossing penalty enabled
pub fn improve_crossings(
    paths: &mut [RoutedPath],
    connectors: &[Connector],
    net_ids: &[String],
    graph: &VisibilityGraph,
    config: &RouterConfig,
    existing_segments: &[ExistingRouteSegment],
) {
    log::debug!(
        "[improve_crossings] Called with {} paths, {} connectors",
        paths.len(),
        connectors.len()
    );

    const MAX_ITERATIONS: usize = 5;
    for iteration in 0..MAX_ITERATIONS {
        // Step 1: Detect overlapping pairs
        let crossing_info = detect_overlapping_pairs(paths, net_ids);

        if !crossing_info.has_crossings() {
            log::debug!("[improve_crossings] Iteration {iteration}: No crossings - done!");
            return;
        }

        log::info!(
            "[improve_crossings] Iteration {}: Detected {} crossing groups",
            iteration,
            crossing_info.groups().len()
        );

        let mut any_rerouted = false;

        // Step 2: Process each group
        for (group_idx, group) in crossing_info.groups().iter().enumerate() {
            log::debug!(
                "[improve_crossings] Processing group {} with {} connectors",
                group_idx,
                group.len()
            );

            // Step 3: Select connectors to reroute
            let to_reroute = select_connectors_to_reroute(group, paths);

            if to_reroute.is_empty() {
                continue;
            }

            log::debug!(
                "[improve_crossings] Rerouting {} connectors: {:?}",
                to_reroute.len(),
                to_reroute
                    .iter()
                    .map(|&i| paths.get(i).map(|p| p.connector_id.as_str()).unwrap_or("?"))
                    .collect::<Vec<_>>()
            );

            // Step 4: Reroute each connector one at a time
            // For each reroute, rebuild the registry excluding that connector
            let pathfinder = Pathfinder::new(graph, config);

            for &path_idx in &to_reroute {
                let connector = &connectors[path_idx];
                let net_id = &net_ids[path_idx];

                // Build registry excluding the current connector
                let mut segment_registry = SegmentRegistry::new();
                let mut bend_registry = BendPointRegistry::new();
                crate::router::seed_existing_segments(&mut segment_registry, existing_segments);
                for (idx, path) in paths.iter().enumerate() {
                    if idx != path_idx && !path.points.is_empty() {
                        segment_registry.register_path(&path.points, &net_ids[idx]);
                        bend_registry.register_path(&path.points, &net_ids[idx]);
                    }
                }

                let net_context = NetAwareContext {
                    registry: &segment_registry,
                    bend_registry: &bend_registry,
                    net_id,
                    existing_segments,
                };

                match pathfinder.find_path_with_context(
                    &connector.source_port_id,
                    &connector.target_port_id,
                    Some(net_context),
                ) {
                    Some(result) => {
                        log::debug!(
                            "[improve_crossings] Rerouted '{}': {} points, {} bends, cost={:.1}",
                            connector.id,
                            result.points.len(),
                            result.bend_count,
                            result.cost
                        );

                        // Preserve the original net_id when rerouting
                        let original_net_id = paths[path_idx].net_id.clone();
                        paths[path_idx] = RoutedPath::with_net(
                            connector.id.clone(),
                            result.points,
                            original_net_id,
                        );
                        any_rerouted = true;
                    }
                    None => {
                        log::warn!(
                            "[improve_crossings] Failed to reroute connector '{}'",
                            connector.id
                        );
                    }
                }
            }
        }

        // If nothing was rerouted, we're stuck - exit loop
        if !any_rerouted {
            log::debug!(
                "[improve_crossings] Iteration {iteration}: No connectors were rerouted - stopping"
            );
            break;
        }
    }

    // Log final state
    let final_crossings = detect_overlapping_pairs(paths, net_ids);
    if final_crossings.has_crossings() {
        log::warn!(
            "[improve_crossings] {} crossing groups remain after iterations",
            final_crossings.groups().len()
        );
    } else {
        log::info!("[improve_crossings] All crossings resolved");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crossing_info_single_pair() {
        let mut info = CrossingConnectorsInfo::new();
        info.add_crossing(0, 1);

        assert_eq!(info.groups().len(), 1);
        assert!(info.groups()[0].contains_key(&0));
        assert!(info.groups()[0].contains_key(&1));
        assert!(info.groups()[0][&0].contains(&1));
        assert!(info.groups()[0][&1].contains(&0));
    }

    #[test]
    fn test_crossing_info_transitive() {
        let mut info = CrossingConnectorsInfo::new();
        info.add_crossing(0, 1);
        info.add_crossing(1, 2);

        // Should merge into one group
        assert_eq!(info.groups().len(), 1);
        assert!(info.groups()[0].contains_key(&0));
        assert!(info.groups()[0].contains_key(&1));
        assert!(info.groups()[0].contains_key(&2));
    }

    #[test]
    fn test_crossing_info_separate_groups() {
        let mut info = CrossingConnectorsInfo::new();
        info.add_crossing(0, 1);
        info.add_crossing(2, 3);

        // Should be two separate groups
        assert_eq!(info.groups().len(), 2);
    }

    #[test]
    fn test_crossing_info_merge_groups() {
        let mut info = CrossingConnectorsInfo::new();
        info.add_crossing(0, 1);
        info.add_crossing(2, 3);
        info.add_crossing(1, 2); // This should merge the two groups

        assert_eq!(info.groups().len(), 1);
        assert!(info.groups()[0].contains_key(&0));
        assert!(info.groups()[0].contains_key(&1));
        assert!(info.groups()[0].contains_key(&2));
        assert!(info.groups()[0].contains_key(&3));
    }
}
