//! Hierarchical layout algorithm for schematic components
//!
//! This implements a bottom-up layout approach where:
//! 1. Each module is laid out internally using a corner-tracking algorithm
//! 2. Modules are then placed within their parent modules as single units

use std::collections::{HashMap, HashSet};

/// Represents a 2D point in schematic space
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// Represents a size (width and height)
#[derive(Debug, Clone, Copy)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}

impl Size {
    pub fn new(width: f64, height: f64) -> Self {
        Self { width, height }
    }
}

/// Represents a bounding box
#[derive(Debug, Clone, Copy)]
pub struct BoundingBox {
    pub position: Point,
    pub size: Size,
}

impl BoundingBox {
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            position: Point { x, y },
            size: Size { width, height },
        }
    }

    pub fn from_position_and_size(position: Point, size: Size) -> Self {
        Self { position, size }
    }

    pub fn min_x(&self) -> f64 {
        self.position.x
    }

    pub fn min_y(&self) -> f64 {
        self.position.y
    }

    pub fn max_x(&self) -> f64 {
        self.position.x + self.size.width
    }

    pub fn max_y(&self) -> f64 {
        self.position.y + self.size.height
    }

    pub fn area(&self) -> f64 {
        self.size.width * self.size.height
    }

    pub fn top_left(&self) -> Point {
        self.position
    }

    pub fn bottom_right(&self) -> Point {
        Point {
            x: self.max_x(),
            y: self.max_y(),
        }
    }

    pub fn intersects(&self, other: &BoundingBox) -> bool {
        !(self.max_x() < other.min_x()
            || self.min_x() > other.max_x()
            || self.max_y() < other.min_y()
            || self.min_y() > other.max_y())
    }

    /// Expand this bounding box to include another
    pub fn union(&self, other: &BoundingBox) -> BoundingBox {
        let min_x = self.min_x().min(other.min_x());
        let min_y = self.min_y().min(other.min_y());
        let max_x = self.max_x().max(other.max_x());
        let max_y = self.max_y().max(other.max_y());

        BoundingBox::new(min_x, min_y, max_x - min_x, max_y - min_y)
    }
}

/// Represents a component or module that can be placed
#[derive(Debug, Clone)]
pub struct LayoutItem {
    pub id: String,
    pub bounds: BoundingBox,
    pub is_module: bool,
    pub children: Vec<String>, // IDs of child items
}

/// Represents a placement decision
#[derive(Debug, Clone)]
pub struct Placement {
    pub item_id: String,
    pub position: Point, // Position of the item's top-left corner
}

/// The main hierarchical layout engine
pub struct HierarchicalLayout {
    /// Component sizes (id -> size)
    component_sizes: HashMap<String, Size>,
    /// Module hierarchy (parent -> children)
    module_hierarchy: HashMap<String, Vec<String>>,
    /// Spacing between components
    spacing: f64,
}

impl HierarchicalLayout {
    pub fn new(spacing: f64) -> Self {
        Self {
            component_sizes: HashMap::new(),
            module_hierarchy: HashMap::new(),
            spacing,
        }
    }

    /// Set the size of a component
    pub fn set_component_size(&mut self, id: String, size: Size) {
        self.component_sizes.insert(id, size);
    }

    /// Define a module containing other components/modules
    pub fn add_module(&mut self, id: String, children: Vec<String>) {
        self.module_hierarchy.insert(id, children);
    }

    /// Check if a module has more than one child
    pub fn module_has_multiple_children(&self, module_id: &str) -> bool {
        self.module_hierarchy
            .get(module_id)
            .map(|children| children.len() > 1)
            .unwrap_or(false)
    }

    /// Perform the hierarchical layout and return bounding boxes for all items
    pub fn layout(&mut self) -> HashMap<String, BoundingBox> {
        let mut results = HashMap::new();

        // Find root items (components/modules with no parent)
        let root_items = self.find_root_items();

        if root_items.is_empty() {
            return results;
        }

        // Layout each root item recursively to get their sizes
        let mut root_bboxes = Vec::new();
        for root_id in &root_items {
            let bbox = self.layout_module_recursive(root_id, &mut results);
            root_bboxes.push((root_id.clone(), bbox));
        }

        // Sort root items by area (largest first)
        root_bboxes.sort_by(|a, b| b.1.area().partial_cmp(&a.1.area()).unwrap());

        // Use corner-tracking to pack root items
        let _packed_bbox = self.pack_items(&root_bboxes, &mut results);

        // The pack_items function already updates the results with correct positions
        // We just need to update any nested children positions
        for (root_id, _) in &root_bboxes {
            if let Some(root_bbox) = results.get(root_id) {
                self.update_child_positions(root_id, root_bbox.position, &mut results);
            }
        }

        results
    }

    /// Find items (components or modules) that have no parent
    fn find_root_items(&self) -> Vec<String> {
        let mut all_children = HashSet::new();
        for children in self.module_hierarchy.values() {
            for child in children {
                all_children.insert(child.clone());
            }
        }

        // Include both modules without parents and components not in any module
        let mut root_items: Vec<String> = self
            .module_hierarchy
            .keys()
            .filter(|id| !all_children.contains(*id))
            .cloned()
            .collect();

        // Add components that aren't in any module
        for component_id in self.component_sizes.keys() {
            if !all_children.contains(component_id)
                && !self.module_hierarchy.contains_key(component_id)
            {
                root_items.push(component_id.clone());
            }
        }

        root_items
    }

    /// Recursively layout a module and return its bounding box
    fn layout_module_recursive(
        &mut self,
        module_id: &str,
        results: &mut HashMap<String, BoundingBox>,
    ) -> BoundingBox {
        // Check if this is a leaf component
        if let Some(&size) = self.component_sizes.get(module_id) {
            let bbox = BoundingBox::from_position_and_size(Point { x: 0.0, y: 0.0 }, size);
            results.insert(module_id.to_string(), bbox);
            return bbox;
        }

        // Get children of this module
        let children = match self.module_hierarchy.get(module_id) {
            Some(c) => c.clone(),
            None => return BoundingBox::new(0.0, 0.0, 0.0, 0.0),
        };

        // Layout all children
        let mut child_bboxes = Vec::new();
        for child_id in &children {
            let bbox = self.layout_module_recursive(child_id, results);
            child_bboxes.push((child_id.clone(), bbox));
        }

        // Sort children by area (largest first)
        child_bboxes.sort_by(|a, b| b.1.area().partial_cmp(&a.1.area()).unwrap());

        // Pack children using corner-tracking algorithm
        let packed_bbox = self.pack_items(&child_bboxes, results);

        // Store the module's bounding box
        results.insert(module_id.to_string(), packed_bbox);

        packed_bbox
    }

    /// Pack items using the corner-tracking algorithm
    fn pack_items(
        &self,
        items: &[(String, BoundingBox)],
        results: &mut HashMap<String, BoundingBox>,
    ) -> BoundingBox {
        if items.is_empty() {
            return BoundingBox::new(0.0, 0.0, 0.0, 0.0);
        }

        let mut placement_points: Vec<Point> = Vec::new();
        let mut placed_items: Vec<BoundingBox> = Vec::new();
        let mut group_bbox = BoundingBox::new(0.0, 0.0, 0.0, 0.0);

        for (i, (item_id, item_bbox)) in items.iter().enumerate() {
            if i == 0 {
                // First item is placed at origin
                let position = Point { x: 0.0, y: 0.0 };
                let placed_bbox = BoundingBox::from_position_and_size(position, item_bbox.size);

                results.insert(item_id.clone(), placed_bbox);
                placed_items.push(placed_bbox);
                group_bbox = placed_bbox;

                // Add corners as potential placement points
                placement_points.push(Point {
                    x: placed_bbox.max_x() + self.spacing,
                    y: placed_bbox.min_y(),
                }); // right side
                placement_points.push(Point {
                    x: placed_bbox.min_x(),
                    y: placed_bbox.max_y() + self.spacing,
                }); // bottom side
            } else {
                // Try each placement point and find the best one
                let mut best_position = None;
                let mut best_score = f64::INFINITY;

                for point in &placement_points {
                    // Try placing item at this point
                    let test_bbox = BoundingBox::from_position_and_size(*point, item_bbox.size);

                    // Check for collisions
                    let mut collides = false;
                    for placed_bbox in &placed_items {
                        if test_bbox.intersects(placed_bbox) {
                            collides = true;
                            break;
                        }
                    }

                    if !collides {
                        // Calculate score (prefer compact layouts)
                        let test_group_bbox = group_bbox.union(&test_bbox);
                        let score = test_group_bbox.size.width
                            + test_group_bbox.size.height
                            + (test_group_bbox.size.width - test_group_bbox.size.height).abs();

                        if score < best_score {
                            best_score = score;
                            best_position = Some(*point);
                        }
                    }
                }

                if let Some(position) = best_position {
                    // Place the item
                    let placed_bbox = BoundingBox::from_position_and_size(position, item_bbox.size);

                    results.insert(item_id.clone(), placed_bbox);

                    // Update placement points
                    placement_points.retain(|p| *p != position);
                    placement_points.push(Point {
                        x: placed_bbox.max_x() + self.spacing,
                        y: placed_bbox.min_y(),
                    }); // right side
                    placement_points.push(Point {
                        x: placed_bbox.min_x(),
                        y: placed_bbox.max_y() + self.spacing,
                    }); // bottom side

                    // Update group bounds
                    group_bbox = group_bbox.union(&placed_bbox);
                    placed_items.push(placed_bbox);
                }
            }
        }

        // Add spacing around the group
        // Use extra padding for modules to create visual separation between hierarchical layers
        let padding = if items.len() > 1 {
            self.spacing * 2.0 // Double spacing for modules
        } else {
            self.spacing // Normal spacing for single components
        };

        BoundingBox::new(
            group_bbox.min_x() - padding,
            group_bbox.min_y() - padding,
            group_bbox.size.width + 2.0 * padding,
            group_bbox.size.height + 2.0 * padding,
        )
    }

    /// Update positions of all children relative to a parent offset
    fn update_child_positions(
        &self,
        module_id: &str,
        offset: Point,
        results: &mut HashMap<String, BoundingBox>,
    ) {
        if let Some(children) = self.module_hierarchy.get(module_id) {
            for child_id in children {
                if let Some(child_bbox) = results.get(child_id).cloned() {
                    // Update child position
                    let new_bbox = BoundingBox::from_position_and_size(
                        Point {
                            x: child_bbox.position.x + offset.x,
                            y: child_bbox.position.y + offset.y,
                        },
                        child_bbox.size,
                    );
                    results.insert(child_id.clone(), new_bbox);

                    // Recursively update grandchildren
                    self.update_child_positions(child_id, new_bbox.position, results);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_layout() {
        let mut layout = HierarchicalLayout::new(5.0);

        // Set component sizes
        layout.set_component_size("R1".to_string(), Size::new(10.0, 5.0));
        layout.set_component_size("C1".to_string(), Size::new(8.0, 8.0));
        layout.set_component_size("U1".to_string(), Size::new(20.0, 15.0));

        // Create a module containing these components
        layout.add_module(
            "Module1".to_string(),
            vec!["R1".to_string(), "C1".to_string(), "U1".to_string()],
        );

        let bboxes = layout.layout();

        // Check that all components were placed
        assert_eq!(bboxes.len(), 4); // 3 components + 1 module
        assert!(bboxes.contains_key("R1"));
        assert!(bboxes.contains_key("C1"));
        assert!(bboxes.contains_key("U1"));
        assert!(bboxes.contains_key("Module1"));

        // Module should contain all components
        let module_bbox = bboxes.get("Module1").unwrap();
        assert!(module_bbox.size.width >= 20.0); // At least as wide as U1
        assert!(module_bbox.size.height >= 15.0); // At least as tall as U1
    }

    #[test]
    fn test_hierarchical_layout() {
        let mut layout = HierarchicalLayout::new(5.0);

        // Set component sizes
        layout.set_component_size("R1".to_string(), Size::new(10.0, 5.0));
        layout.set_component_size("C1".to_string(), Size::new(8.0, 8.0));
        layout.set_component_size("R2".to_string(), Size::new(10.0, 5.0));
        layout.set_component_size("C2".to_string(), Size::new(8.0, 8.0));

        // Create two modules
        layout.add_module(
            "power".to_string(),
            vec!["R1".to_string(), "C1".to_string()],
        );
        layout.add_module(
            "signal".to_string(),
            vec!["R2".to_string(), "C2".to_string()],
        );

        // Create a parent module containing both sub-modules
        layout.add_module(
            "main".to_string(),
            vec!["power".to_string(), "signal".to_string()],
        );

        let bboxes = layout.layout();

        // Check that all items were placed
        assert_eq!(bboxes.len(), 7); // 4 components + 2 sub-modules + 1 main module

        // Verify hierarchical structure
        let main_bbox = bboxes.get("main").unwrap();
        let power_bbox = bboxes.get("power").unwrap();
        let signal_bbox = bboxes.get("signal").unwrap();

        // Main module should contain both sub-modules
        assert!(main_bbox.size.width >= power_bbox.size.width);
        assert!(main_bbox.size.width >= signal_bbox.size.width);

        // Components in the same module should be close together
        let r1_bbox = bboxes.get("R1").unwrap();
        let c1_bbox = bboxes.get("C1").unwrap();
        let r2_bbox = bboxes.get("R2").unwrap();
        let c2_bbox = bboxes.get("C2").unwrap();

        // Calculate distances
        let r1_c1_dist = ((r1_bbox.position.x - c1_bbox.position.x).powi(2)
            + (r1_bbox.position.y - c1_bbox.position.y).powi(2))
        .sqrt();
        let r2_c2_dist = ((r2_bbox.position.x - c2_bbox.position.x).powi(2)
            + (r2_bbox.position.y - c2_bbox.position.y).powi(2))
        .sqrt();
        let r1_r2_dist = ((r1_bbox.position.x - r2_bbox.position.x).powi(2)
            + (r1_bbox.position.y - r2_bbox.position.y).powi(2))
        .sqrt();

        println!("R1-C1 distance: {r1_c1_dist}");
        println!("R2-C2 distance: {r2_c2_dist}");
        println!("R1-R2 distance: {r1_r2_dist}");

        // Components in different modules should be further apart
        assert!(r1_r2_dist > r1_c1_dist);
        assert!(r1_r2_dist > r2_c2_dist);
    }

    #[test]
    fn test_no_column_layout() {
        let mut layout = HierarchicalLayout::new(5.0);

        // Add many components of similar size
        for i in 1..=10 {
            layout.set_component_size(format!("R{i}"), Size::new(10.0, 5.0));
        }

        let bboxes = layout.layout();

        // Check that components are not all in a single column
        let mut x_positions: Vec<f64> = bboxes.values().map(|b| b.position.x).collect();
        x_positions.sort_by(|a, b| a.partial_cmp(b).unwrap());
        x_positions.dedup();

        // Should have multiple unique X positions (not all in one column)
        assert!(
            x_positions.len() > 1,
            "All components are in a single column!"
        );

        // Print layout for debugging
        println!("Component positions:");
        let mut items: Vec<_> = bboxes.iter().collect();
        items.sort_by_key(|(id, _)| id.as_str());
        for (id, bbox) in items {
            println!("{}: x={:.1}, y={:.1}", id, bbox.position.x, bbox.position.y);
        }
    }
}
