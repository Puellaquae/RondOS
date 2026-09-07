//! ramfs: an in-memory file system.
//!
//! Nodes form a simple directory tree; file contents live in heap `Vec`s.
//! This is the backing store for the VFS until real on-disk file systems
//! arrive with the user-mode milestone.  All paths are byte slices (no UTF-8
//! assumptions) so tar names pass through untouched.

#![allow(dead_code)]

use alloc::vec::Vec;

pub enum NodeKind {
    Dir(Vec<Node>),
    File(Vec<u8>),
}

pub struct Node {
    pub name: Vec<u8>,
    pub kind: NodeKind,
}

impl Node {
    fn dir(name: &[u8]) -> Node {
        Node {
            name: name.to_vec(),
            kind: NodeKind::Dir(Vec::new()),
        }
    }

    fn file(name: &[u8], data: Vec<u8>) -> Node {
        Node {
            name: name.to_vec(),
            kind: NodeKind::File(data),
        }
    }
}

pub struct RamFs {
    pub root: Node,
}

impl RamFs {
    pub fn new() -> RamFs {
        RamFs {
            root: Node::dir(b"/"),
        }
    }

    /// Split a path into non-empty components (ignores `//` and `.`).
    fn components<'a>(path: &'a [u8]) -> Vec<&'a [u8]> {
        path.split(|&b| b == b'/')
            .filter(|c| !c.is_empty() && *c != b".")
            .collect()
    }

    /// Walk/create the chain of directories in `comps`, returning the
    /// directory's child list.  The final component is NOT consumed.
    fn ensure_dir<'a>(mut cur: &'a mut Vec<Node>, comps: &[&[u8]]) -> &'a mut Vec<Node> {
        for comp in comps {
            let pos = cur.iter().position(|n| n.name == *comp);
            let Some(i) = pos else {
                // Not present: create the directory and descend into it.
                cur.push(Node::dir(comp));
                let last = cur.len() - 1;
                match &mut cur[last].kind {
                    NodeKind::Dir(ch) => cur = ch,
                    _ => unreachable!(),
                }
                continue;
            };
            // Present: if it is a file the path is invalid; stop where we are.
            if matches!(cur[i].kind, NodeKind::File(_)) {
                break;
            }
            match &mut cur[i].kind {
                NodeKind::Dir(ch) => cur = ch,
                _ => unreachable!(),
            }
        }
        cur
    }

    /// Ensure a directory exists at `path`, creating parents as needed.
    pub fn mkdir(&mut self, path: &[u8]) -> bool {
        let comps = Self::components(path);
        let root_children = self.root.kind_dir();
        let _ = Self::ensure_dir(root_children, &comps);
        true
    }

    /// Create or replace the file at `path`.
    pub fn write_file(&mut self, path: &[u8], data: Vec<u8>) -> bool {
        let comps = Self::components(path);
        if comps.is_empty() {
            return false;
        }
        let (parents, name) = comps.split_at(comps.len() - 1);
        let root_children = self.root.kind_dir();
        let children = Self::ensure_dir(root_children, parents);
        if let Some(i) = children.iter().position(|n| n.name == name[0]) {
            children[i] = Node::file(name[0], data);
        } else {
            children.push(Node::file(name[0], data));
        }
        true
    }

    fn lookup<'a>(&'a self, path: &[u8]) -> Option<&'a Node> {
        let comps = Self::components(path);
        let mut node = &self.root;
        for comp in &comps {
            match &node.kind {
                NodeKind::Dir(children) => match children.iter().find(|n| &n.name[..] == *comp) {
                    Some(n) => node = n,
                    None => return None,
                },
                NodeKind::File(_) => return None,
            }
        }
        Some(node)
    }

    /// Read a whole file as a slice.
    pub fn read_file(&self, path: &[u8]) -> Option<&[u8]> {
        match self.lookup(path)?.kind {
            NodeKind::File(ref data) => Some(data),
            NodeKind::Dir(_) => None,
        }
    }

    /// List a directory: `(name, is_dir)` pairs.
    pub fn list(&self, path: &[u8]) -> Option<Vec<(Vec<u8>, bool)>> {
        let node = self.lookup(path)?;
        match &node.kind {
            NodeKind::Dir(children) => Some(
                children
                    .iter()
                    .map(|n| {
                        let is_dir = matches!(n.kind, NodeKind::Dir(_));
                        (n.name.clone(), is_dir)
                    })
                    .collect(),
            ),
            NodeKind::File(_) => None,
        }
    }
}

impl Node {
    fn kind_dir(&mut self) -> &mut Vec<Node> {
        match &mut self.kind {
            NodeKind::Dir(ch) => ch,
            NodeKind::File(_) => {
                self.kind = NodeKind::Dir(Vec::new());
                match &mut self.kind {
                    NodeKind::Dir(ch) => ch,
                    _ => unreachable!(),
                }
            }
        }
    }
}
