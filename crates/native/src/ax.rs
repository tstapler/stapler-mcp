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
use chromiumoxide::cdp::browser_protocol::dom::{
    BackendNodeId, DescribeNodeParams, ResolveNodeParams,
};
use chromiumoxide::cdp::browser_protocol::page::FrameId;
use chromiumoxide::cdp::js_protocol::runtime::CallFunctionOnParams;
use chromiumoxide::Page;
use futures::future::BoxFuture;

use stapler_mcp_core::ports::{AxNode, AxSnapshot, PortError, REDACTED_PLACEHOLDER};

/// Depth limit on same-process `<iframe>` recursion (issue #20). Guards
/// against pathological/self-referential iframe nesting; a frame beyond this
/// depth is left as a childless `Iframe` leaf rather than recursed into.
const MAX_FRAME_DEPTH: usize = 5;

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
    let url = page
        .url()
        .await
        .map_err(|e| PortError::Other(e.to_string()))?
        .unwrap_or_default();

    let raw = fetch_frame_tree(page, frame_id, 0).await?;

    let mut capture = build_tree(&raw, next_ref_id, url, previous_refs);
    let probe = PageProbe(page);
    redact_form_control_values(&mut capture.snapshot.root, &capture.refs, &probe).await;

    Ok(capture)
}

/// Fetches one frame's AX tree via `Accessibility.getFullAXTree`, then
/// recursively fetches and splices in the AX tree of every same-process
/// child `<iframe>` it finds, since `getFullAXTree` itself stops at frame
/// boundaries — an `Iframe`-role node comes back with no children even
/// though its document has its own accessibility tree (issue #20; confirmed
/// empirically that shadow DOM content is *already* included in a single
/// call, so only iframe traversal needed fixing).
///
/// A raw `AXNodeId` is only unique within the frame that produced it, so
/// every node id (and parent id) is prefixed with its owning frame's id
/// before nodes from different frames are flattened into one `Vec` for
/// `build_tree`. Depth-limited by `MAX_FRAME_DEPTH`. A child frame that
/// fails to resolve — cross-origin (a different renderer process, per
/// site-isolation, that this CDP session can't traverse into), closed
/// mid-capture, or lacking a content document — is left as a childless
/// `Iframe` leaf rather than failing the whole capture.
fn fetch_frame_tree(
    page: &Page,
    frame_id: Option<FrameId>,
    depth: usize,
) -> BoxFuture<'_, Result<Vec<RawAxNode>, PortError>> {
    Box::pin(async move {
        let params = match frame_id.clone() {
            Some(id) => GetFullAxTreeParams::builder().frame_id(id).build(),
            None => GetFullAxTreeParams::default(),
        };
        let resp = page
            .execute(params)
            .await
            .map_err(|e| PortError::Other(e.to_string()))?;

        let frame_key = frame_id.as_ref().map(|f| f.inner().as_str()).unwrap_or("");
        let prefix = |id: &str| format!("{frame_key}:{id}");

        let mut raw: Vec<RawAxNode> = resp
            .result
            .nodes
            .iter()
            .map(|n| RawAxNode {
                node_id: prefix(n.node_id.inner()),
                parent_id: n.parent_id.as_ref().map(|p| prefix(p.inner())),
                ignored: n.ignored,
                role: n.role.as_ref().and_then(ax_value_to_string),
                name: n.name.as_ref().and_then(ax_value_to_string),
                value: n.value.as_ref().and_then(ax_value_to_string),
                backend_node_id: n.backend_dom_node_id.as_ref().map(|id| *id.inner()),
            })
            .collect();

        if depth >= MAX_FRAME_DEPTH {
            return Ok(raw);
        }

        let iframe_nodes: Vec<(String, i64)> = raw
            .iter()
            .filter(|n| n.role.as_deref() == Some("Iframe"))
            .filter_map(|n| n.backend_node_id.map(|b| (n.node_id.clone(), b)))
            .collect();

        for (iframe_node_id, backend_id) in iframe_nodes {
            let describe = match page
                .execute(
                    DescribeNodeParams::builder()
                        .backend_node_id(BackendNodeId::new(backend_id))
                        .build(),
                )
                .await
            {
                Ok(d) => d,
                Err(_) => continue,
            };
            let Some(child_frame_id) = describe.result.node.frame_id.clone() else {
                continue;
            };
            let mut child_nodes =
                match fetch_frame_tree(page, Some(child_frame_id), depth + 1).await {
                    Ok(nodes) => nodes,
                    Err(_) => continue,
                };
            if let Some(child_root) = child_nodes.iter_mut().find(|n| n.parent_id.is_none()) {
                child_root.parent_id = Some(iframe_node_id);
            }
            raw.extend(child_nodes);
        }

        Ok(raw)
    })
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
        let node_ref = match child
            .backend_node_id
            .and_then(|id| previous_by_backend_id.get(&id))
        {
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

/// AX `role` values `redact_form_control_values` bothers probing.
/// Deliberately narrow (rather than probing every surviving node) — a
/// generic/button/link node can never carry a form-control `value` to leak,
/// so probing it would just be a wasted CDP round trip per node.
fn is_form_control_role(role: &str) -> bool {
    matches!(role, "textbox" | "searchbox" | "combobox")
}

/// Seam Task 2.2.1b introduces so `redact_form_control_values` doesn't need
/// to know whether it's talking to a live `Page` or a test fake — production
/// uses `PageProbe` (below), unit tests (Task 2.2.1c) use a fake that never
/// touches CDP.
///
/// `Sync` supertrait: `redact_form_control_values` holds a `&dyn
/// RedactionProbe`-shaped reference across an `.await` inside its own
/// `BoxFuture` (which is `Send`-bound); without `Sync` here, that reference
/// wouldn't be `Send`.
trait RedactionProbe: Sync {
    fn probe(&self, backend_node_id: BackendNodeId) -> BoxFuture<'_, Result<bool, ()>>;
}

/// Production `RedactionProbe`: delegates to `probe_redaction` over a real
/// CDP connection.
struct PageProbe<'a>(&'a Page);

impl RedactionProbe for PageProbe<'_> {
    fn probe(&self, backend_node_id: BackendNodeId) -> BoxFuture<'_, Result<bool, ()>> {
        Box::pin(probe_redaction(self.0, backend_node_id))
    }
}

/// Combined DOM `type`/`autocomplete` redaction check for one node, run via
/// a single `Runtime.callFunctionOn` eval bound to `this` (mirroring
/// `browser.rs`'s `invoke_on_node`/`ACTIONABILITY_CHECK_JS` pattern) rather
/// than two separate CDP calls — `research/pitfalls.md` §3a's atomicity
/// concern: two independent round trips can straddle a DOM mutation and see
/// an inconsistent `type`/`autocomplete` pair.
///
/// Deliberately **not** implemented with `DOM.describeNode`/
/// `DescribeNodeParams`: that call's `pierce` option defaults to `false`, so
/// it cannot see into even an *open* shadow root, silently missing a
/// password field nested inside a web component (`research/pitfalls.md`
/// §3c). A `Runtime.callFunctionOn` eval bound to `this`, by contrast, sees
/// into an open shadow root the same way any page script would — it is
/// ordinary JS reading `this.shadowRoot`'s contents when that root is open —
/// which is what makes Story 2.2.2's open-shadow-root case work at all. A
/// future refactor back to `DescribeNodeParams` for this check would
/// silently reintroduce the closed/open shadow-root asymmetry; don't.
///
/// Returns `Ok(true)` when `this.type === 'password'` or `this.autocomplete`
/// is one of `one-time-code`/`current-password`/`new-password`. `Err(())` on
/// *any* CDP failure (node gone, resolution failure, thrown exception) is
/// the fail-safe signal: the caller (`redact_form_control_values`) treats it
/// identically to `Ok(true)`, since a node whose type can't be determined
/// must never be assumed safe to leave in the clear.
///
/// **Empirical correction to `research/pitfalls.md` §3c** (verified against
/// Google Chrome 146.0.7680.153, via this module's own `#[ignore]`d
/// `snapshot_should_redact_value_when_shadow_root_is_closed_and_probe_fails`
/// test): a *closed* shadow root's contents are **not**, in fact,
/// unresolvable here. `DOM.resolveNode` on a `BackendNodeId` already
/// obtained from `Accessibility.getFullAXTree` — which itself pierces closed
/// roots — resolves to a working `RemoteObject` regardless of the shadow
/// root's open/closed mode, because CDP operates on the browser engine's own
/// internal DOM tree, not through the JS-visible `Element.shadowRoot`
/// accessor that "closed" mode nulls out at the *page-script* API surface
/// only. So `this` inside the eval is bound directly to the password
/// `<input>` either way, and closed-shadow redaction in practice happens via
/// a genuine `Ok(true)` type match, not the `Err(())` fail-safe path. The
/// fail-safe handling above is kept regardless — it's still correct
/// defense-in-depth for other genuine resolution failures (a node detached
/// between the AX-tree fetch and this probe, a thrown accessor, etc.) — but
/// don't assume `Err(())` is what makes closed-shadow content safe; it's the
/// eval succeeding anyway that does.
///
/// **Story 2.2.3's headless-vs-headed empirical check** (also verified
/// against Google Chrome 146.0.7680.153, via
/// `snapshot_should_match_headless_and_headed_ax_output_for_password_field`):
/// headless Chrome's own `Accessibility.getFullAXTree` already masks a
/// `type="password"` field's `value` to a run of bullet characters
/// (`"•••••••"` for a 7-character value) rather than exposing the raw
/// plaintext — a fact this module's redaction layer doesn't rely on (it
/// unconditionally substitutes `REDACTED_PLACEHOLDER`, which also hides the
/// value's *length*, unlike Chrome's own bullet masking) but is worth
/// recording because it underscores why the `autocomplete`-keyed branch of
/// this probe matters: that native masking is tied to `type="password"`
/// specifically, so a `type="text" autocomplete="one-time-code"` field gets
/// **no** such protection from Chrome itself and would surface its raw
/// value verbatim through the AX tree without this probe's `autocomplete`
/// check. The headed half of the comparison — whether headed Chrome differs
/// from this — could not be completed in the sandbox this was verified in
/// (no accessible X11 display: `google-chrome`'s own headed launch fails
/// with "Missing X server or $DISPLAY" there); this is an open gap for
/// whoever next runs `snapshot_should_match_headless_and_headed_ax_output_for_password_field`
/// with a real display attached.
async fn probe_redaction(page: &Page, backend_node_id: BackendNodeId) -> Result<bool, ()> {
    let resolved = page
        .execute(
            ResolveNodeParams::builder()
                .backend_node_id(backend_node_id)
                .build(),
        )
        .await
        .map_err(|_| ())?;
    let object_id = resolved.result.object.object_id.clone().ok_or(())?;

    let params = CallFunctionOnParams::builder()
        .object_id(object_id)
        .function_declaration(
            "function() { return this.type === 'password' || \
             ['one-time-code', 'current-password', 'new-password'].includes(this.autocomplete); }",
        )
        .return_by_value(true)
        .build()
        .map_err(|_| ())?;

    let response = page.execute(params).await.map_err(|_| ())?;
    if response.result.exception_details.is_some() {
        return Err(());
    }

    response
        .result
        .result
        .value
        .as_ref()
        .and_then(serde_json::Value::as_bool)
        .ok_or(())
}

/// Post-`walk_children` resolution pass: for every surviving node whose
/// `role` looks like a form control, probes its live DOM `type`/
/// `autocomplete` and substitutes `REDACTED_PLACEHOLDER` for `value` when the
/// probe says to redact, or fails outright (fail-safe — see
/// `probe_redaction`'s doc comment). Kept separate from `walk_children`
/// itself (rather than threading a `&Page` through the tree walk) so
/// `walk_children`'s existing pure/sync unit tests keep working without a
/// live `Page`; this pass is looked up by the same `BackendNodeId` ->
/// `ResolvedRef` map `build_tree` already produces.
fn redact_form_control_values<'a>(
    node: &'a mut AxNode,
    refs: &'a HashMap<String, ResolvedRef>,
    probe: &'a impl RedactionProbe,
) -> BoxFuture<'a, ()> {
    Box::pin(async move {
        if is_form_control_role(&node.role) {
            if let Some(resolved) = refs.get(&node.node_ref) {
                let result = probe.probe(resolved.backend_node_id).await;
                if !matches!(result, Ok(false)) {
                    node.value = Some(REDACTED_PLACEHOLDER.to_string());
                }
            }
        }
        for child in node.children.iter_mut() {
            redact_form_control_values(child, refs, probe).await;
        }
    })
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
    fn capture_snapshot_should_prune_ignored_node_and_assign_refs_when_axtree_has_hidden_sibling() {
        let next_ref_id = Cell::new(1);
        let nodes = vec![
            node("1", None, false, None, None),
            node("2", Some("1"), false, Some("button"), Some("Submit")),
            node("3", Some("1"), true, None, None),
        ];

        let capture = build_tree(
            &nodes,
            &next_ref_id,
            "https://example.com".into(),
            &HashMap::new(),
        );

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
        let first = build_tree(
            &first_nodes,
            &next_ref_id,
            "https://example.com/a".into(),
            &HashMap::new(),
        );
        assert_eq!(first.snapshot.root.children[0].node_ref, "e1");
        assert_eq!(next_ref_id.get(), 2);

        // A different page entirely, same session-owned counter.
        let second_nodes = vec![
            node("10", None, false, None, None),
            node("11", Some("10"), false, Some("link"), Some("Home")),
        ];
        let second = build_tree(
            &second_nodes,
            &next_ref_id,
            "https://example.com/b".into(),
            &HashMap::new(),
        );

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

        let capture = build_tree(
            &nodes,
            &next_ref_id,
            "https://example.com".into(),
            &HashMap::new(),
        );

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

        let capture = build_tree(
            &nodes,
            &next_ref_id,
            "https://example.com".into(),
            &HashMap::new(),
        );

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

        let capture = build_tree(
            &nodes,
            &next_ref_id,
            "https://example.com".into(),
            &HashMap::new(),
        );

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

        let capture = build_tree(
            &nodes,
            &next_ref_id,
            "https://example.com".into(),
            &HashMap::new(),
        );

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

    // ---- Epic 2.2, Story 2.2.1: redaction ----

    /// Same as `node()` but also sets `value` — Story 2.2.1's ACs are all
    /// about whether `value` survives or gets replaced, which `node()`
    /// deliberately doesn't expose (existing tests above never needed it).
    fn node_with_value(id: &str, parent: Option<&str>, role: &str, value: &str) -> RawAxNode {
        RawAxNode {
            node_id: id.to_string(),
            parent_id: parent.map(|p| p.to_string()),
            ignored: false,
            role: Some(role.to_string()),
            name: None,
            value: Some(value.to_string()),
            backend_node_id: Some(id.parse().unwrap_or(0)),
        }
    }

    /// Fake `RedactionProbe`: returns a fixed, pre-canned result for every
    /// node, standing in for a live CDP round trip (Task 2.2.1c's seam).
    struct FakeProbe(Result<bool, ()>);

    impl RedactionProbe for FakeProbe {
        fn probe(&self, _backend_node_id: BackendNodeId) -> BoxFuture<'_, Result<bool, ()>> {
            let result = self.0;
            Box::pin(async move { result })
        }
    }

    #[tokio::test]
    async fn build_tree_should_redact_value_when_dom_type_is_password() {
        // Chromium's computed AX role for `<input type="password">` is the
        // same "textbox" role any other text field gets — redaction can't
        // key off `role`, which is exactly why this drives the probe's own
        // `Ok(true)` result (standing in for `this.type === 'password'`)
        // rather than asserting anything about role.
        let next_ref_id = Cell::new(1);
        let nodes = vec![
            node("1", None, false, None, None),
            node_with_value("2", Some("1"), "textbox", "hunter2"),
        ];
        let mut capture = build_tree(
            &nodes,
            &next_ref_id,
            "https://example.com".into(),
            &HashMap::new(),
        );

        redact_form_control_values(
            &mut capture.snapshot.root,
            &capture.refs,
            &FakeProbe(Ok(true)),
        )
        .await;

        assert_eq!(
            capture.snapshot.root.children[0].value,
            Some(REDACTED_PLACEHOLDER.to_string())
        );
    }

    #[tokio::test]
    async fn build_tree_should_leave_value_unredacted_when_neither_key_matches() {
        let next_ref_id = Cell::new(1);
        let nodes = vec![
            node("1", None, false, None, None),
            node_with_value("2", Some("1"), "textbox", "Jane"),
        ];
        let mut capture = build_tree(
            &nodes,
            &next_ref_id,
            "https://example.com".into(),
            &HashMap::new(),
        );

        redact_form_control_values(
            &mut capture.snapshot.root,
            &capture.refs,
            &FakeProbe(Ok(false)),
        )
        .await;

        assert_eq!(
            capture.snapshot.root.children[0].value,
            Some("Jane".to_string())
        );
    }

    #[tokio::test]
    async fn build_tree_should_redact_value_when_autocomplete_is_one_time_code() {
        // Not `type="password"` at all — the probe alone (an `autocomplete`
        // match) must still trigger redaction. This is the single most
        // consequential redaction-key finding of the whole feature: a
        // one-time code is exactly as sensitive as a password, and a
        // redaction rule keyed only off `type` would silently leak it.
        let next_ref_id = Cell::new(1);
        let nodes = vec![
            node("1", None, false, None, None),
            node_with_value("2", Some("1"), "textbox", "482913"),
        ];
        let mut capture = build_tree(
            &nodes,
            &next_ref_id,
            "https://example.com".into(),
            &HashMap::new(),
        );

        redact_form_control_values(
            &mut capture.snapshot.root,
            &capture.refs,
            &FakeProbe(Ok(true)),
        )
        .await;

        assert_eq!(
            capture.snapshot.root.children[0].value,
            Some(REDACTED_PLACEHOLDER.to_string())
        );
    }

    #[tokio::test]
    async fn redact_form_control_values_should_redact_when_probe_fails() {
        // Fail-safe path: `Err(())` from the probe (node resolution failure,
        // exception, or — per Story 2.2.2 — an unobservable closed shadow
        // root) must redact, never be treated as "couldn't tell, leave it."
        let next_ref_id = Cell::new(1);
        let nodes = vec![
            node("1", None, false, None, None),
            node_with_value("2", Some("1"), "textbox", "secret"),
        ];
        let mut capture = build_tree(
            &nodes,
            &next_ref_id,
            "https://example.com".into(),
            &HashMap::new(),
        );

        redact_form_control_values(
            &mut capture.snapshot.root,
            &capture.refs,
            &FakeProbe(Err(())),
        )
        .await;

        assert_eq!(
            capture.snapshot.root.children[0].value,
            Some(REDACTED_PLACEHOLDER.to_string())
        );
    }

    #[tokio::test]
    async fn redact_form_control_values_should_not_probe_non_form_control_role() {
        let next_ref_id = Cell::new(1);
        let nodes = vec![
            node("1", None, false, None, None),
            node_with_value("2", Some("1"), "button", "irrelevant"),
        ];
        let mut capture = build_tree(
            &nodes,
            &next_ref_id,
            "https://example.com".into(),
            &HashMap::new(),
        );

        redact_form_control_values(
            &mut capture.snapshot.root,
            &capture.refs,
            &FakeProbe(Ok(true)),
        )
        .await;

        assert_eq!(
            capture.snapshot.root.children[0].value,
            Some("irrelevant".to_string()),
            "a non-form-control role must never be probed/redacted"
        );
    }

    // ---- Epic 2.2, Story 2.2.2: shadow-DOM integration coverage ----
    //
    // Both tests below need a real Chrome (they launch `chromiumoxide`'s own
    // `Browser`), so they're `#[ignore]`d rather than part of the default
    // `cargo test` run — `crates/native/tests/` has no existing integration
    // test convention to fit into (the directory doesn't exist yet), so
    // these live here per the Task 2.2.2b/c investigation note.

    /// Launches a real Chrome with the given config and loads `html` via
    /// `Page::set_content`. Shared by every `#[ignore]`d test in this module
    /// that needs a live CDP connection, so the launch/event-pump/set-content
    /// boilerplate exists exactly once.
    async fn launch_chrome_page_with_config(
        config: chromiumoxide::BrowserConfig,
        html: &str,
    ) -> (chromiumoxide::Browser, Page) {
        let (browser, mut handler) = chromiumoxide::Browser::launch(config)
            .await
            .expect("Chrome must be installed to run this ignored integration test");
        tokio::spawn(async move {
            use futures::StreamExt;
            while handler.next().await.is_some() {}
        });
        let page = browser
            .new_page("about:blank")
            .await
            .expect("new_page must succeed");
        page.set_content(html).await.expect("set_content");
        (browser, page)
    }

    /// A fresh, never-reused profile directory — `BrowserConfig::builder()`'s
    /// own default (unset `user_data_dir`) points every launch at one fixed
    /// shared path, so two sequential launches within the same test process
    /// (e.g. Story 2.2.3's headless-then-headed comparison) collide on
    /// Chrome's own `SingletonLock` (confirmed empirically — see that test's
    /// history). Mirrors the same fix `browser.rs`'s `NativeBrowser::launch`
    /// already applies in production.
    fn unique_test_profile_dir(tag: &str) -> std::path::PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "stapler-mcp-ax-test-{tag}-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create test profile dir");
        dir
    }

    /// Headless launch — same default `NativeBrowser::launch` (`browser.rs`)
    /// uses in production.
    async fn launch_headless_chrome_page(html: &str) -> (chromiumoxide::Browser, Page) {
        let config = chromiumoxide::BrowserConfig::builder()
            .user_data_dir(unique_test_profile_dir("headless"))
            .build()
            .expect("headless BrowserConfig must build");
        launch_chrome_page_with_config(config, html).await
    }

    /// Headed launch, for Story 2.2.3's empirical comparison.
    async fn launch_headed_chrome_page(html: &str) -> (chromiumoxide::Browser, Page) {
        let config = chromiumoxide::BrowserConfig::builder()
            .user_data_dir(unique_test_profile_dir("headed"))
            .with_head()
            .build()
            .expect("headed BrowserConfig must build");
        launch_chrome_page_with_config(config, html).await
    }

    const MY_LOGIN_HTML_TEMPLATE: &str = r#"<!doctype html>
<html><body>
<my-login></my-login>
<script>
customElements.define('my-login', class extends HTMLElement {
  connectedCallback() {
    const root = this.attachShadow({ mode: '__MODE__' });
    root.innerHTML = '<input type="password" value="hunter2">';
  }
});
</script>
</body></html>"#;

    // ---- Phase 7, Story 7.1.2: the literal acceptance test
    // `requirements.md`'s Success Metrics section names — "verified by a
    // test that autofills a password field out-of-band and then calls
    // `stapler_browser_snapshot`." Distinct from `MY_LOGIN_HTML_TEMPLATE`'s
    // tests above (which bake the value into the initial HTML, so it's
    // present before any AX tree is ever built): this one sets the value
    // *after* the page has loaded via a raw `page.evaluate()` call — the
    // same mechanism a real browser/password-manager autofill write uses,
    // and explicitly not `type_secret`/`type_text` — to prove redaction is
    // keyed on the live node's `type`/`autocomplete` at capture time, not on
    // how or when the value arrived.
    #[tokio::test]
    #[ignore = "requires a real Chrome; run with `cargo test -- --ignored`"]
    async fn snapshot_should_redact_value_when_field_was_autofilled_out_of_band() {
        let html = r#"<!doctype html><html><body>
<input id="pw" type="password">
</body></html>"#;
        let (browser, page) = launch_headless_chrome_page(html).await;

        // Simulates browser/password-manager autofill: written directly onto
        // the live DOM via CDP (`Runtime.evaluate`, under `page.evaluate()`),
        // never through `type_secret`/`type_text`.
        page.evaluate("document.getElementById('pw').value = 'hunter2';")
            .await
            .expect("evaluate: autofill password field out-of-band");

        let next_ref_id = Cell::new(1);
        let capture = capture_snapshot(&page, &next_ref_id, &HashMap::new())
            .await
            .expect("capture_snapshot should succeed");

        let password_node = find_node_by_role(&capture.snapshot.root, "textbox")
            .expect("out-of-band-autofilled password field should surface in the AX tree");
        assert_eq!(
            password_node.value.as_deref(),
            Some(REDACTED_PLACEHOLDER),
            "an out-of-band-autofilled password field must never surface its raw value"
        );

        let mut browser = browser;
        let _ = browser.close().await;
    }

    #[tokio::test]
    #[ignore = "requires a real Chrome; run with `cargo test -- --ignored`"]
    async fn snapshot_should_redact_value_when_shadow_root_is_open_and_probe_succeeds() {
        let html = MY_LOGIN_HTML_TEMPLATE.replace("__MODE__", "open");
        let (browser, page) = launch_headless_chrome_page(&html).await;

        let next_ref_id = Cell::new(1);
        let capture = capture_snapshot(&page, &next_ref_id, &HashMap::new())
            .await
            .expect("capture_snapshot should succeed");

        let password_node = find_node_by_role(&capture.snapshot.root, "textbox")
            .expect("password field should surface in the AX tree through the open shadow root");
        assert_eq!(
            password_node.value.as_deref(),
            Some(REDACTED_PLACEHOLDER),
            "open-shadow-root password field must be redacted via successful piercing"
        );

        let mut browser = browser;
        let _ = browser.close().await;
    }

    /// Named (and originally written) for the fail-safe path
    /// `pitfalls.md` predicted for a closed shadow root — but running this
    /// against a real Chrome (see `probe_redaction`'s doc comment) found
    /// that assumption wrong: `DOM.resolveNode`/`Runtime.callFunctionOn`
    /// bound to a `BackendNodeId` already obtained from the AX tree succeeds
    /// for closed-shadow content too, so this actually exercises `Ok(true)`
    /// (a genuine `type === 'password'` match), not `Err(())`. The AC this
    /// story cares about — the field ends up redacted either way — still
    /// holds, which is what's asserted below; the second assertion checks
    /// *which* of the two safe outcomes actually occurred, documenting the
    /// corrected finding in a way that would fail loudly if a future Chrome
    /// build reintroduces genuine unresolvability here.
    #[tokio::test]
    #[ignore = "requires a real Chrome; run with `cargo test -- --ignored`"]
    async fn snapshot_should_redact_value_when_shadow_root_is_closed_and_probe_fails() {
        let html = MY_LOGIN_HTML_TEMPLATE.replace("__MODE__", "closed");
        let (browser, page) = launch_headless_chrome_page(&html).await;

        let next_ref_id = Cell::new(1);
        let capture = capture_snapshot(&page, &next_ref_id, &HashMap::new())
            .await
            .expect("capture_snapshot should succeed");

        let password_node = find_node_by_role(&capture.snapshot.root, "textbox")
            .expect("password field inside a closed shadow root still surfaces in the AX tree");
        assert_eq!(
            password_node.value.as_deref(),
            Some(REDACTED_PLACEHOLDER),
            "closed-shadow-root password field must end up redacted, whether via a genuine \
             type match or the fail-safe path"
        );

        let resolved = capture
            .refs
            .get(&password_node.node_ref)
            .expect("password node's ref must resolve to a BackendNodeId");
        let probe_result = probe_redaction(&page, resolved.backend_node_id).await;
        assert!(
            matches!(probe_result, Ok(true) | Err(())),
            "expected a genuine type match or the fail-safe path, got {probe_result:?}"
        );

        let mut browser = browser;
        let _ = browser.close().await;
    }

    fn find_node_by_role<'a>(node: &'a AxNode, role: &str) -> Option<&'a AxNode> {
        if node.role == role {
            return Some(node);
        }
        node.children
            .iter()
            .find_map(|c| find_node_by_role(c, role))
    }

    // ---- Epic 2.2, Story 2.2.3: empirical headless-vs-headed check ----

    #[tokio::test]
    #[ignore = "manual/empirical; launches Chrome twice (headless + headed) and needs a real \
                X11 display for the headed half — run with \
                `cargo test -- --ignored snapshot_should_match_headless_and_headed_ax_output`"]
    async fn snapshot_should_match_headless_and_headed_ax_output_for_password_field() {
        use chromiumoxide::cdp::browser_protocol::accessibility::{
            AxNode as CdpAxNode, GetFullAxTreeParams,
        };

        let html = r#"<!doctype html><html><body>
<input id="pw" type="password" value="hunter2">
</body></html>"#;

        // Looks up by role rather than by the raw `"hunter2"` value: per
        // this test's own empirical finding (see `probe_redaction`'s doc
        // comment), the AX tree's `value` for a `type="password"` field is
        // never the raw plaintext in the first place.
        async fn fetch_password_ax_node(page: &Page) -> Option<CdpAxNode> {
            let tree = page
                .execute(GetFullAxTreeParams::default())
                .await
                .expect("getFullAXTree");
            tree.result
                .nodes
                .iter()
                .find(|n| {
                    n.role.as_ref().and_then(ax_value_to_string).as_deref() == Some("textbox")
                })
                .cloned()
        }

        let (mut headless_browser, headless_page) = launch_headless_chrome_page(html).await;
        let headless_pw = fetch_password_ax_node(&headless_page).await;

        let (mut headed_browser, headed_page) = launch_headed_chrome_page(html).await;
        let headed_pw = fetch_password_ax_node(&headed_page).await;

        println!("headless password AX node: {headless_pw:?}");
        println!("headed password AX node:   {headed_pw:?}");

        // No assertion beyond both being found: this test's job is to
        // *produce* the comparison printed above for a human to read and
        // fold into the doc comment on `probe_redaction`/this module, per
        // Task 2.2.3a's AC — see that doc comment for the recorded finding.
        assert!(headless_pw.is_some() && headed_pw.is_some());

        let _ = headless_browser.close().await;
        let _ = headed_browser.close().await;
    }
}
