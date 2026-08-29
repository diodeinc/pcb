//! Simple tree printing helper using termtree.

/// Build a tree from a root and children using a visitor function.
///
/// - `root_label`: label for the root node
/// - `roots`: children of the root node
/// - `visit`: for each node, returns (label, children)
pub fn build_tree<Id, Roots, F>(
    root_label: String,
    roots: Roots,
    mut visit: F,
) -> termtree::Tree<String>
where
    Id: Clone,
    Roots: IntoIterator<Item = Id>,
    F: FnMut(&Id) -> (String, Vec<Id>),
{
    fn build_node<Id: Clone, F: FnMut(&Id) -> (String, Vec<Id>)>(
        id: Id,
        visit: &mut F,
    ) -> termtree::Tree<String> {
        let (label, children) = visit(&id);
        let leaves: Vec<termtree::Tree<String>> = children
            .into_iter()
            .map(|child| build_node(child, visit))
            .collect();
        termtree::Tree::new(label).with_leaves(leaves)
    }

    let leaves: Vec<termtree::Tree<String>> = roots
        .into_iter()
        .map(|id| build_node(id, &mut visit))
        .collect();
    termtree::Tree::new(root_label).with_leaves(leaves)
}

/// Print a tree to stdout.
///
/// - `root_label`: label for the root node
/// - `roots`: children of the root node
/// - `visit`: for each node, returns (label, children)
pub fn print_tree<Id, Roots, F>(root_label: String, roots: Roots, visit: F) -> std::io::Result<()>
where
    Id: Clone,
    Roots: IntoIterator<Item = Id>,
    F: FnMut(&Id) -> (String, Vec<Id>),
{
    let tree = build_tree(root_label, roots, visit);
    print!("{}", tree);
    Ok(())
}
