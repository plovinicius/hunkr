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
    /// when they are expanded. Keeps every file.
    pub fn recompute_visible(&mut self) {
        self.recompute_visible_with(|_| true);
    }

    /// Recompute `visible`, keeping only file leaves for which `keep(file_idx)`
    /// is true, and folders that (transitively) contain at least one kept file.
    /// Used to drop hidden files — and the folders that become empty once their
    /// only files are hidden — from the sidebar.
    pub fn recompute_visible_with<F: Fn(usize) -> bool>(&mut self, keep: F) {
        fn subtree_keeps_any<F: Fn(usize) -> bool>(
            nodes: &[TreeNode],
            idx: usize,
            keep: &F,
        ) -> bool {
            nodes[idx].children.iter().any(|&c| match nodes[c].file {
                Some(fi) => keep(fi),
                None => subtree_keeps_any(nodes, c, keep),
            })
        }
        fn dfs<F: Fn(usize) -> bool>(
            nodes: &[TreeNode],
            idx: usize,
            keep: &F,
            out: &mut Vec<usize>,
        ) {
            for &child in &nodes[idx].children {
                match nodes[child].file {
                    Some(fi) => {
                        if keep(fi) {
                            out.push(child);
                        }
                    }
                    None => {
                        if subtree_keeps_any(nodes, child, keep) {
                            out.push(child);
                            if !nodes[child].collapsed {
                                dfs(nodes, child, keep, out);
                            }
                        }
                    }
                }
            }
        }
        let mut visible = Vec::new();
        dfs(&self.nodes, self.root, &keep, &mut visible);
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

    #[test]
    fn recompute_visible_with_prunes_files_and_empty_folders() {
        // src/foo.rs, src/bar.rs, README.md. Hide both files under src/ → the
        // src/ folder should disappear too, leaving only README.md.
        let files = vec![file("src/foo.rs"), file("src/bar.rs"), file("README.md")];
        let mut tree = FileTree::build(&files);

        let hidden: std::collections::HashSet<usize> = [0, 1].into_iter().collect();
        tree.recompute_visible_with(|fi| !hidden.contains(&fi));

        let names: Vec<&str> = tree
            .visible
            .iter()
            .map(|&n| tree.nodes[n].name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["README.md"],
            "empty src/ folder should be pruned"
        );

        // The inverse view (only the hidden files) keeps src/ and its two files.
        tree.recompute_visible_with(|fi| hidden.contains(&fi));
        let names: Vec<&str> = tree
            .visible
            .iter()
            .map(|&n| tree.nodes[n].name.as_str())
            .collect();
        assert_eq!(names, vec!["src", "bar.rs", "foo.rs"]);
    }
}
