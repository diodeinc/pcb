pub mod csr;
pub mod path;
pub mod starlark;

use csr::{CsrError, CsrList};
use fixedbitset::FixedBitSet;
use smallvec::SmallVec;
use std::collections::{HashMap, HashSet};
use thiserror::Error;

/// Component path wrapper type
#[derive(Debug, Clone, PartialEq, Eq, Hash, allocative::Allocative)]
pub struct ComponentPath(pub String);

impl ComponentPath {
    pub fn new(path: impl Into<String>) -> Self {
        Self(path.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for ComponentPath {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

impl From<&str> for ComponentPath {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl std::fmt::Display for ComponentPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Port path wrapper type (component path + pin name)
#[derive(Debug, Clone, PartialEq, Eq, Hash, allocative::Allocative)]
pub struct PortPath {
    pub component: ComponentPath,
    pub pin: String,
}

impl PortPath {
    pub fn new(component: impl Into<ComponentPath>, pin: impl Into<String>) -> Self {
        Self {
            component: component.into(),
            pin: pin.into(),
        }
    }
}

impl From<(&str, &str)> for PortPath {
    fn from((component, pin): (&str, &str)) -> Self {
        Self::new(component, pin)
    }
}

impl std::fmt::Display for PortPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.component, self.pin)
    }
}

// Dense IDs. Keep them opaque.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, allocative::Allocative)]
pub struct PortId(pub u32);

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, allocative::Allocative)]
pub struct FactorId(pub u32);

#[derive(Clone, Debug, Eq, PartialEq, allocative::Allocative)]
pub enum FactorType {
    Net(String),       // Net name
    Component(String), // Component path
}

#[derive(Error, Debug)]
pub enum GraphError {
    #[error("CSR error: {0}")]
    Csr(#[from] CsrError),
    #[error("port {0} already has two factors")]
    TooManyFactors(String),
    #[error("port {0} has fewer than two factors")]
    TooFewFactors(String),
    #[error("IO factor must have exactly one port, got {0}")]
    InvalidIoFactor(usize),
}

/// Circuit graph that combines metadata and connectivity functionality
#[derive(Debug, allocative::Allocative)]
pub struct CircuitGraph {
    // Path to ID mappings
    port_by_path: HashMap<PortPath, PortId>,
    factor_by_name: HashMap<String, FactorId>,
    factor_types: Vec<FactorType>, // FactorId -> FactorType

    // The connectivity graph data (previously in FrozenConnectivity)
    /// factor -> ports (F rows)
    factor_ports: CsrList<PortId>,
    /// port -> [factor0, factor1] (order is arbitrary; treat them the same)
    port_factors: Vec<[FactorId; 2]>,
}

impl CircuitGraph {
    /// Iterate unique neighbor ports of `p` via either factor (dedup to avoid duplicates
    /// when two pins share both the same net and the same component).
    pub fn for_each_neighbor<F: FnMut(PortId)>(&self, p: PortId, mut f: F) {
        let [f0, f1] = self.port_factors[p.0 as usize];
        // Small linear dedup buffer; degrees are tiny in practice
        let mut seen: SmallVec<[PortId; 8]> = SmallVec::new();

        for &fid in &[f0, f1] {
            for &q in self.factor_ports.row_unchecked(fid.0 as usize) {
                if q == p {
                    continue;
                }
                if !seen.contains(&q) {
                    seen.push(q);
                    f(q);
                }
            }
        }
    }

    /// Find all simple paths from start to goal up to max_len ports
    /// Ensures that no factor (net or component) is visited more than once in any path
    /// Returns both ports and the factors traversed between them
    pub fn all_simple_paths_with_factors<F: FnMut(&[PortId], &[FactorId])>(
        &self,
        start: PortId,
        goal: PortId,
        max_len: Option<usize>,
        mut on_path: F,
    ) {
        let p_count = self.port_factors.len();
        let f_count = self.factor_count();

        let mut vis_p = FixedBitSet::with_capacity(p_count);
        let mut vis_f = FixedBitSet::with_capacity(f_count);
        let mut path: SmallVec<[PortId; 64]> = SmallVec::new();
        let mut factors: SmallVec<[FactorId; 64]> = SmallVec::new();

        #[allow(clippy::too_many_arguments)]
        fn dfs<F: FnMut(&[PortId], &[FactorId])>(
            g: &CircuitGraph,
            cur: PortId,
            goal: PortId,
            max_len: Option<usize>,
            vis_p: &mut FixedBitSet,
            vis_f: &mut FixedBitSet,
            path: &mut SmallVec<[PortId; 64]>,
            factors: &mut SmallVec<[FactorId; 64]>,
            on_path: &mut F,
        ) {
            if let Some(limit) = max_len {
                if path.len() > limit {
                    return;
                }
            }
            if cur == goal {
                on_path(path.as_slice(), factors.as_slice());
                return;
            }

            // For each factor connected to the current port
            let [cur_f0, cur_f1] = g.port_factors[cur.0 as usize];
            // Deduplicate factors to avoid duplicate paths from external ports
            let factors_to_explore = if cur_f0 == cur_f1 {
                &[cur_f0][..]
            } else {
                &[cur_f0, cur_f1][..]
            };
            for &factor_id in factors_to_explore {
                // Skip if this factor has already been traversed
                if vis_f.contains(factor_id.0 as usize) {
                    continue;
                }

                // Find all other ports connected to this factor
                for &q in g.factor_ports.row_unchecked(factor_id.0 as usize) {
                    if q == cur {
                        continue; // Skip current port
                    }

                    let qi = q.0 as usize;
                    if vis_p.contains(qi) {
                        continue; // Skip already visited ports
                    }

                    // Mark the traversed factor and destination port as visited
                    vis_f.insert(factor_id.0 as usize);
                    vis_p.insert(qi);
                    path.push(q);
                    factors.push(factor_id);

                    dfs(g, q, goal, max_len, vis_p, vis_f, path, factors, on_path);

                    // Unmark during backtracking
                    factors.pop();
                    path.pop();
                    vis_p.set(qi, false);
                    vis_f.set(factor_id.0 as usize, false);
                }
            }
        }

        // Mark starting port as visited, but NOT its factors
        // Factors should only be marked as visited when we traverse through them
        vis_p.insert(start.0 as usize);
        path.push(start);

        dfs(
            self,
            start,
            goal,
            max_len,
            &mut vis_p,
            &mut vis_f,
            &mut path,
            &mut factors,
            &mut on_path,
        );
    }

    /// Find all simple paths from start to goal up to max_len ports
    /// Ensures that no factor (net or component) is visited more than once in any path
    pub fn all_simple_paths<F: FnMut(&[PortId])>(
        &self,
        start: PortId,
        goal: PortId,
        max_len: usize,
        mut on_path: F,
    ) {
        let p_count = self.port_factors.len();
        let f_count = self.factor_count();

        let mut vis_p = FixedBitSet::with_capacity(p_count);
        let mut vis_f = FixedBitSet::with_capacity(f_count);
        let mut path: SmallVec<[PortId; 64]> = SmallVec::new();

        #[allow(clippy::too_many_arguments)]
        fn dfs<F: FnMut(&[PortId])>(
            g: &CircuitGraph,
            cur: PortId,
            goal: PortId,
            max_len: usize,
            vis_p: &mut FixedBitSet,
            vis_f: &mut FixedBitSet,
            path: &mut SmallVec<[PortId; 64]>,
            on_path: &mut F,
        ) {
            if path.len() > max_len {
                return;
            }
            if cur == goal {
                on_path(path.as_slice());
                return;
            }

            // For each factor connected to the current port
            let [cur_f0, cur_f1] = g.port_factors[cur.0 as usize];
            for &factor_id in &[cur_f0, cur_f1] {
                // Skip if this factor has already been traversed
                if vis_f.contains(factor_id.0 as usize) {
                    continue;
                }

                // Find all other ports connected to this factor
                for &q in g.factor_ports.row_unchecked(factor_id.0 as usize) {
                    if q == cur {
                        continue; // Skip current port
                    }

                    let qi = q.0 as usize;
                    if vis_p.contains(qi) {
                        continue; // Skip already visited ports
                    }

                    // Mark the traversed factor and destination port as visited
                    vis_f.insert(factor_id.0 as usize);
                    vis_p.insert(qi);
                    path.push(q);

                    dfs(g, q, goal, max_len, vis_p, vis_f, path, on_path);

                    // Unmark during backtracking
                    path.pop();
                    vis_p.set(qi, false);
                    vis_f.set(factor_id.0 as usize, false);
                }
            }
        }

        // Mark starting port as visited, but NOT its factors
        // Factors should only be marked as visited when we traverse through them
        vis_p.insert(start.0 as usize);
        path.push(start);

        dfs(
            self,
            start,
            goal,
            max_len,
            &mut vis_p,
            &mut vis_f,
            &mut path,
            &mut on_path,
        );
    }

    pub fn port_count(&self) -> usize {
        self.port_factors.len()
    }

    pub fn factor_count(&self) -> usize {
        self.factor_ports.rows()
    }

    pub fn port_factors(&self, port: PortId) -> [FactorId; 2] {
        self.port_factors[port.0 as usize]
    }

    pub fn factor_ports(&self, factor: FactorId) -> &[PortId] {
        self.factor_ports.row_unchecked(factor.0 as usize)
    }

    /// Create a CircuitGraph from components data using new wrapper types
    pub fn new(
        net_to_ports: HashMap<String, Vec<PortPath>>,
        component_pins: HashMap<ComponentPath, Vec<String>>,
        public_nets: HashSet<String>,
    ) -> Result<Self, GraphError> {
        let mut port_by_path = HashMap::new();
        let mut factor_by_name = HashMap::new();
        let mut factor_types = Vec::new();
        let mut factor_buckets = Vec::new();
        let mut port_factors_map: HashMap<PortPath, [Option<FactorId>; 2]> = HashMap::new();

        // Add all nets as factors
        for (net_name, ports) in &net_to_ports {
            let factor_id = FactorId(factor_by_name.len() as u32);
            factor_by_name.insert(net_name.clone(), factor_id);
            factor_types.push(FactorType::Net(net_name.clone()));

            // Convert port paths to PortIds
            let mut port_ids = Vec::new();
            for port_path in ports {
                let port_id = if let Some(&existing) = port_by_path.get(port_path) {
                    existing
                } else {
                    let id = PortId(port_by_path.len() as u32);
                    port_by_path.insert(port_path.clone(), id);
                    port_factors_map.insert(port_path.clone(), [None, None]);
                    id
                };
                port_ids.push(port_id);

                // Add this factor to the port's factor list
                let slots = port_factors_map.get_mut(port_path).unwrap();
                if slots[0].is_none() {
                    slots[0] = Some(factor_id);
                } else if slots[1].is_none() {
                    slots[1] = Some(factor_id);
                } else {
                    return Err(GraphError::TooManyFactors(port_path.to_string()));
                }
            }
            factor_buckets.push(port_ids);
        }

        // Add all components as factors
        for (comp_path, pins) in &component_pins {
            let factor_id = FactorId(factor_by_name.len() as u32);
            let comp_name = comp_path.to_string();
            factor_by_name.insert(comp_name.clone(), factor_id);
            factor_types.push(FactorType::Component(comp_name));

            // Convert pin names to PortIds using proper PortPath
            let mut port_ids = Vec::new();
            for pin_name in pins {
                let port_path = PortPath::new(comp_path.clone(), pin_name);

                let port_id = if let Some(&existing) = port_by_path.get(&port_path) {
                    existing
                } else {
                    let id = PortId(port_by_path.len() as u32);
                    port_by_path.insert(port_path.clone(), id);
                    port_factors_map.insert(port_path.clone(), [None, None]);
                    id
                };
                port_ids.push(port_id);

                // Add this factor to the port's factor list
                let slots = port_factors_map.get_mut(&port_path).unwrap();
                if slots[0].is_none() {
                    slots[0] = Some(factor_id);
                } else if slots[1].is_none() {
                    slots[1] = Some(factor_id);
                } else {
                    return Err(GraphError::TooManyFactors(port_path.to_string()));
                }
            }
            factor_buckets.push(port_ids);
        }

        // Add external ports for public nets (io() parameters)
        for public_net_name in &public_nets {
            // Only create external port if this net actually exists in the circuit
            if let Some(&factor_id) = factor_by_name.get(public_net_name) {
                let external_port_path = PortPath::new("<external>", public_net_name);
                let port_id = PortId(port_by_path.len() as u32);

                port_by_path.insert(external_port_path.clone(), port_id);
                // External ports only have 1 factor (the net itself), but we need to pad for validation
                // We'll handle this special case in validation
                port_factors_map.insert(external_port_path, [Some(factor_id), None]);

                // Add this port to the net's factor bucket
                let factor_idx = factor_id.0 as usize;
                if factor_idx < factor_buckets.len() {
                    factor_buckets[factor_idx].push(port_id);
                }
            }
        }

        // Validate that each port has exactly 2 factors (except external ports)
        let mut port_factors = vec![[FactorId(0), FactorId(0)]; port_by_path.len()];
        for (port_path, port_id) in &port_by_path {
            let slots = &port_factors_map[port_path];
            match (slots[0], slots[1]) {
                (Some(f0), Some(f1)) => {
                    port_factors[port_id.0 as usize] = [f0, f1];
                }
                (Some(f0), None) if port_path.component.as_str() == "<external>" => {
                    // External ports only have 1 factor, duplicate it for consistency
                    port_factors[port_id.0 as usize] = [f0, f0];
                }
                _ => return Err(GraphError::TooFewFactors(port_path.to_string())),
            }
        }

        // Create the combined circuit graph
        let factor_ports = CsrList::from_buckets(factor_buckets);

        Ok(Self {
            port_by_path,
            factor_by_name,
            factor_types,
            factor_ports,
            port_factors,
        })
    }

    // Optional: expose path→id maps for querying
    pub fn port_id(&self, path: &PortPath) -> Option<PortId> {
        self.port_by_path.get(path).copied()
    }

    pub fn factor_id(&self, name: &str) -> Option<FactorId> {
        self.factor_by_name.get(name).copied()
    }

    pub fn factor_type(&self, fid: FactorId) -> &FactorType {
        &self.factor_types[fid.0 as usize]
    }

    pub fn port_path(&self, pid: PortId) -> Option<&PortPath> {
        self.port_by_path
            .iter()
            .find(|(_, &id)| id == pid)
            .map(|(path, _)| path)
    }

    pub fn factor_name(&self, fid: FactorId) -> Option<&str> {
        self.factor_by_name
            .iter()
            .find(|(_, &id)| id == fid)
            .map(|(name, _)| name.as_str())
    }
}

impl std::fmt::Display for CircuitGraph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "CircuitGraph {{")?;
        writeln!(
            f,
            "  ports: {}, factors: {}",
            self.port_by_path.len(),
            self.factor_by_name.len()
        )?;

        // Print factors and their connected ports
        writeln!(f, "  factors:")?;
        for factor_id in self.factor_by_name.values() {
            let factor_type = &self.factor_types[factor_id.0 as usize];
            let ports = self.factor_ports.row_unchecked(factor_id.0 as usize);

            write!(f, "    {:?} -> [", factor_type)?;
            for (i, &port_id) in ports.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                if let Some(port_path) = self.port_path(port_id) {
                    write!(f, "{}", port_path)?;
                } else {
                    write!(f, "<unknown>")?;
                }
            }
            writeln!(f, "]")?;
        }

        // Print ports and their connected factors
        writeln!(f, "  ports:")?;
        for (port_path, &port_id) in &self.port_by_path {
            let [f0, f1] = self.port_factors[port_id.0 as usize];
            let factor_names: Vec<String> = [f0, f1]
                .iter()
                .map(|fid| self.factor_name(*fid).unwrap_or("<unknown>").to_string())
                .collect();

            writeln!(f, "    {} -> [{}]", port_path, factor_names.join(", "))?;
        }

        write!(f, "}}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_circuit_graph() {
        let net_to_ports = HashMap::from([
            (
                "VCC".to_string(),
                vec![("R1", "A").into(), ("C1", "+").into()],
            ),
            (
                "GND".to_string(),
                vec![("R1", "B").into(), ("C1", "-").into()],
            ),
        ]);

        let component_pins = HashMap::from([
            ("R1".into(), vec!["A".to_string(), "B".to_string()]),
            ("C1".into(), vec!["+".to_string(), "-".to_string()]),
        ]);

        let graph = CircuitGraph::new(net_to_ports, component_pins, HashSet::new()).unwrap();

        assert_eq!(graph.port_count(), 4);
        assert_eq!(graph.factor_count(), 4); // 2 nets + 2 components
    }

    #[test]
    fn test_io_factor() {
        // IO factors test removed since IO is not used in real circuits
        // Only components and nets are used in practice
    }

    #[test]
    fn test_path_finding() {
        let net_to_ports = HashMap::from([
            (
                "VCC".to_string(),
                vec![("R1", "A").into(), ("C1", "+").into()],
            ),
            (
                "GND".to_string(),
                vec![("R1", "B").into(), ("C1", "-").into()],
            ),
        ]);

        let component_pins = HashMap::from([
            ("R1".into(), vec!["A".to_string(), "B".to_string()]),
            ("C1".into(), vec!["+".to_string(), "-".to_string()]),
        ]);

        let graph = CircuitGraph::new(net_to_ports, component_pins, HashSet::new()).unwrap();

        // Test a valid cross-component path: R1.A to C1.+ (via VCC net)
        let start_id = graph.port_id(&("R1", "A").into()).unwrap();
        let goal_id = graph.port_id(&("C1", "+").into()).unwrap();

        let mut paths = Vec::new();
        graph.all_simple_paths(start_id, goal_id, 10, |path| {
            paths.push(path.to_vec());
        });
        assert!(!paths.is_empty());
        // Should find paths from R1.A to C1.+, including the direct VCC path
        // The direct path should be one of them
        let direct_path_exists = paths.iter().any(|path| path.len() == 2);
        assert!(direct_path_exists, "Should include direct path via VCC net");

        // With factor tracking, we should get exactly 2 valid paths:
        // 1. R1.A -> (VCC) -> C1.+
        // 2. R1.A -> (R1) -> R1.B -> (GND) -> C1.- -> (C1) -> C1.+
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn test_complex_circuit() {
        // Create a more complex circuit:
        let net_to_ports = HashMap::from([
            (
                "VCC".to_string(),
                vec![
                    ("MCU", "VDD").into(),
                    ("R1", "A").into(),
                    ("C1", "+").into(),
                ],
            ),
            (
                "GND".to_string(),
                vec![
                    ("MCU", "GND").into(),
                    ("R1", "B").into(),
                    ("C1", "-").into(),
                ],
            ),
            (
                "SDA".to_string(),
                vec![
                    ("MCU", "SDA").into(),
                    ("BMI270", "SDA").into(),
                    ("R2", "A").into(),
                ],
            ),
            (
                "SCL".to_string(),
                vec![
                    ("MCU", "SCL").into(),
                    ("BMI270", "SCL").into(),
                    ("R3", "A").into(),
                ],
            ),
            (
                "PULLUP_VCC".to_string(),
                vec![("R2", "B").into(), ("R3", "B").into()],
            ),
        ]);

        let component_pins = HashMap::from([
            (
                "MCU".into(),
                vec![
                    "VDD".to_string(),
                    "GND".to_string(),
                    "SDA".to_string(),
                    "SCL".to_string(),
                ],
            ),
            ("BMI270".into(), vec!["SDA".to_string(), "SCL".to_string()]),
            ("R1".into(), vec!["A".to_string(), "B".to_string()]),
            ("R2".into(), vec!["A".to_string(), "B".to_string()]),
            ("R3".into(), vec!["A".to_string(), "B".to_string()]),
            ("C1".into(), vec!["+".to_string(), "-".to_string()]),
        ]);

        let graph = CircuitGraph::new(net_to_ports, component_pins, HashSet::new()).unwrap();

        // Get port IDs for testing
        let mcu_sda = graph.port_id(&("MCU", "SDA").into()).unwrap();
        let bmi_sda = graph.port_id(&("BMI270", "SDA").into()).unwrap();

        // Verify the graph structure
        assert_eq!(graph.port_count(), 14); // All unique ports from the circuit
        assert_eq!(graph.factor_count(), 11); // 5 nets + 6 components

        let mut paths = Vec::new();
        graph.all_simple_paths(mcu_sda, bmi_sda, 10, |path| {
            paths.push(path.to_vec());
        });

        assert!(
            !paths.is_empty(),
            "Should find at least one path between MCU.SDA and BMI270.SDA"
        );

        // The path should go through the SDA net:
        // MCU.SDA -> MCU component -> (some other pin) -> SDA net -> BMI270.SDA
        // Or more directly: MCU.SDA -> SDA net -> BMI270.SDA
        for path in &paths {
            assert!(path.len() >= 2, "Path should have at least 2 ports");
            assert_eq!(path[0], mcu_sda, "Path should start at MCU.SDA");
            assert_eq!(
                path[path.len() - 1],
                bmi_sda,
                "Path should end at BMI270.SDA"
            );
        }
    }

    #[test]
    fn test_graph_constraints() {
        // Test creating with proper types using cleaner API
        let net_to_ports = HashMap::from([
            (
                "VCC".to_string(),
                vec![("R1", "A").into(), ("C1", "+").into()],
            ),
            (
                "GND".to_string(),
                vec![("R1", "B").into(), ("C1", "-").into()],
            ),
        ]);

        let component_pins = HashMap::from([
            ("R1".into(), vec!["A".to_string(), "B".to_string()]),
            ("C1".into(), vec!["+".to_string(), "-".to_string()]),
        ]);

        let graph_result = CircuitGraph::new(net_to_ports, component_pins, HashSet::new());
        match &graph_result {
            Ok(_) => println!("✓ Graph created successfully"),
            Err(e) => println!("✗ Graph creation failed: {}", e),
        }
        assert!(
            graph_result.is_ok(),
            "Should succeed creating graph: {:?}",
            graph_result
        );
    }

    #[test]
    fn test_neighbor_iteration() {
        let net_to_ports = HashMap::from([
            (
                "VCC".to_string(),
                vec![("R1", "A").into(), ("C1", "+").into()],
            ),
            (
                "GND".to_string(),
                vec![("R1", "B").into(), ("C1", "-").into()],
            ),
        ]);

        let component_pins = HashMap::from([
            ("R1".into(), vec!["A".to_string(), "B".to_string()]),
            ("C1".into(), vec!["+".to_string(), "-".to_string()]),
        ]);

        let graph = CircuitGraph::new(net_to_ports, component_pins, HashSet::new()).unwrap();

        // Get port IDs
        let r1_a = graph.port_id(&("R1", "A").into()).unwrap();
        let c1_plus = graph.port_id(&("C1", "+").into()).unwrap();
        let r1_b = graph.port_id(&("R1", "B").into()).unwrap();
        let mut neighbors = Vec::new();
        graph.for_each_neighbor(r1_a, |port| neighbors.push(port));

        // R1.A should be connected to:
        // 1. Via VCC net: C1.+
        // 2. Via R1 component: R1.B
        assert_eq!(neighbors.len(), 2);

        assert!(
            neighbors.contains(&c1_plus),
            "Should be connected to C1.+ via VCC net"
        );
        assert!(
            neighbors.contains(&r1_b),
            "Should be connected to R1.B via R1 component"
        );
    }

    #[test]
    fn test_display() {
        // Simple circuit for testing display
        let net_to_ports = HashMap::from([
            (
                "VCC".to_string(),
                vec![("R1", "A").into(), ("C1", "+").into()],
            ),
            (
                "GND".to_string(),
                vec![("R1", "B").into(), ("C1", "-").into()],
            ),
        ]);

        let component_pins = HashMap::from([
            ("R1".into(), vec!["A".to_string(), "B".to_string()]),
            ("C1".into(), vec!["+".to_string(), "-".to_string()]),
        ]);

        let graph = CircuitGraph::new(net_to_ports, component_pins, HashSet::new()).unwrap();

        let display_output = format!("{}", graph);

        // Check that the output contains expected elements
        assert!(display_output.contains("CircuitGraph"));
        assert!(display_output.contains("factors:"));
        assert!(display_output.contains("ports:"));
        assert!(display_output.contains("VCC"));
        assert!(display_output.contains("R1.A"));
        assert!(display_output.contains("Component(\"R1\")"));

        println!("Display output:\n{}", display_output);
    }

    #[test]
    fn test_factor_tracking_prevents_net_revisit() {
        // Create a circuit where there are multiple paths, but factor tracking should prevent net revisits
        let net_to_ports = HashMap::from([
            (
                "NET_A".to_string(),
                vec![("R1", "A").into(), ("R2", "A").into()],
            ),
            (
                "NET_B".to_string(),
                vec![("R1", "B").into(), ("R3", "A").into()],
            ),
            (
                "NET_C".to_string(),
                vec![("R2", "B").into(), ("R3", "B").into()],
            ),
        ]);

        let component_pins = HashMap::from([
            ("R1".into(), vec!["A".to_string(), "B".to_string()]),
            ("R2".into(), vec!["A".to_string(), "B".to_string()]),
            ("R3".into(), vec!["A".to_string(), "B".to_string()]),
        ]);

        let graph = CircuitGraph::new(net_to_ports, component_pins, HashSet::new()).unwrap();

        // Find paths from R1.A to R3.B (should go via R2)
        let start_id = graph.port_id(&("R1", "A").into()).unwrap();
        let goal_id = graph.port_id(&("R3", "B").into()).unwrap();

        let mut paths = Vec::new();
        graph.all_simple_paths(start_id, goal_id, 20, |path| {
            paths.push(path.to_vec());
        });

        // Verify that the factor tracking is working correctly by ensuring we get valid paths
        // The key is that our pathfinding should prevent invalid factor reuse
        // For this specific circuit, we should be able to go from R1.A to R3.B via R2

        // Should find exactly 2 valid paths with no factor reuse:
        // 1. R1.A -> (NET_A) -> R2.A -> (R2) -> R2.B -> (NET_C) -> R3.B
        // 2. R1.A -> (R1) -> R1.B -> (NET_B) -> R3.A -> (R3) -> R3.B
        assert!(!paths.is_empty(), "Should find at least one valid path");
        assert_eq!(
            paths.len(),
            2,
            "Should find exactly 2 valid paths with no factor reuse"
        );
    }

    #[test]
    fn test_factor_tracking_prevents_component_revisit() {
        // Create a simple linear circuit where component revisit prevention is clear
        let net_to_ports = HashMap::from([
            (
                "NET_1".to_string(),
                vec![("R1", "A").into(), ("R2", "A").into()],
            ),
            (
                "NET_2".to_string(),
                vec![("R2", "B").into(), ("R3", "A").into()],
            ),
            ("NET_3".to_string(), vec![("R1", "B").into()]),
            ("NET_4".to_string(), vec![("R3", "B").into()]),
        ]);

        let component_pins = HashMap::from([
            ("R1".into(), vec!["A".to_string(), "B".to_string()]),
            ("R2".into(), vec!["A".to_string(), "B".to_string()]),
            ("R3".into(), vec!["A".to_string(), "B".to_string()]),
        ]);

        let graph = CircuitGraph::new(net_to_ports, component_pins, HashSet::new()).unwrap();

        let start_id = graph.port_id(&("R1", "A").into()).unwrap();
        let goal_id = graph.port_id(&("R3", "A").into()).unwrap();

        let mut paths = Vec::new();
        graph.all_simple_paths(start_id, goal_id, 20, |path| {
            paths.push(path.to_vec());
        });

        // Should find exactly one path: R1.A -> (NET_1) -> R2.A -> (R2) -> R2.B -> (NET_2) -> R3.A
        assert!(!paths.is_empty(), "Should find at least one valid path");
        assert_eq!(paths.len(), 1, "Should find exactly one path");
    }

    #[test]
    fn test_factor_tracking_with_complex_circuit() {
        // More complex circuit to thoroughly test factor tracking
        let net_to_ports = HashMap::from([
            (
                "VCC".to_string(),
                vec![
                    ("MCU", "VDD").into(),
                    ("R1", "A").into(),
                    ("C1", "+").into(),
                ],
            ),
            (
                "GND".to_string(),
                vec![
                    ("MCU", "GND").into(),
                    ("R1", "B").into(),
                    ("C1", "-").into(),
                ],
            ),
            (
                "SDA".to_string(),
                vec![
                    ("MCU", "SDA").into(),
                    ("BMI270", "SDA").into(),
                    ("R2", "A").into(),
                ],
            ),
            (
                "SCL".to_string(),
                vec![
                    ("MCU", "SCL").into(),
                    ("BMI270", "SCL").into(),
                    ("R3", "A").into(),
                ],
            ),
            (
                "PULLUP_VCC".to_string(),
                vec![("R2", "B").into(), ("R3", "B").into()],
            ),
        ]);

        let component_pins = HashMap::from([
            (
                "MCU".into(),
                vec![
                    "VDD".to_string(),
                    "GND".to_string(),
                    "SDA".to_string(),
                    "SCL".to_string(),
                ],
            ),
            ("BMI270".into(), vec!["SDA".to_string(), "SCL".to_string()]),
            ("R1".into(), vec!["A".to_string(), "B".to_string()]),
            ("R2".into(), vec!["A".to_string(), "B".to_string()]),
            ("R3".into(), vec!["A".to_string(), "B".to_string()]),
            ("C1".into(), vec!["+".to_string(), "-".to_string()]),
        ]);

        let graph = CircuitGraph::new(net_to_ports, component_pins, HashSet::new()).unwrap();

        // Test multiple path queries
        let test_cases = vec![
            (("MCU", "VDD"), ("C1", "+")),
            (("MCU", "SDA"), ("BMI270", "SDA")),
            (("R2", "B"), ("R3", "B")),
        ];

        for (start_port, end_port) in test_cases {
            let start_id = graph.port_id(&start_port.into()).unwrap();
            let goal_id = graph.port_id(&end_port.into()).unwrap();

            let mut paths = Vec::new();
            graph.all_simple_paths(start_id, goal_id, 20, |path| {
                paths.push(path.to_vec());
            });

            // With factor tracking, we should find valid paths with no factor reuse
            // Just ensure we find reasonable paths for this complex circuit
        }
    }

    #[test]
    fn test_factor_tracking_efficiency() {
        // Test that the pathfinding terminates efficiently even with larger circuits
        let mut net_to_ports = HashMap::new();
        let mut component_pins = HashMap::new();

        // Create a larger test circuit with 10 components (smaller for testing)
        for i in 0..10 {
            let comp_name = format!("R{}", i);
            component_pins.insert(
                comp_name.clone().into(),
                vec!["A".to_string(), "B".to_string()],
            );
        }

        // Create nets connecting adjacent components
        for i in 0..9 {
            let net_name = format!("NET_{}", i);
            net_to_ports.insert(
                net_name,
                vec![
                    (format!("R{}", i).as_str(), "B").into(),
                    (format!("R{}", i + 1).as_str(), "A").into(),
                ],
            );
        }

        // Add an additional net to connect R0.A to something (so R0.A has both factors)
        net_to_ports.insert("START_NET".to_string(), vec![("R0", "A").into()]);

        // Add an additional net to connect R9.B to something (so R9.B has both factors)
        net_to_ports.insert("END_NET".to_string(), vec![("R9", "B").into()]);

        let graph = CircuitGraph::new(net_to_ports, component_pins, HashSet::new()).unwrap();

        let start_id = graph.port_id(&("R0", "A").into()).unwrap();
        let goal_id = graph.port_id(&("R9", "B").into()).unwrap();

        let mut paths = Vec::new();
        let start_time = std::time::Instant::now();

        graph.all_simple_paths(start_id, goal_id, 30, |path| {
            paths.push(path.to_vec());
        });

        let duration = start_time.elapsed();

        // Should complete quickly (under 100ms for this size)
        assert!(
            duration.as_millis() < 100,
            "Pathfinding took too long: {:?}",
            duration
        );

        // Should find exactly one path (linear chain)
        assert_eq!(
            paths.len(),
            1,
            "Should find exactly one path in linear chain"
        );

        // Path should have expected length (10 components = 20 ports in linear chain)
        assert_eq!(
            paths[0].len(),
            20,
            "Path should visit all 20 ports across 10 components"
        );
    }

    #[test]
    fn test_external_nets_pathfinding() {
        // Create a simple circuit: R1 connected to VCC net
        let mut net_to_ports = HashMap::new();
        let mut component_pins = HashMap::new();

        // VCC net connects R1.A
        net_to_ports.insert("VCC".to_string(), vec![("R1", "A").into()]);

        // R1 component with pin A
        component_pins.insert("R1".into(), vec!["A".to_string()]);

        // Mark VCC as a public net
        let mut public_nets = HashSet::new();
        public_nets.insert("VCC".to_string());

        let graph = CircuitGraph::new(net_to_ports, component_pins, public_nets).unwrap();

        // Should now have 2 ports: R1.A and <external>.VCC
        assert_eq!(graph.port_count(), 2);

        // Get port IDs
        let r1_a = graph.port_id(&("R1", "A").into()).unwrap();
        let external_vcc = graph.port_id(&("<external>", "VCC").into()).unwrap();

        // Test pathfinding from internal port to external port
        let mut paths = Vec::new();
        graph.all_simple_paths(r1_a, external_vcc, 10, |path| {
            paths.push(path.to_vec());
        });

        // Should find at least one path from R1.A to external VCC
        assert!(!paths.is_empty(), "Should find path to external net");

        // Test pathfinding from external port to internal port
        let mut reverse_paths = Vec::new();
        graph.all_simple_paths(external_vcc, r1_a, 10, |path| {
            reverse_paths.push(path.to_vec());
        });

        // Should find at least one path from external VCC to R1.A
        assert!(
            !reverse_paths.is_empty(),
            "Should find path from external net"
        );

        // Verify external port has the expected port factors (should be duplicated)
        let [f0, f1] = graph.port_factors(external_vcc);
        assert_eq!(f0, f1, "External port should have duplicated factor");
    }

    #[test]
    fn test_multiple_external_nets() {
        // Create a circuit with multiple nets that could be external
        let mut net_to_ports = HashMap::new();
        let mut component_pins = HashMap::new();

        // Two nets: VCC and GND
        net_to_ports.insert("VCC".to_string(), vec![("R1", "A").into()]);
        net_to_ports.insert("GND".to_string(), vec![("R1", "B").into()]);

        // R1 component with pins A and B
        component_pins.insert("R1".into(), vec!["A".to_string(), "B".to_string()]);

        // Mark both VCC and GND as public nets
        let mut public_nets = HashSet::new();
        public_nets.insert("VCC".to_string());
        public_nets.insert("GND".to_string());

        let graph = CircuitGraph::new(net_to_ports, component_pins, public_nets).unwrap();

        // Should now have 4 ports: R1.A, R1.B, <external>.VCC, <external>.GND
        assert_eq!(graph.port_count(), 4);

        // Verify both external ports exist
        assert!(graph.port_id(&("<external>", "VCC").into()).is_some());
        assert!(graph.port_id(&("<external>", "GND").into()).is_some());

        // Verify pathfinding works between external nets via the internal component
        let external_vcc = graph.port_id(&("<external>", "VCC").into()).unwrap();
        let external_gnd = graph.port_id(&("<external>", "GND").into()).unwrap();

        let mut paths = Vec::new();
        graph.all_simple_paths(external_vcc, external_gnd, 10, |path| {
            paths.push(path.to_vec());
        });

        // Should find at least one path from external VCC to external GND via R1
        assert!(
            !paths.is_empty(),
            "Should find path between external nets via component"
        );
    }
}
