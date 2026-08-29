//! VPSC - Variable Placement with Separation Constraints
//!
//! A solver for the problem of positioning variables subject to separation constraints.
//! Ported from the libavoid C++ implementation.
//!
//! Reference: Tim Dwyer, Kim Marriott, and Peter J. Stuckey.
//! Fast Node Overlap Removal. In Proceedings of the 13th International
//! Symposium on Graph Drawing (GD'05), Lecture Notes in Computer Science
//! 3843, pp. 153-164. Springer-Verlag, 2005.

// ============================================================================
// Constants
// ============================================================================

const ZERO_WEIGHT_THRESHOLD: f64 = 1e-10;

// ============================================================================
// Variable
// ============================================================================

/// A variable in the VPSC problem
#[derive(Debug)]
pub struct Variable {
    /// Unique identifier
    pub id: usize,
    /// Desired position
    pub desired_position: f64,
    /// Final solved position
    pub final_position: f64,
    /// Weight (how much variable wants to be at desired position)
    pub weight: f64,
    /// Scale factor
    pub scale: f64,
    /// Offset within block
    pub offset: f64,
    /// Block this variable belongs to
    pub block_id: Option<usize>,
    /// Whether variable has been visited during traversal
    pub visited: bool,
    /// Whether desired position is fixed
    pub fixed_desired_position: bool,
    /// Incoming constraints (this variable is on the right)
    pub constraints_in: Vec<usize>,
    /// Outgoing constraints (this variable is on the left)
    pub constraints_out: Vec<usize>,
}

impl Variable {
    pub fn new(id: usize, desired_pos: f64, weight: f64) -> Self {
        Variable {
            id,
            desired_position: desired_pos,
            final_position: desired_pos,
            weight,
            scale: 1.0,
            offset: 0.0,
            block_id: None,
            visited: false,
            fixed_desired_position: false,
            constraints_in: Vec::new(),
            constraints_out: Vec::new(),
        }
    }

    pub fn with_scale(id: usize, desired_pos: f64, weight: f64, scale: f64) -> Self {
        Variable {
            id,
            desired_position: desired_pos,
            final_position: desired_pos,
            weight,
            scale,
            offset: 0.0,
            block_id: None,
            visited: false,
            fixed_desired_position: false,
            constraints_in: Vec::new(),
            constraints_out: Vec::new(),
        }
    }

    /// Compute the derivative of the cost function with respect to this variable
    pub fn dfdv(&self) -> f64 {
        2.0 * self.weight * (self.final_position - self.desired_position)
    }
}

// ============================================================================
// Constraint
// ============================================================================

/// A separation constraint: left.position + gap <= right.position
#[derive(Debug)]
pub struct Constraint {
    /// Unique identifier
    pub id: usize,
    /// Left variable index
    pub left: usize,
    /// Right variable index
    pub right: usize,
    /// Minimum gap between left and right
    pub gap: f64,
    /// Lagrange multiplier
    pub lm: f64,
    /// Whether constraint is active (tight)
    pub active: bool,
    /// Whether this is an equality constraint
    pub equality: bool,
    /// Whether constraint is unsatisfiable
    pub unsatisfiable: bool,
    /// Timestamp for block operations
    pub time_stamp: u64,
}

impl Constraint {
    pub fn new(id: usize, left: usize, right: usize, gap: f64) -> Self {
        Constraint {
            id,
            left,
            right,
            gap,
            lm: 0.0,
            active: false,
            equality: false,
            unsatisfiable: false,
            time_stamp: 0,
        }
    }

    pub fn equality(id: usize, left: usize, right: usize, gap: f64) -> Self {
        Constraint {
            id,
            left,
            right,
            gap,
            lm: 0.0,
            active: false,
            equality: true,
            unsatisfiable: false,
            time_stamp: 0,
        }
    }

    /// Compute slack: how much the constraint is satisfied by
    /// Negative slack means constraint is violated
    pub fn slack(&self, variables: &[Variable]) -> f64 {
        if self.unsatisfiable {
            return f64::MAX;
        }
        let left = &variables[self.left];
        let right = &variables[self.right];
        right.final_position * right.scale - self.gap - left.final_position * left.scale
    }
}

// ============================================================================
// Block
// ============================================================================

/// A block of variables that move together
#[derive(Debug)]
pub struct Block {
    /// Block identifier
    pub id: usize,
    /// Variables in this block
    pub variables: Vec<usize>,
    /// Block position
    pub position: f64,
    /// Position statistics for weighted positioning
    pub ps: PositionStats,
    /// Whether block has been deleted
    pub deleted: bool,
    /// Timestamp for operations
    pub time_stamp: u64,
}

/// Statistics for computing weighted block position
#[derive(Debug, Clone, Default)]
pub struct PositionStats {
    pub scale: f64,
    pub ab: f64, // sum of weight * desired_position
    pub ad: f64, // sum of weight * offset
    pub a2: f64, // sum of weight
}

impl PositionStats {
    pub fn new() -> Self {
        PositionStats {
            scale: 1.0,
            ab: 0.0,
            ad: 0.0,
            a2: 0.0,
        }
    }

    pub fn add_variable(&mut self, var: &Variable) {
        let a = var.scale / self.scale;
        self.ab += a * var.weight * var.desired_position;
        self.ad += a * var.weight * var.offset;
        self.a2 += a * a * var.weight;
    }

    pub fn optimal_position(&self) -> f64 {
        if self.a2.abs() < ZERO_WEIGHT_THRESHOLD {
            0.0
        } else {
            (self.ab - self.ad) / self.a2
        }
    }
}

impl Block {
    pub fn new(id: usize) -> Self {
        Block {
            id,
            variables: Vec::new(),
            position: 0.0,
            ps: PositionStats::new(),
            deleted: false,
            time_stamp: 0,
        }
    }

    pub fn with_variable(id: usize, var_idx: usize, variables: &mut [Variable]) -> Self {
        let mut block = Block::new(id);
        // Set initial block position to variable's desired position
        block.position = variables[var_idx].desired_position;
        block.add_variable(var_idx, variables);
        block.update_weighted_position(variables);
        block
    }

    pub fn add_variable(&mut self, var_idx: usize, variables: &mut [Variable]) {
        let var = &mut variables[var_idx];
        var.block_id = Some(self.id);
        var.offset = 0.0; // Offset relative to block position
        self.ps.add_variable(var);
        self.variables.push(var_idx);
    }

    /// Update the weighted position of the block
    pub fn update_weighted_position(&mut self, variables: &[Variable]) {
        self.ps = PositionStats::new();
        for &var_idx in &self.variables {
            self.ps.add_variable(&variables[var_idx]);
        }
        self.position = self.ps.optimal_position();
    }

    /// Compute cost of this block (sum of squared deviations from desired)
    pub fn cost(&self, variables: &[Variable]) -> f64 {
        let mut c = 0.0;
        for &var_idx in &self.variables {
            let var = &variables[var_idx];
            let diff = var.final_position - var.desired_position;
            c += var.weight * diff * diff;
        }
        c
    }
}

// ============================================================================
// Incremental Solver
// ============================================================================

/// Incremental VPSC solver
///
/// Solves the problem of placing variables to minimize weighted deviation
/// from desired positions while satisfying separation constraints.
pub struct IncSolver {
    /// Variables
    pub variables: Vec<Variable>,
    /// Constraints
    pub constraints: Vec<Constraint>,
    /// Blocks
    pub blocks: Vec<Block>,
}

impl IncSolver {
    pub fn new() -> Self {
        IncSolver {
            variables: Vec::new(),
            constraints: Vec::new(),
            blocks: Vec::new(),
        }
    }

    /// Create solver with given variables and constraints
    pub fn with_problem(mut variables: Vec<Variable>, constraints: Vec<Constraint>) -> Self {
        // Link constraints to variables
        for (cid, constraint) in constraints.iter().enumerate() {
            variables[constraint.left].constraints_out.push(cid);
            variables[constraint.right].constraints_in.push(cid);
        }

        let mut solver = IncSolver {
            variables,
            constraints,
            blocks: Vec::new(),
        };
        solver.init_blocks();
        solver
    }

    /// Add a variable and return its index
    pub fn add_variable(&mut self, desired_pos: f64, weight: f64) -> usize {
        let id = self.variables.len();
        self.variables.push(Variable::new(id, desired_pos, weight));
        id
    }

    /// Add a constraint and return its index
    pub fn add_constraint(&mut self, left: usize, right: usize, gap: f64) -> usize {
        self.add_constraint_internal(left, right, gap, false)
    }

    /// Add an equality constraint (forces exact separation)
    /// C++ ref: libavoid/orthogonal.cpp:2779-2786 - equality=true for shouldAlignWith
    pub fn add_equality_constraint(&mut self, left: usize, right: usize, gap: f64) -> usize {
        self.add_constraint_internal(left, right, gap, true)
    }

    fn add_constraint_internal(
        &mut self,
        left: usize,
        right: usize,
        gap: f64,
        equality: bool,
    ) -> usize {
        let id = self.constraints.len();
        let mut constraint = Constraint::new(id, left, right, gap);
        constraint.equality = equality;

        // Add to variable's constraint lists
        self.variables[left].constraints_out.push(id);
        self.variables[right].constraints_in.push(id);

        self.constraints.push(constraint);
        id
    }

    /// Initialize blocks - one block per variable initially
    fn init_blocks(&mut self) {
        self.blocks.clear();
        for i in 0..self.variables.len() {
            let block = Block::with_variable(i, i, &mut self.variables);
            self.blocks.push(block);
        }
    }

    /// Solve the VPSC problem
    pub fn solve(&mut self) {
        // Initialize blocks if needed
        if self.blocks.is_empty() {
            self.init_blocks();
        }

        // Set initial positions
        for block in &self.blocks {
            for &var_idx in &block.variables {
                self.variables[var_idx].final_position =
                    block.position + self.variables[var_idx].offset;
            }
        }

        #[cfg(test)]
        {
            eprintln!(
                "VPSC solve: {} variables, {} constraints",
                self.variables.len(),
                self.constraints.len()
            );
            for (i, c) in self.constraints.iter().enumerate() {
                eprintln!(
                    "  constraint {}: var[{}] + {} <= var[{}]",
                    i, c.left, c.gap, c.right
                );
            }
        }

        // Simple greedy constraint satisfaction
        // Process constraints in order until no more violations
        // C++ ref: libavoid/vpsc.cpp:284 - equality constraints are always processed
        let max_iterations = self.constraints.len() * 2 + 1;
        #[allow(unused_variables)]
        for iter in 0..max_iterations {
            let mut satisfied_any = false;

            for cid in 0..self.constraints.len() {
                if self.constraints[cid].active || self.constraints[cid].unsatisfiable {
                    continue;
                }

                let slack = self.constraints[cid].slack(&self.variables);
                let is_equality = self.constraints[cid].equality;

                // Process if: equality constraint OR violated (slack < 0)
                // Equality constraints must always be activated to force alignment
                #[allow(clippy::collapsible_if)]
                if is_equality || slack < -1e-10 {
                    #[cfg(test)]
                    eprintln!(
                        "  iter {}: processing constraint {} (var[{}] + {} <= var[{}]), slack={}, equality={}",
                        iter,
                        cid,
                        self.constraints[cid].left,
                        self.constraints[cid].gap,
                        self.constraints[cid].right,
                        slack,
                        is_equality
                    );
                    // Constraint needs to be satisfied - merge blocks
                    if self.satisfy_constraint(cid) {
                        satisfied_any = true;
                        #[cfg(test)]
                        {
                            eprintln!("    after merge:");
                            for (i, var) in self.variables.iter().enumerate() {
                                eprintln!(
                                    "      var[{}]: pos={:.2}, offset={:.2}, block={:?}",
                                    i, var.final_position, var.offset, var.block_id
                                );
                            }
                        }
                    }
                }
            }

            if !satisfied_any {
                break;
            }
        }

        // Update final positions
        self.update_final_positions();

        #[cfg(test)]
        {
            eprintln!("VPSC final:");
            for (i, var) in self.variables.iter().enumerate() {
                eprintln!("  var[{}]: final_pos={:.2}", i, var.final_position);
            }
            // Check all constraints
            for c in &self.constraints {
                let left_pos = self.variables[c.left].final_position;
                let right_pos = self.variables[c.right].final_position;
                let slack = right_pos - left_pos - c.gap;
                if slack < -1e-6 {
                    eprintln!(
                        "  VIOLATED: var[{}] + {} <= var[{}] (slack={:.4})",
                        c.left, c.gap, c.right, slack
                    );
                }
            }
        }
    }

    /// Satisfy a violated constraint by merging blocks or adjusting offsets
    /// Returns true if any change was made
    fn satisfy_constraint(&mut self, constraint_id: usize) -> bool {
        let constraint = &self.constraints[constraint_id];
        let left_var = constraint.left;
        let right_var = constraint.right;
        let gap = constraint.gap;

        let left_block_id = self.variables[left_var].block_id;
        let right_block_id = self.variables[right_var].block_id;

        match (left_block_id, right_block_id) {
            (Some(lb), Some(rb))
                if lb != rb && !self.blocks[lb].deleted && !self.blocks[rb].deleted =>
            {
                // Different blocks - merge them
                self.merge_blocks(lb, rb, constraint_id);
                true
            }
            (Some(lb), Some(rb)) if lb == rb && !self.blocks[lb].deleted => {
                // Same block - check if constraint is satisfied by offsets
                let left_offset = self.variables[left_var].offset;
                let right_offset = self.variables[right_var].offset;

                // Constraint: left_pos + gap <= right_pos
                // In terms of offsets: (block_pos + left_offset) + gap <= (block_pos + right_offset)
                // Simplifies to: left_offset + gap <= right_offset

                if left_offset + gap > right_offset + 1e-10 {
                    // Constraint violated within block - adjust offsets
                    // Need to increase right_offset to satisfy: right_offset >= left_offset + gap
                    let required_right_offset = left_offset + gap;

                    // Shift the right variable and all variables connected to it through
                    // constraints that are "downstream" of right_var.
                    // For simplicity, just shift right_var and recalculate.
                    self.variables[right_var].offset = required_right_offset;

                    // Recalculate block optimal position with new offsets
                    self.blocks[lb].update_weighted_position(&self.variables);

                    // Update final positions for all variables in block
                    for &var_idx in &self.blocks[lb].variables.clone() {
                        self.variables[var_idx].final_position =
                            self.blocks[lb].position + self.variables[var_idx].offset;
                    }

                    self.constraints[constraint_id].active = true;
                    true
                } else {
                    // Constraint already satisfied
                    false
                }
            }
            _ => {
                // Invalid state
                false
            }
        }
    }

    /// Merge two blocks due to a constraint
    fn merge_blocks(&mut self, left_block_id: usize, right_block_id: usize, constraint_id: usize) {
        let constraint = &self.constraints[constraint_id];
        let left_var_idx = constraint.left;
        let right_var_idx = constraint.right;
        let gap = constraint.gap;

        // Get current positions of the constraint variables
        let left_var_pos =
            self.blocks[left_block_id].position + self.variables[left_var_idx].offset;
        let right_var_pos =
            self.blocks[right_block_id].position + self.variables[right_var_idx].offset;

        // The constraint requires: right_var_pos >= left_var_pos + gap
        // We need to shift right block so this holds
        let required_right_var_pos = left_var_pos + gap;
        let right_block_shift = required_right_var_pos - right_var_pos;

        // Note: right_block_shift moves the entire right block to satisfy the constraint

        // Move all variables from right block to left block
        let right_vars: Vec<usize> = self.blocks[right_block_id].variables.clone();

        for &var_idx in &right_vars {
            // Adjust offset: var was at (right_block.position + var.offset)
            // Now should be at same absolute position: (left_block.position + new_offset)
            // new_offset = (right_block.position + shift + var.offset) - left_block.position
            let old_abs_pos = self.blocks[right_block_id].position
                + self.variables[var_idx].offset
                + right_block_shift;
            self.variables[var_idx].offset = old_abs_pos - self.blocks[left_block_id].position;
            self.variables[var_idx].block_id = Some(left_block_id);
            self.blocks[left_block_id].variables.push(var_idx);
        }

        // Recalculate block statistics and optimal position
        self.blocks[left_block_id].update_weighted_position(&self.variables);

        // Update positions for all variables in the merged block
        for &var_idx in &self.blocks[left_block_id].variables {
            self.variables[var_idx].final_position =
                self.blocks[left_block_id].position + self.variables[var_idx].offset;
        }

        // Mark right block as deleted
        self.blocks[right_block_id].deleted = true;
        self.blocks[right_block_id].variables.clear();

        // Mark constraint as active
        self.constraints[constraint_id].active = true;
    }

    /// Update final positions of all variables
    fn update_final_positions(&mut self) {
        for var in &mut self.variables {
            if let Some(block_id) = var.block_id {
                let block = &self.blocks[block_id];
                if !block.deleted {
                    var.final_position = (block.position + var.offset) / var.scale;
                }
            }
        }
    }

    /// Get the final position of a variable
    pub fn get_position(&self, var_idx: usize) -> f64 {
        self.variables[var_idx].final_position
    }

    /// Compute total cost (sum of weighted squared deviations)
    pub fn cost(&self) -> f64 {
        let mut total = 0.0;
        for var in &self.variables {
            let diff = var.final_position - var.desired_position;
            total += var.weight * diff * diff;
        }
        total
    }

    /// Compute Lagrange multipliers for all active constraints in a block.
    /// C++ ref: libavoid/vpsc.cpp:350-380 - Block::compute_lm()
    ///
    /// Returns the constraint with the most negative LM (best candidate for splitting).
    fn compute_lagrange_multipliers(&mut self, block_id: usize) -> Option<usize> {
        let block = &self.blocks[block_id];
        if block.deleted {
            return None;
        }

        // Find all active constraints within this block
        let mut min_lm = 0.0;
        let mut min_constraint = None;

        for cid in 0..self.constraints.len() {
            let constraint = &self.constraints[cid];
            if !constraint.active {
                continue;
            }

            // Check if both endpoints are in this block
            let left_block = self.variables[constraint.left].block_id;
            let right_block = self.variables[constraint.right].block_id;

            if left_block != Some(block_id) || right_block != Some(block_id) {
                continue;
            }

            // Compute Lagrange multiplier
            // LM = derivative of cost w.r.t. relaxing this constraint
            // For constraint l + gap <= r: LM = 2 * (sum of weighted derivatives on right side)
            let lm = self.compute_constraint_lm(cid);
            self.constraints[cid].lm = lm;

            // Track most negative LM
            if lm < min_lm {
                min_lm = lm;
                min_constraint = Some(cid);
            }
        }

        min_constraint
    }

    /// Compute Lagrange multiplier for a single constraint.
    /// C++ ref: libavoid/vpsc.cpp - constraint LM calculation
    fn compute_constraint_lm(&self, constraint_id: usize) -> f64 {
        let constraint = &self.constraints[constraint_id];
        let right_var = &self.variables[constraint.right];

        // Simple approximation: LM based on derivative at right variable
        // Positive LM means constraint is "pulling" - wants to stay tight
        // Negative LM means constraint is "pushing" - should be relaxed
        right_var.dfdv()
    }

    /// Split a block at a constraint with negative Lagrange multiplier.
    /// C++ ref: libavoid/vpsc.cpp:420-500 - Block::split()
    ///
    /// Returns true if split was performed.
    fn split_block(&mut self, block_id: usize, constraint_id: usize) -> bool {
        let constraint = &self.constraints[constraint_id];
        let left_var = constraint.left;
        let right_var = constraint.right;

        // Verify constraint is in this block
        if self.variables[left_var].block_id != Some(block_id)
            || self.variables[right_var].block_id != Some(block_id)
        {
            return false;
        }

        // Create a new block for variables on the right side of the split
        let new_block_id = self.blocks.len();
        let _old_block_position = self.blocks[block_id].position; // Preserved for debugging

        // Find variables reachable from right_var without crossing this constraint
        let mut right_side: Vec<usize> = Vec::new();
        let mut visited = vec![false; self.variables.len()];

        self.collect_right_side(right_var, constraint_id, &mut visited, &mut right_side);

        if right_side.is_empty() {
            return false;
        }

        // Create new block
        let mut new_block = Block {
            id: new_block_id,
            variables: Vec::new(),
            position: 0.0,
            ps: PositionStats::new(),
            deleted: false,
            time_stamp: 0,
        };

        // Move right-side variables to new block
        for &var_idx in &right_side {
            // Note: absolute position would be old_block_position + offset
            // Currently not needed as we recalculate positions after split
            self.variables[var_idx].block_id = Some(new_block_id);
            new_block.variables.push(var_idx);
        }

        // Remove right-side variables from old block
        self.blocks[block_id]
            .variables
            .retain(|&v| !right_side.contains(&v));

        // Add new block
        self.blocks.push(new_block);

        // Update block statistics and positions
        self.blocks[block_id].update_weighted_position(&self.variables);
        self.blocks[new_block_id].update_weighted_position(&self.variables);

        // Deactivate the constraint we split on
        self.constraints[constraint_id].active = false;

        // Update final positions
        for &var_idx in &self.blocks[block_id].variables {
            self.variables[var_idx].final_position =
                self.blocks[block_id].position + self.variables[var_idx].offset;
        }
        for &var_idx in &self.blocks[new_block_id].variables {
            self.variables[var_idx].final_position =
                self.blocks[new_block_id].position + self.variables[var_idx].offset;
        }

        true
    }

    /// Collect variables reachable from start_var without crossing split_constraint
    fn collect_right_side(
        &self,
        start_var: usize,
        split_constraint: usize,
        visited: &mut [bool],
        result: &mut Vec<usize>,
    ) {
        if visited[start_var] {
            return;
        }
        visited[start_var] = true;
        result.push(start_var);

        // Follow outgoing constraints
        for &cid in &self.variables[start_var].constraints_out {
            if cid == split_constraint || !self.constraints[cid].active {
                continue;
            }
            let next = self.constraints[cid].right;
            self.collect_right_side(next, split_constraint, visited, result);
        }

        // Follow incoming constraints (go backwards)
        for &cid in &self.variables[start_var].constraints_in {
            if cid == split_constraint || !self.constraints[cid].active {
                continue;
            }
            let next = self.constraints[cid].left;
            self.collect_right_side(next, split_constraint, visited, result);
        }
    }

    /// Solve with optimization phase (split blocks with negative LMs).
    /// C++ ref: libavoid/vpsc.cpp - IncSolver::solve()
    pub fn solve_optimal(&mut self) {
        // First satisfy all constraints
        self.solve();

        // Then optimize by splitting blocks with negative LMs
        let max_split_iterations = self.blocks.len() * 2;
        for _ in 0..max_split_iterations {
            let mut did_split = false;

            for block_id in 0..self.blocks.len() {
                if self.blocks[block_id].deleted {
                    continue;
                }

                // Compute LMs and find best split candidate
                if let Some(split_constraint) = self.compute_lagrange_multipliers(block_id) {
                    if self.constraints[split_constraint].lm < -1e-10 {
                        // Split improves solution
                        if self.split_block(block_id, split_constraint) {
                            did_split = true;
                            break; // Restart iteration after split
                        }
                    }
                }
            }

            if !did_split {
                break;
            }

            // Re-run satisfy phase after splitting
            self.solve();
        }

        self.update_final_positions();
    }
}

impl Default for IncSolver {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_variable() {
        let mut solver = IncSolver::new();
        solver.add_variable(100.0, 1.0);
        solver.solve();

        assert!((solver.get_position(0) - 100.0).abs() < 0.001);
    }

    #[test]
    fn test_two_variables_no_constraint() {
        let mut solver = IncSolver::new();
        solver.add_variable(0.0, 1.0);
        solver.add_variable(100.0, 1.0);
        solver.solve();

        assert!((solver.get_position(0) - 0.0).abs() < 0.001);
        assert!((solver.get_position(1) - 100.0).abs() < 0.001);
    }

    #[test]
    fn test_two_variables_with_constraint() {
        let mut solver = IncSolver::new();
        let v0 = solver.add_variable(0.0, 1.0);
        let v1 = solver.add_variable(5.0, 1.0);
        solver.add_constraint(v0, v1, 10.0); // v1 >= v0 + 10
        solver.solve();

        // Variables should be pushed apart
        let gap = solver.get_position(1) - solver.get_position(0);
        assert!(gap >= 10.0 - 0.001, "Gap {} should be >= 10", gap);
    }

    #[test]
    fn test_chain_of_constraints() {
        let mut solver = IncSolver::new();
        let v0 = solver.add_variable(0.0, 1.0);
        let v1 = solver.add_variable(0.0, 1.0);
        let v2 = solver.add_variable(0.0, 1.0);

        solver.add_constraint(v0, v1, 10.0);
        solver.add_constraint(v1, v2, 10.0);
        solver.solve();

        let gap01 = solver.get_position(1) - solver.get_position(0);
        let gap12 = solver.get_position(2) - solver.get_position(1);

        assert!(gap01 >= 10.0 - 0.001, "Gap 0-1 {} should be >= 10", gap01);
        assert!(gap12 >= 10.0 - 0.001, "Gap 1-2 {} should be >= 10", gap12);
    }

    #[test]
    fn test_weighted_variables() {
        let mut solver = IncSolver::new();
        // Heavy variable wants to be at 0
        let v0 = solver.add_variable(0.0, 100.0);
        // Light variable wants to be at 0 too
        let v1 = solver.add_variable(0.0, 1.0);
        // But must be 10 apart
        solver.add_constraint(v0, v1, 10.0);
        solver.solve();

        // Heavy variable should barely move, light variable should move more
        let p0 = solver.get_position(0);
        let p1 = solver.get_position(1);

        assert!(p0.abs() < 1.0, "Heavy var should be near 0, got {}", p0);
        assert!(p1 >= 9.0, "Light var should be pushed right, got {}", p1);
    }
}
