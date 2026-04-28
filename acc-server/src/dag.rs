//! DAG utilities for fleet task scheduling.
//!
//! Task dependencies form a directed acyclic graph where an edge from A to B
//! means "A depends on B" (A is blocked by B; A cannot run until B completes).
//!
//! The graph is stored implicitly: each task's `blocked_by` column holds a JSON
//! array of task IDs that must complete before it can be claimed.

use std::collections::{HashMap, HashSet};

/// Returns `true` if adding `new_id → new_blocked_by` edges would create a cycle.
///
/// `graph` maps task_id → its current `blocked_by` list (i.e. "depends on" edges).
/// A cycle is created when any of the proposed blockers can reach `new_id` by
/// following existing dependency edges — which would mean `new_id` transitively
/// depends on itself.
///
/// For a brand-new task (not yet in the graph), this can only fire if `new_id`
/// appears directly in `new_blocked_by` (self-dependency). For PATCH updates to an
/// existing task's `blocked_by`, it catches transitive cycles.
pub fn would_create_cycle(
    graph: &HashMap<String, Vec<String>>,
    new_id: &str,
    new_blocked_by: &[String],
) -> bool {
    // DFS from each proposed blocker: if we can reach new_id, we'd have a cycle.
    let mut visited: HashSet<&str> = HashSet::new();
    let mut stack: Vec<&str> = new_blocked_by.iter().map(|s| s.as_str()).collect();

    while let Some(node) = stack.pop() {
        if node == new_id {
            return true;
        }
        if !visited.insert(node) {
            continue;
        }
        if let Some(deps) = graph.get(node) {
            for dep in deps {
                stack.push(dep.as_str());
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(pairs: &[(&str, &[&str])]) -> HashMap<String, Vec<String>> {
        pairs
            .iter()
            .map(|(k, vs)| (k.to_string(), vs.iter().map(|s| s.to_string()).collect()))
            .collect()
    }

    #[test]
    fn new_task_with_no_deps_is_safe() {
        let graph = g(&[("A", &["B"]), ("B", &["C"])]);
        assert!(!would_create_cycle(&graph, "D", &[]));
    }

    #[test]
    fn new_task_depending_on_existing_is_safe() {
        // A→B, B→C; adding D→A is fine
        let graph = g(&[("A", &["B"]), ("B", &["C"])]);
        assert!(!would_create_cycle(&graph, "D", &["A".into()]));
    }

    #[test]
    fn self_cycle_detected() {
        let graph = HashMap::new();
        assert!(would_create_cycle(&graph, "A", &["A".into()]));
    }

    #[test]
    fn direct_cycle_detected() {
        // B already depends on A; trying to make A depend on B → cycle
        let graph = g(&[("B", &["A"])]);
        assert!(would_create_cycle(&graph, "A", &["B".into()]));
    }

    #[test]
    fn transitive_cycle_detected() {
        // A→B→C; setting C.blocked_by=[A] would create A→B→C→A
        let graph = g(&[("A", &["B"]), ("B", &["C"])]);
        assert!(would_create_cycle(&graph, "C", &["A".into()]));
    }

    #[test]
    fn parallel_branches_are_safe() {
        // A→C, B→C (diamond); adding D→A and D→B is fine
        let graph = g(&[("A", &["C"]), ("B", &["C"])]);
        assert!(!would_create_cycle(&graph, "D", &["A".into(), "B".into()]));
    }

    #[test]
    fn long_chain_cycle_detected() {
        // Chain: 1→2→3→4→5; making 5 depend on 1 would close the loop
        let graph = g(&[("2", &["1"]), ("3", &["2"]), ("4", &["3"]), ("5", &["4"])]);
        assert!(would_create_cycle(&graph, "1", &["5".into()]));
    }

    #[test]
    fn no_false_positive_on_shared_ancestor() {
        // A→Z, B→Z; adding C→A and C→B is fine (Z is a shared dependency, not a cycle)
        let graph = g(&[("A", &["Z"]), ("B", &["Z"])]);
        assert!(!would_create_cycle(&graph, "C", &["A".into(), "B".into()]));
    }
}
