//! Native AX-tree capture: walks Chromium's accessibility tree (fetched via
//! CDP `Accessibility.getFullAXTree`) into the platform-agnostic `AxSnapshot`
//! shape, assigning each surviving interactive node a session-scoped `ref`
//! string.
//!
//! Chromium builds its accessibility tree lazily: a `getFullAXTree` call
//! issued immediately after a navigation's load event can come back
//! empty or partial even though the DOM itself has fully loaded
//! (`research/pitfalls.md` §1, citing `research/stack.md`). `capture_snapshot`
//! itself stays a single-shot call — it does not retry or poll. A caller that
//! just navigated (`NativeBrowser::navigate`, Epic 3 Story 3.2) is
//! responsible for its own bounded priming retry when the first call comes
//! back with zero children; this function has no "did we just navigate"
//! context that would let it make that judgment call correctly.

use std::cell::Cell;
use std::collections::HashMap;

use chromiumoxide::cdp::browser_protocol::accessibility::{AxValue, GetFullAxTreeParams};
use chromiumoxide::cdp::browser_protocol::dom::BackendNodeId;
use chromiumoxide::Page;

use stapler_mcp_core::ports::{AxNode, AxSnapshot, PortError};

/// Upper bound on how many non-root nodes a single `AxSnapshot` may contain
/// before `build_tree` stops descending and sets `AxSnapshot.truncated`
/// (UX AC #6: truncation must be legible — flagged explicitly, never a
/// silent cutoff the caller can't detect).
const MAX_SNAPSHOT_NODES: usize = 500;

/// `node_ref` given to the snapshot's own root node. The root represents the
/// whole document, not a clickable/typeable element, so — unlike every other
/// surviving node — it is never a valid `resolve_locator` target and
/// deliberately does not consume `next_ref_id`'s counter (see the Task 3.1.1
/// AC: a 3-node tree with 1 root + 1 surviving child advances `next_ref_id`
/// by exactly 1, not 2).
const ROOT_REF: &str = "root";

/// One CDP `Accessibility.AXNode`, reduced to the fields `build_tree` needs.
/// Kept separate from `chromiumoxide_cdp`'s generated `AxNode` type so the
/// tree-walk/pruning/ref-assignment logic below is unit-testable with plain
/// struct literals instead of having to construct real CDP wire types.
#[derive(Debug, Clone)]
struct RawAxNode {
    node_id: String,
    parent_id: Option<String>,
    ignored: bool,
    role: Option<String>,
    name: Option<String>,
    value: Option<String>,
    backend_node_id: Option<i64>,
}

/// A `ref`-resolved element handle: the `BackendNodeId` `browser.rs`'s
/// `resolve_locator` needs to dispatch a click/type call, plus the `role`
/// the node had at snapshot time (so `verify_node_live`, Task 3.1.4, can
/// detect Chromium silently reusing a `BackendNodeId` for a different kind
/// of element between snapshot and dispatch).
#[derive(Debug, Clone)]
pub struct ResolvedRef {
    pub backend_node_id: BackendNodeId,
    pub role: String,
}

/// `capture_snapshot`'s full result: the platform-agnostic `AxSnapshot`
/// (threaded all the way up through `BrowserDriver`) plus the native-only
/// `ref -> ResolvedRef` table `browser.rs` stores into the session's
/// `latest_refs`.
pub struct AxCapture {
    pub snapshot: AxSnapshot,
    pub refs: HashMap<String, ResolvedRef>,
}

/// Fetches the full accessibility tree for `page`'s current document and
/// converts it into an `AxCapture`. `next_ref_id` must be the session's own
/// counter (never a freshly-constructed one) — see the module doc comment on
/// why ref strings must never be reused within a session's lifetime.
///
/// `previous_refs` is the session's current `latest_refs` (empty for a fresh
/// navigation): a `BackendNodeId` that appears in both `previous_refs` and
/// this capture keeps the same `ref` string rather than being reassigned a
/// new one. Without this, every capture — including the one `click`/
/// `type_text` issue after dispatch to reflect the resulting DOM mutation —
/// walks the tree from scratch and hands out fresh sequential `eN` strings to
/// every surviving node, silently invalidating every ref a caller was handed
/// by an *earlier* capture even though the underlying page never navigated
/// (see `browser.rs`'s `resolve_locator_impl`, whose "no element with ref"
/// error this produced for any ref used after an intervening click).
pub async fn capture_snapshot(
    page: &Page,
    next_ref_id: &Cell<u64>,
    previous_refs: &HashMap<String, ResolvedRef>,
) -> Result<AxCapture, PortError> {
    // `research/stack.md` §CDP surface flags recent Chrome as requiring an
    // explicit `frameId` on `getFullAXTree` or it returns only the root
    // node — scope the call to the page's main frame defensively even
    // though it wasn't what produced the empty-children symptom this
    // module now fixes (see `walk_children` below for the real cause).
    let frame_id = page
        .mainframe()
        .await
        .map_err(|e| PortError::Other(e.to_string()))?;
    let params = match frame_id {
        Some(id) => GetFullAxTreeParams::builder().frame_id(id).build(),
        None => GetFullAxTreeParams::default(),
    };
    let resp = page
        .execute(params)
        .await
        .map_err(|e| PortError::Other(e.to_string()))?;
    let url = page
        .url()
        .await
        .map_err(|e| PortError::Other(e.to_string()))?
        .unwrap_or_default();

    let raw: Vec<RawAxNode> = resp
        .result
        .nodes
        .iter()
        .map(|n| RawAxNode {
            node_id: n.node_id.inner().to_string(),
            parent_id: n.parent_id.as_ref().map(|p| p.inner().to_string()),
            ignored: n.ignored,
            role: n.role.as_ref().and_then(ax_value_to_string),
            name: n.name.as_ref().and_then(ax_value_to_string),
            value: n.value.as_ref().and_then(ax_value_to_string),
            backend_node_id: n.backend_dom_node_id.as_ref().map(|id| *id.inner()),
        })
        .collect();

    Ok(build_tree(&raw, next_ref_id, url, previous_refs))
}

/// Best-effort extraction of a plain string out of an `AXValue`'s `optional
/// any value` field (CDP's `Accessibility.AXValue.value` is untyped JSON —
/// usually a JSON string for `role`/`name`, but falls back to the value's
/// JSON text for anything else rather than silently dropping it).
pub(crate) fn ax_value_to_string(v: &AxValue) -> Option<String> {
    let raw = v.value.as_ref()?;
    Some(match raw.as_str() {
        Some(s) => s.to_string(),
        None => raw.to_string(),
    })
}

/// Pure tree-walk: parents `nodes` by `parent_id`, prunes any node with
/// `ignored: true` (and everything under it is still visited independently —
/// an ignored node's *children* are not automatically ignored, matching
/// Chromium's own AX semantics), assigns a monotonic `ref` to every
/// surviving non-root node it has not seen before in `previous_refs` (a node
/// whose `BackendNodeId` matches one already resolved keeps that same `ref`
/// string), and caps the walk at `MAX_SNAPSHOT_NODES` surviving nodes.
fn build_tree(
    nodes: &[RawAxNode],
    next_ref_id: &Cell<u64>,
    url: String,
    previous_refs: &HashMap<String, ResolvedRef>,
) -> AxCapture {
    let mut by_parent: HashMap<Option<String>, Vec<&RawAxNode>> = HashMap::new();
    for n in nodes {
        by_parent.entry(n.parent_id.clone()).or_default().push(n);
    }

    // Reverse index so `walk_children` can look a node's prior `ref` string
    // up by `BackendNodeId` in O(1) instead of scanning `previous_refs` per
    // node. Ambiguous if a `BackendNodeId` somehow had more than one ref in
    // the prior capture (shouldn't happen — `install_snapshot` only ever
    // installs one `AxCapture`'s worth of refs at a time); last-write-wins in
    // that case, which is no worse than picking arbitrarily.
    let mut previous_by_backend_id: HashMap<i64, &str> = HashMap::new();
    for (r, resolved) in previous_refs {
        previous_by_backend_id.insert(*resolved.backend_node_id.inner(), r.as_str());
    }

    let mut refs = HashMap::new();
    let mut count = 0usize;
    let mut truncated = false;

    let root_raw = nodes.iter().find(|n| n.parent_id.is_none());

    let root = match root_raw {
        Some(r) => {
            let children = walk_children(
                r,
                &by_parent,
                next_ref_id,
                &mut refs,
                &mut count,
                &mut truncated,
                &previous_by_backend_id,
            );
            AxNode {
                node_ref: ROOT_REF.to_string(),
                role: r.role.clone().unwrap_or_default(),
                name: r.name.clone().unwrap_or_default(),
                value: r.value.clone(),
                children,
            }
        }
        None => AxNode {
            node_ref: ROOT_REF.to_string(),
            role: String::new(),
            name: String::new(),
            value: None,
            children: Vec::new(),
        },
    };

    AxCapture {
        snapshot: AxSnapshot {
            root,
            url,
            truncated,
            navigated_from: None,
        },
        refs,
    }
}

fn walk_children<'a>(
    parent: &'a RawAxNode,
    by_parent: &HashMap<Option<String>, Vec<&'a RawAxNode>>,
    next_ref_id: &Cell<u64>,
    refs: &mut HashMap<String, ResolvedRef>,
    count: &mut usize,
    truncated: &mut bool,
    previous_by_backend_id: &HashMap<i64, &str>,
) -> Vec<AxNode> {
    let mut out = Vec::new();
    let Some(children) = by_parent.get(&Some(parent.node_id.clone())) else {
        return out;
    };
    for child in children {
        if child.ignored {
            // Per the module/doc comment above: an ignored node's *children*
            // are not automatically ignored. Chromium routinely wraps real
            // content in an `ignored: true` structural/generic node (e.g. a
            // layout wrapper `<div>`), so skipping the ignored node here
            // without still descending into it would silently drop every
            // interactive descendant — exactly the "root has zero children"
            // failure this comment warned about but the code didn't
            // actually implement. Splice its surviving children in as if
            // they were direct children of `parent`.
            let mut spliced = walk_children(
                child,
                by_parent,
                next_ref_id,
                refs,
                count,
                truncated,
                previous_by_backend_id,
            );
            out.append(&mut spliced);
            continue;
        }
        if *count >= MAX_SNAPSHOT_NODES {
            *truncated = true;
            continue;
        }
        *count += 1;

        let grandchildren = walk_children(
            child,
            by_parent,
            next_ref_id,
            refs,
            count,
            truncated,
            previous_by_backend_id,
        );

        // Reuse the ref string this same `BackendNodeId` already had in the
        // previous capture (if any) instead of always minting a new one —
        // otherwise a node that is still live and unchanged gets a brand-new
        // ref on every single capture, invalidating any ref a caller is
        // still holding from an earlier snapshot in the same navigation.
        let node_ref = match child.backend_node_id.and_then(|id| previous_by_backend_id.get(&id)) {
            Some(existing) => existing.to_string(),
            None => {
                let n = next_ref_id.get();
                next_ref_id.set(n + 1);
                format!("e{n}")
            }
        };

        if let Some(backend_id) = child.backend_node_id {
            refs.insert(
                node_ref.clone(),
                ResolvedRef {
                    backend_node_id: BackendNodeId::new(backend_id),
                    role: child.role.clone().unwrap_or_default(),
                },
            );
        }

        out.push(AxNode {
            node_ref,
            role: child.role.clone().unwrap_or_default(),
            name: child.name.clone().unwrap_or_default(),
            value: child.value.clone(),
            children: grandchildren,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(
        id: &str,
        parent: Option<&str>,
        ignored: bool,
        role: Option<&str>,
        name: Option<&str>,
    ) -> RawAxNode {
        RawAxNode {
            node_id: id.to_string(),
            parent_id: parent.map(|p| p.to_string()),
            ignored,
            role: role.map(|r| r.to_string()),
            name: name.map(|n| n.to_string()),
            value: None,
            backend_node_id: Some(id.parse().unwrap_or(0)),
        }
    }

    // ---- Task 3.1.1 ----

    #[test]
    fn capture_snapshot_should_prune_ignored_node_and_assign_refs_when_axtree_has_hidden_sibling()
    {
        let next_ref_id = Cell::new(1);
        let nodes = vec![
            node("1", None, false, None, None),
            node("2", Some("1"), false, Some("button"), Some("Submit")),
            node("3", Some("1"), true, None, None),
        ];

        let capture = build_tree(&nodes, &next_ref_id, "https://example.com".into(), &HashMap::new());

        assert_eq!(capture.snapshot.root.children.len(), 1);
        let child = &capture.snapshot.root.children[0];
        assert_eq!(child.node_ref, "e1");
        assert_eq!(child.role, "button");
        assert_eq!(child.name, "Submit");
        assert_eq!(next_ref_id.get(), 2);
    }

    #[test]
    fn capture_snapshot_should_not_reuse_ref_strings_when_called_again_after_renavigation() {
        let next_ref_id = Cell::new(1);
        let first_nodes = vec![
            node("1", None, false, None, None),
            node("2", Some("1"), false, Some("button"), Some("Go")),
        ];
        let first = build_tree(&first_nodes, &next_ref_id, "https://example.com/a".into(), &HashMap::new());
        assert_eq!(first.snapshot.root.children[0].node_ref, "e1");
        assert_eq!(next_ref_id.get(), 2);

        // A different page entirely, same session-owned counter.
        let second_nodes = vec![
            node("10", None, false, None, None),
            node("11", Some("10"), false, Some("link"), Some("Home")),
        ];
        let second = build_tree(&second_nodes, &next_ref_id, "https://example.com/b".into(), &HashMap::new());

        assert_eq!(second.snapshot.root.children[0].node_ref, "e2");
        assert_ne!(second.snapshot.root.children[0].node_ref, "e1");
    }

    // ---- UX AC #6: truncation is legible ----

    #[test]
    fn ux_ac6_snapshot_should_set_truncated_true_when_node_count_exceeds_cap() {
        let next_ref_id = Cell::new(1);
        let mut nodes = vec![node("root", None, false, None, None)];
        for i in 0..(MAX_SNAPSHOT_NODES + 5) {
            nodes.push(node(
                &format!("c{i}"),
                Some("root"),
                false,
                Some("generic"),
                Some("x"),
            ));
        }

        let capture = build_tree(&nodes, &next_ref_id, "https://example.com".into(), &HashMap::new());

        assert!(capture.snapshot.truncated);
        assert_eq!(capture.snapshot.root.children.len(), MAX_SNAPSHOT_NODES);
    }

    // ---- UX AC #7: accessible role+name fidelity ----

    #[test]
    fn ux_ac7_button_with_no_explicit_role_attribute_should_surface_as_role_button() {
        let next_ref_id = Cell::new(1);
        // Chromium itself computes `role: "button"` for a plain `<button>`
        // with no ARIA attributes before this ever reaches `build_tree` —
        // this test locks down that we pass that computed role through
        // unmodified rather than mangling or defaulting it away.
        let nodes = vec![
            node("1", None, false, None, None),
            node("2", Some("1"), false, Some("button"), Some("Go")),
        ];

        let capture = build_tree(&nodes, &next_ref_id, "https://example.com".into(), &HashMap::new());

        assert_eq!(capture.snapshot.root.children[0].role, "button");
    }

    // ---- UX AC #8: hidden/non-interactive nodes pruned ----

    #[test]
    fn ux_ac8_aria_hidden_and_ignored_nodes_should_be_absent_from_snapshot() {
        let next_ref_id = Cell::new(1);
        let nodes = vec![
            node("1", None, false, None, None),
            // Both an `aria-hidden` node and a `display:none` node surface to
            // this layer identically: Chromium marks both `ignored: true` in
            // the AX tree itself (there is no separate CSS-visibility signal
            // available here — see the module doc comment on relying on
            // Chromium's own `ignored` flag).
            node("2", Some("1"), true, Some("generic"), Some("hidden-aria")),
            node("3", Some("1"), true, Some("generic"), Some("display-none")),
            node("4", Some("1"), false, Some("button"), Some("Visible")),
        ];

        let capture = build_tree(&nodes, &next_ref_id, "https://example.com".into(), &HashMap::new());

        assert_eq!(capture.snapshot.root.children.len(), 1);
        assert_eq!(capture.snapshot.root.children[0].name, "Visible");
    }

    #[test]
    fn build_tree_should_populate_refs_map_with_backend_node_id_and_role() {
        let next_ref_id = Cell::new(1);
        let nodes = vec![
            node("1", None, false, None, None),
            node("2", Some("1"), false, Some("textbox"), Some("Name")),
        ];

        let capture = build_tree(&nodes, &next_ref_id, "https://example.com".into(), &HashMap::new());

        let resolved = capture.refs.get("e1").expect("ref e1 should be resolvable");
        assert_eq!(resolved.role, "textbox");
        assert_eq!(*resolved.backend_node_id.inner(), 2);
    }

    // ---- ref identity survives a same-page recapture ----

    #[test]
    fn build_tree_should_reuse_ref_string_for_backend_node_id_seen_in_previous_capture() {
        // Mirrors what `click`/`type_text` do: dispatch a DOM mutation, then
        // re-capture the AX tree on the *same* page (no navigation). A node
        // that survives untouched (the textbox) must keep the exact ref
        // string a caller may already be holding from the first capture —
        // otherwise every ref becomes unusable the instant any other action
        // triggers a recapture, even though the page never navigated.
        let next_ref_id = Cell::new(1);
        let first_nodes = vec![
            node("1", None, false, None, None),
            node("2", Some("1"), false, Some("button"), Some("Go")),
            node("3", Some("1"), false, Some("textbox"), Some("Name")),
        ];
        let first = build_tree(
            &first_nodes,
            &next_ref_id,
            "https://example.com".into(),
            &HashMap::new(),
        );
        let name_ref = first
            .snapshot
            .root
            .children
            .iter()
            .find(|c| c.role == "textbox")
            .expect("textbox present")
            .node_ref
            .clone();

        // Same underlying nodes (ids "2"/"3" => same BackendNodeIds), as if
        // re-captured after clicking the button — nothing navigated.
        let second_nodes = vec![
            node("1", None, false, None, None),
            node("2", Some("1"), false, Some("button"), Some("Go")),
            node("3", Some("1"), false, Some("textbox"), Some("Name")),
        ];
        let second = build_tree(
            &second_nodes,
            &next_ref_id,
            "https://example.com".into(),
            &first.refs,
        );

        let second_name_ref = second
            .snapshot
            .root
            .children
            .iter()
            .find(|c| c.role == "textbox")
            .expect("textbox present")
            .node_ref
            .clone();

        assert_eq!(
            second_name_ref, name_ref,
            "same BackendNodeId across recaptures must keep the same ref string"
        );
        assert!(
            second.refs.contains_key(&name_ref),
            "reused ref string must still resolve in the new capture's refs map"
        );
    }
}
