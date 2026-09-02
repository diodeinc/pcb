//! Minimum node cut over the physical adjacency graph.
//!
//! Wires and junctions are cuttable nodes with a cost; connection points,
//! labels, pins, and name merges are uncuttable. The cut separates two groups
//! of terminals with the least total cost, which is the smallest set of items
//! whose removal splits them in the geometric model. The planner verifies the
//! result through the real reducer before trusting it.

use std::collections::{BTreeSet, VecDeque};

use crate::connectivity::CutGraph;

const INFINITE: u64 = 1 << 40;

struct Arc {
    to: usize,
    capacity: u64,
    reverse: usize,
}

struct FlowNetwork {
    adjacency: Vec<Vec<Arc>>,
}

impl FlowNetwork {
    fn new(nodes: usize) -> Self {
        Self {
            adjacency: (0..nodes).map(|_| Vec::new()).collect(),
        }
    }

    fn add_arc(&mut self, from: usize, to: usize, capacity: u64) {
        let forward_index = self.adjacency[from].len();
        let reverse_index = self.adjacency[to].len();
        self.adjacency[from].push(Arc {
            to,
            capacity,
            reverse: reverse_index,
        });
        self.adjacency[to].push(Arc {
            to: from,
            capacity: 0,
            reverse: forward_index,
        });
    }

    /// Edmonds-Karp, bounded so an unreachable sink never loops.
    fn max_flow(&mut self, source: usize, sink: usize, limit: u64) -> u64 {
        let mut flow = 0;
        while flow < limit {
            let mut parent = vec![None::<(usize, usize)>; self.adjacency.len()];
            let mut queue = VecDeque::from([source]);
            parent[source] = Some((source, usize::MAX));
            while let Some(node) = queue.pop_front() {
                if node == sink {
                    break;
                }
                for (index, arc) in self.adjacency[node].iter().enumerate() {
                    if arc.capacity > 0 && parent[arc.to].is_none() {
                        parent[arc.to] = Some((node, index));
                        queue.push_back(arc.to);
                    }
                }
            }
            if parent[sink].is_none() {
                break;
            }
            let mut bottleneck = limit - flow;
            let mut node = sink;
            while node != source {
                let (previous, index) = parent[node].expect("path node has a parent");
                bottleneck = bottleneck.min(self.adjacency[previous][index].capacity);
                node = previous;
            }
            let mut node = sink;
            while node != source {
                let (previous, index) = parent[node].expect("path node has a parent");
                self.adjacency[previous][index].capacity -= bottleneck;
                let reverse = self.adjacency[previous][index].reverse;
                self.adjacency[node][reverse].capacity += bottleneck;
                node = previous;
            }
            flow += bottleneck;
        }
        flow
    }

    fn reachable(&self, source: usize) -> Vec<bool> {
        let mut seen = vec![false; self.adjacency.len()];
        let mut queue = VecDeque::from([source]);
        seen[source] = true;
        while let Some(node) = queue.pop_front() {
            for arc in &self.adjacency[node] {
                if arc.capacity > 0 && !seen[arc.to] {
                    seen[arc.to] = true;
                    queue.push_back(arc.to);
                }
            }
        }
        seen
    }
}

/// The least-cost set of cuttable nodes separating every source from every
/// sink, or `None` when no finite cut exists (the groups touch through
/// uncuttable nodes, or a node is both source and sink).
///
/// `cost` returns `Some(cost)` for cuttable nodes and `None` for uncuttable
/// ones. Deterministic for a fixed graph and cost function.
pub(crate) fn minimum_node_cut(
    graph: &CutGraph,
    sources: &BTreeSet<usize>,
    sinks: &BTreeSet<usize>,
    cost: impl Fn(usize) -> Option<u64>,
) -> Option<Vec<usize>> {
    if sources.is_empty() || sinks.is_empty() || !sources.is_disjoint(sinks) {
        return None;
    }
    let nodes = graph.nodes.len();
    let entry = |node: usize| 2 * node;
    let exit = |node: usize| 2 * node + 1;
    let source = 2 * nodes;
    let sink = 2 * nodes + 1;
    let mut network = FlowNetwork::new(2 * nodes + 2);
    let mut costs = Vec::with_capacity(nodes);
    for node in 0..nodes {
        let node_cost = cost(node);
        costs.push(node_cost);
        network.add_arc(entry(node), exit(node), node_cost.unwrap_or(INFINITE));
    }
    // An undirected edge lets flow leave either node and enter the other, so
    // every path through a node still pays that node's entry-to-exit arc.
    for (a, b) in &graph.edges {
        network.add_arc(exit(*a), entry(*b), INFINITE);
        network.add_arc(exit(*b), entry(*a), INFINITE);
    }
    for node in sources {
        network.add_arc(source, entry(*node), INFINITE);
    }
    for node in sinks {
        network.add_arc(exit(*node), sink, INFINITE);
    }
    let flow = network.max_flow(source, sink, INFINITE);
    if flow >= INFINITE {
        return None;
    }
    let reachable = network.reachable(source);
    let cut = (0..nodes)
        .filter(|node| costs[*node].is_some() && reachable[entry(*node)] && !reachable[exit(*node)])
        .collect::<Vec<_>>();
    (!cut.is_empty()).then_some(cut)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectivity::CutNode;

    fn graph(nodes: usize, edges: &[(usize, usize)]) -> CutGraph {
        CutGraph {
            nodes: (0..nodes).map(|_| CutNode::default()).collect(),
            edges: edges.to_vec(),
        }
    }

    #[test]
    fn cuts_the_single_bridge_in_a_chain() {
        // 0 - 1 - 2 - 3 - 4, only node 2 cuttable.
        let graph = graph(5, &[(0, 1), (1, 2), (2, 3), (3, 4)]);
        let cut = minimum_node_cut(&graph, &BTreeSet::from([0]), &BTreeSet::from([4]), |node| {
            (node == 2).then_some(1)
        });
        assert_eq!(cut, Some(vec![2]));
    }

    #[test]
    fn prefers_one_expensive_node_over_two_cheap_parallel_paths() {
        // 0 - {1, 2} - 3 - 4 - 5: nodes 1 and 2 cost 1 each, node 4 costs 1.
        let graph = graph(6, &[(0, 1), (0, 2), (1, 3), (2, 3), (3, 4), (4, 5)]);
        let cut = minimum_node_cut(&graph, &BTreeSet::from([0]), &BTreeSet::from([5]), |node| {
            matches!(node, 1 | 2 | 4).then_some(1)
        });
        assert_eq!(cut, Some(vec![4]));
    }

    #[test]
    fn parallel_duplicates_are_both_cut_when_they_are_the_only_bridge() {
        let graph = graph(4, &[(0, 1), (0, 2), (1, 3), (2, 3)]);
        let cut = minimum_node_cut(&graph, &BTreeSet::from([0]), &BTreeSet::from([3]), |node| {
            matches!(node, 1 | 2).then_some(1)
        });
        assert_eq!(cut, Some(vec![1, 2]));
    }

    #[test]
    fn no_cut_exists_through_uncuttable_nodes() {
        let graph = graph(3, &[(0, 1), (1, 2)]);
        assert_eq!(
            minimum_node_cut(&graph, &BTreeSet::from([0]), &BTreeSet::from([2]), |_| None),
            None
        );
    }
}
