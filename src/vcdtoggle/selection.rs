//! Target-scope lookup and the kept-signal filter (vendored from
//! `vcdtogglemeasure`): clock/reset/debug/hidden names and names starting with
//! `_` are always dropped, plus anything in the exclusion file.

use super::excludes::Excluded;
use wellen::{Hierarchy, ScopeRef, SignalRef, Var};

/// Resolves the dotted scope path (e.g. `TOP.dut`) to a scope; error if absent.
pub fn lookup_target(hierarchy: &Hierarchy, target_scope: &str) -> Result<ScopeRef, String> {
    let target_scope_path: Vec<&str> = target_scope.split('.').collect();
    hierarchy
        .lookup_scope(&target_scope_path)
        .ok_or_else(|| format!("Scope {target_scope} not found in hierarchy"))
}

/// Signal references of all kept variables below the target scope, deduplicated
/// (aliased variables share one signal).
pub fn unique_signal_refs(
    hierarchy: &Hierarchy,
    target_scope: ScopeRef,
    excluded: &Excluded,
) -> Vec<SignalRef> {
    let mut refs: std::collections::HashSet<SignalRef> = std::collections::HashSet::new();
    for var_ref in hierarchy[target_scope].all_vars(hierarchy) {
        let var = &hierarchy[var_ref];
        if keep_signal(hierarchy, var, excluded) {
            refs.insert(var.signal_ref());
        }
    }
    // id order is declaration order: keeps related signals adjacent in memory
    let mut refs: Vec<SignalRef> = refs.into_iter().collect();
    refs.sort_unstable();
    refs
}

/// Whether a variable is counted: clock/reset/debug/hidden-style names and `_`-prefixed
/// names are always dropped, as are explicitly excluded full names.
pub fn keep_signal(hierarchy: &Hierarchy, var: &Var, excluded: &Excluded) -> bool {
    let name = var.name(hierarchy);

    !(name.contains("clock")
        || name.contains("reset")
        || name.contains("debug")
        || name.contains("hidden")
        || name.contains("clk")
        || name.starts_with('_')
        || (!excluded.is_empty() && excluded.contains(&var.full_name(hierarchy))))
}
