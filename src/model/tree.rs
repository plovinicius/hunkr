//! Arena-backed file tree with folder grouping and collapse/expand.
//!
//! Nodes live in a flat `Vec` and reference each other by index (no `Rc`/
//! `RefCell`, no pointers). `visible` is the flattened, in-order list of node
//! indices that are currently on screen given the collapsed state — the tree
//! panel and cursor navigation operate purely over `visible`.

use std::collections::HashMap;

use crate::model::file::ChangedFile;

#[derive(Debug, Clone)]
pub struct TreeNode {
    pub name: String,
    pub children: Vec<usize>,
    /// `Some(index into files)` for a file leaf; `None` for a folder.
    pub file: Option<usize>,
    pub collapsed: bool,
    pub depth: u16,
}

#[derive(Debug, Clone)]
pub struct FileTree {
    pub nodes: Vec<TreeNode>,
    pub root: usize,
    /// Flattened visible node indices (root itself excluded).
    pub visible: Vec<usize>,
}

impl FileTree {
    /// Build a tree from the changed-file list. Files are grouped by their path
    /// components; intermediate folders are created once and reused.
    pub fn build(files: &[ChangedFile]) -> Self {
        let mut nodes = vec![TreeNode {
            name: String::new(),
            children: Vec::new(),
            file: None,
            collapsed: false,
            depth: 0,
        }];
        let root = 0;

        // Sort by path so siblings come out in a stable, grouped order.
        let mut order: Vec<usize> = (0..files.len()).collect();
        order.sort_by(|&a, &b| files[a].path.cmp(&files[b].path));

        // (parent node, folder name) -> existing folder node index.
        let mut dirs: HashMap<(usize, String), usize> = HashMap::new();

        for &fi in &order {
            let comps: Vec<String> = files[fi]
                .path
                .iter()
                .map(|c| c.to_string_lossy().into_owned())
                .collect();
            let mut parent = root;
            for (i, comp) in comps.iter().enumerate() {
                let is_leaf = i + 1 == comps.len();
                if is_leaf {
                    let idx = nodes.len();
                    nodes.push(TreeNode {
                        name: comp.clone(),
                        children: Vec::new(),
                        file: Some(fi),
                        collapsed: false,
                        depth: i as u16,
                    });
                    nodes[parent].children.push(idx);
                } else {
                    let key = (parent, comp.clone());
                    if let Some(&existing) = dirs.get(&key) {
                        parent = existing;
                    } else {
                        let idx = nodes.len();
                        nodes.push(TreeNode {
                            name: comp.clone(),
                            children: Vec::new(),
                            file: None,
                            collapsed: false,
                            depth: i as u16,
                        });
                        nodes[parent].children.push(idx);
                        dirs.insert(key, idx);
                        parent = idx;
                    }
                }
            }
        }

        let mut tree = FileTree {
            nodes,
            root,
            visible: Vec::new(),
        };
        tree.recompute_visible();
        tree
    }

    pub fn is_dir(&self, node: usize) -> bool {
        self.nodes[node].file.is_none()
    }

    /// Toggle a folder's collapsed state and rebuild the visible list.
    pub fn toggle(&mut self, node: usize) {
        if self.is_dir(node) {
            self.nodes[node].collapsed = !self.nodes[node].collapsed;
            self.recompute_visible();
        }
    }

    /// Recompute `visible` via a pre-order DFS, descending into folders only
    /// when they are expanded.
    pub fn recompute_visible(&mut self) {
        fn dfs(nodes: &[TreeNode], idx: usize, out: &mut Vec<usize>) {
            for &child in &nodes[idx].children {
                out.push(child);
                if nodes[child].file.is_none() && !nodes[child].collapsed {
                    dfs(nodes, child, out);
                }
            }
        }
        let mut visible = Vec::new();
        dfs(&self.nodes, self.root, &mut visible);
        self.visible = visible;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::file::ChangeKind;
    use std::path::PathBuf;

    fn file(p: &str) -> ChangedFile {
        ChangedFile::new(PathBuf::from(p), ChangeKind::Modified)
    }

    #[test]
    fn groups_by_folder_and_reuses_dirs() {
        let files = vec![file("src/foo.rs"), file("src/bar.rs"), file("README.md")];
        let tree = FileTree::build(&files);
        // README.md, src/, src/bar.rs, src/foo.rs  (sorted: README before src)
        let names: Vec<&str> = tree
            .visible
            .iter()
            .map(|&n| tree.nodes[n].name.as_str())
            .collect();
        assert_eq!(names, vec!["README.md", "src", "bar.rs", "foo.rs"]);
    }

    #[test]
    fn collapse_hides_children() {
        let files = vec![file("src/foo.rs"), file("src/bar.rs")];
        let mut tree = FileTree::build(&files);
        // Find the `src` folder node and collapse it.
        let src = tree
            .nodes
            .iter()
            .position(|n| n.name == "src" && n.file.is_none())
            .unwrap();
        tree.toggle(src);
        let names: Vec<&str> = tree
            .visible
            .iter()
            .map(|&n| tree.nodes[n].name.as_str())
            .collect();
        assert_eq!(names, vec!["src"]);
    }
}
