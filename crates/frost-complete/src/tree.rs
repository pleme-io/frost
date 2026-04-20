//! Completion tree — the data structure `FrostCompleter` walks at Tab
//! time to drive subcommand / flag / positional completion.
//!
//! Built from `frost_lisp::{SubcmdSpec, FlagSpec, PositSpec}` via
//! [`CompletionTree::build`]. Each node represents a command or
//! subcommand identified by a dotted path (`git`, `git.commit`,
//! `kubectl.get.pods`). Per-node children:
//!
//!   * `subcommands` — `name → (description, sub-node)`
//!   * `flags`       — `name → FlagNode { takes, description }`
//!   * `positionals` — `index → PositNode { takes, description }`
//!
//! The completer uses this to answer three per-Tab questions:
//!
//!   1. "Which node am I at?" — walk the cmdline, match successive
//!      words against the tree's subcommand children. The deepest
//!      match is the current node.
//!   2. "What am I completing?" — classify the current word as a
//!      subcommand candidate, a flag, or the value of a flag / a
//!      positional, based on the preceding token.
//!   3. "Which candidates fit?" — pull from the current node's
//!      `subcommands` / `flags` / `positionals` tables and apply
//!      value-kind-specific completion (choice enum, filesystem walk,
//!      or free text).

use std::collections::BTreeMap;

use frost_lisp::{FlagSpec, PositSpec, SubcmdSpec, ValueKind};

/// A single flag definition at a tree node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlagNode {
    /// Parsed value kind (None = bool flag).
    pub takes: Option<ValueKind>,
    pub description: Option<String>,
}

/// A single positional slot at a tree node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PositNode {
    pub takes: ValueKind,
    pub description: Option<String>,
}

/// One node in the completion tree.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CompletionNode {
    /// Subcommand name → (description, child node).
    pub subcommands: BTreeMap<String, (Option<String>, CompletionNode)>,
    /// Flag name → node. BTreeMap so Tab enumerates them in
    /// deterministic (alphabetical) order.
    pub flags: BTreeMap<String, FlagNode>,
    /// 1-based index → positional node.
    pub positionals: BTreeMap<u32, PositNode>,
}

/// A completion tree indexed by top-level command name.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CompletionTree {
    pub commands: BTreeMap<String, CompletionNode>,
}

impl CompletionTree {
    /// Assemble a tree from the flat spec vectors produced by
    /// [`frost_lisp::apply_source`]. Specs with malformed paths
    /// (empty, all-dots) are silently dropped — rc errors out at
    /// parse time for typed field mismatches, so anything reaching
    /// here is structurally valid; we're defensive about pathological
    /// path strings because they don't warrant a hard crash.
    pub fn build(subcmds: &[SubcmdSpec], flags: &[FlagSpec], positionals: &[PositSpec]) -> Self {
        let mut tree = CompletionTree::default();

        // Subcommand forms: path is the PARENT, name is the new child.
        // For `(defsubcmd :path "git" :name "commit")`, the child node
        // lives at path `git.commit`; the parent node `git` gains the
        // child in its `subcommands` map.
        for s in subcmds {
            let parent = tree.ensure_node_at(&s.path);
            parent
                .subcommands
                .entry(s.name.clone())
                .or_insert_with(|| (s.description.clone(), CompletionNode::default()));
            // Also materialize the CHILD node so later flag/posit lookups find it.
            let child_path = if s.path.is_empty() {
                s.name.clone()
            } else {
                format!("{}.{}", s.path, s.name)
            };
            tree.ensure_node_at(&child_path);
        }

        for f in flags {
            let node = tree.ensure_node_at(&f.path);
            node.flags.insert(
                f.name.clone(),
                FlagNode {
                    takes: f.takes.as_deref().map(ValueKind::parse),
                    description: f.description.clone(),
                },
            );
        }

        for p in positionals {
            let node = tree.ensure_node_at(&p.path);
            node.positionals.insert(
                p.index,
                PositNode {
                    takes: p
                        .takes
                        .as_deref()
                        .map(ValueKind::parse)
                        .unwrap_or(ValueKind::String),
                    description: p.description.clone(),
                },
            );
        }

        tree
    }

    /// True if we have any knowledge about `command`. Used as a fast
    /// check before walking — the completer can skip tree lookup for
    /// unknown commands and fall through to filesystem completion.
    pub fn knows(&self, command: &str) -> bool {
        self.commands.contains_key(command)
    }

    /// Walk dotted path `dotted_path` and return `(node, remaining)`.
    /// `remaining` is the tail of the path that didn't resolve —
    /// callers use this when a user has typed past the depth of the
    /// tree.
    pub fn walk(&self, dotted_path: &str) -> Option<&CompletionNode> {
        let mut parts = dotted_path.split('.').filter(|s| !s.is_empty());
        let head = parts.next()?;
        let mut cur = self.commands.get(head)?;
        for part in parts {
            let (_, child) = cur.subcommands.get(part)?;
            cur = child;
        }
        Some(cur)
    }

    /// Ensure a node exists at `path`, creating intermediate nodes as
    /// needed. Returns a mutable reference to the deepest node.
    fn ensure_node_at(&mut self, path: &str) -> &mut CompletionNode {
        if path.is_empty() {
            // Root lookup — not meaningful for commands. Caller should
            // pass at least a top-level name. We return a scratch node
            // by ensuring a placeholder under an empty-key slot.
            return self.commands.entry(String::new()).or_default();
        }
        let mut parts = path.split('.').filter(|s| !s.is_empty());
        let Some(head) = parts.next() else {
            return self.commands.entry(String::new()).or_default();
        };
        let mut cur = self.commands.entry(head.to_string()).or_default();
        for part in parts {
            cur = &mut cur
                .subcommands
                .entry(part.to_string())
                .or_insert_with(|| (None, CompletionNode::default()))
                .1;
        }
        cur
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sub(path: &str, name: &str, desc: &str) -> SubcmdSpec {
        SubcmdSpec {
            path: path.into(),
            name: name.into(),
            description: Some(desc.into()),
        }
    }

    fn flg(path: &str, name: &str, takes: Option<&str>, desc: &str) -> FlagSpec {
        FlagSpec {
            path: path.into(),
            name: name.into(),
            takes: takes.map(String::from),
            description: Some(desc.into()),
        }
    }

    fn pos(path: &str, idx: u32, takes: &str, desc: &str) -> PositSpec {
        PositSpec {
            path: path.into(),
            index: idx,
            takes: Some(takes.into()),
            description: Some(desc.into()),
        }
    }

    #[test]
    fn tree_builds_basic_subcommand_hierarchy() {
        let tree = CompletionTree::build(
            &[
                sub("git", "commit", "record changes"),
                sub("git", "checkout", "switch branches"),
                sub("git.commit", "--signoff", "add sign-off"), // nested sub
            ],
            &[],
            &[],
        );
        assert!(tree.knows("git"));
        let git = tree.walk("git").unwrap();
        assert_eq!(git.subcommands.len(), 2);
        assert!(git.subcommands.contains_key("commit"));
        assert!(git.subcommands.contains_key("checkout"));
        let commit = tree.walk("git.commit").unwrap();
        assert_eq!(commit.subcommands.len(), 1);
        assert!(commit.subcommands.contains_key("--signoff"));
    }

    #[test]
    fn tree_attaches_flags_and_positionals() {
        let tree = CompletionTree::build(
            &[sub("git", "commit", "record changes")],
            &[
                flg("git.commit", "-m", Some("string"), "commit message"),
                flg("git.commit", "--amend", None, "replace last commit"),
            ],
            &[pos("git.commit", 1, "files", "paths to commit")],
        );
        let commit = tree.walk("git.commit").unwrap();
        assert_eq!(commit.flags.len(), 2);
        let m = commit.flags.get("-m").unwrap();
        assert_eq!(m.takes, Some(ValueKind::String));
        assert_eq!(m.description.as_deref(), Some("commit message"));
        let amend = commit.flags.get("--amend").unwrap();
        assert_eq!(amend.takes, None);
        assert_eq!(commit.positionals.len(), 1);
        assert_eq!(commit.positionals[&1].takes, ValueKind::Files);
    }

    #[test]
    fn walk_returns_none_for_unknown_path() {
        let tree = CompletionTree::build(&[sub("git", "commit", "x")], &[], &[]);
        assert!(tree.walk("unknowncmd").is_none());
        assert!(tree.walk("git.unknown").is_none());
    }

    #[test]
    fn walk_handles_nested_depth() {
        let tree = CompletionTree::build(
            &[
                sub("kubectl", "get", "read"),
                sub("kubectl.get", "pods", "list pods"),
                sub("kubectl.get.pods", "--all-namespaces", "all ns"),
            ],
            &[],
            &[],
        );
        assert!(tree.walk("kubectl").is_some());
        assert!(tree.walk("kubectl.get").is_some());
        assert!(tree.walk("kubectl.get.pods").is_some());
        let deep = tree.walk("kubectl.get.pods").unwrap();
        assert!(deep.subcommands.contains_key("--all-namespaces"));
    }
}
