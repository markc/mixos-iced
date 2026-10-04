//! The scene store: transactional `shell.scene.*` semantics.
//!
//! Its shape:
//! - the page registry and the dialog seat are the store's own (compd has no
//!   Quoin carousel to hold them), so a mount carries only output, owner and
//!   receipt;
//! - `dispatch` returns the topic wires to publish, in order, so the port
//!   publishes them and the ordering rule (a displaced owner hears before its
//!   successor) is a plain return value the tests read;
//! - a mounted scene is the renderer's handle plus the revision it applied.

use std::collections::BTreeMap;

use scene::{ResolvedScene, SceneDocument, Severity};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::mount::{dialog_geometry, is_dialog, page_id, scene_edge};
use crate::seat::{DialogError, DialogSeat, DialogSlot, Edge, PageRegistry, PageSeat};
use crate::templates::{PreparedLists, validate_templates};
use crate::verb::SceneVerb;

/// The command every scene change carries, whatever the host registered as.
pub const CHANGED_COMMAND: &str = "shell.scene.changed";

/// Every family `describe` answers for, in the contract's order.
pub const FAMILIES: [&str; 10] =
    ["window", "column", "row", "text", "field", "button", "toggle", "list", "image", "spacer"];

/// A renderer's live content for one scene: its surface handle and the
/// revision it last drew.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Mounted {
    pub handle: u64,
    pub revision: u64,
}

pub struct SceneEntry {
    pub(crate) document: SceneDocument,
    pub(crate) bindings: scene::bindings::BindingSet,
    pub(crate) tree: ResolvedScene,
    pub(crate) revision: u64,
    pub(crate) prepared: PreparedLists,
    pub(crate) render_error: Option<Value>,
    pub(crate) mounted: Option<Mounted>,
    /// `applied_revision`: the revision the renderer has taken. A drawn
    /// scene's is its surface's; a seated edge page that is not on screen
    /// (a hidden edge, an inactive page) takes each revision as it lands,
    /// because it will draw exactly that one when it shows. Quoin mounts
    /// those pages off screen and so reports them applied; leaving them at
    /// the last drawn revision made a hidden panel's saves never apply
    /// Kept across a hide, never reset to 0.
    pub(crate) applied: u64,
    pub(crate) owner: Option<SceneOwner>,
    /// A dialog is mapped only between `shell.dialog.show` and
    /// `shell.dialog.hide` (or its close button); an edge scene ignores it.
    pub(crate) visible: bool,
    // Set by a loader's JSON load envelope; zero fences a stopped behaviour.
    // Authored citizen metadata never grants model-write authority.
    pub(crate) model_generation: Option<u64>,
}

impl SceneEntry {
    pub fn tree(&self) -> &ResolvedScene {
        &self.tree
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn prepared(&self) -> &PreparedLists {
        &self.prepared
    }

    pub fn mounted(&self) -> Option<Mounted> {
        self.mounted
    }

    /// The revision reported as `applied_revision` (see the field).
    pub fn applied(&self) -> u64 {
        self.applied
    }

    pub fn visible(&self) -> bool {
        self.visible
    }

    pub fn owner(&self) -> Option<&str> {
        self.owner.as_ref().map(|owner| owner.citizen.as_str())
    }

    /// The reservation must match the loader, receipt, edge and host output.
    fn matching_seat<'a>(&self, pages: &'a PageRegistry, output: &str) -> Option<&'a PageSeat> {
        let owner = self.owner.as_ref()?;
        pages.seat(&page_id(&self.tree)).filter(|seat| {
            seat.owner == owner.citizen
                && seat.accepted_at == owner.accepted_at
                && seat.edge == scene_edge(&self.tree)
                && seat.output == output
        })
    }

    /// A dialog scene's reservation is the host's one dialog seat, held by
    /// this scene under the same loader receipt.
    fn holds_dialog_seat(&self, slot: &DialogSlot) -> bool {
        let Some(owner) = self.owner.as_ref() else {
            return false;
        };
        slot.seat().is_some_and(|seat| {
            seat.scene == self.tree.name && seat.owner == owner.citizen && seat.accepted_at == owner.accepted_at
        })
    }

    fn is_model_authority(&self, mount: Option<&SceneMount<'_>>) -> bool {
        self.owner.as_ref().zip(mount).is_some_and(|(owner, caller)| owner.citizen == caller.owner)
    }

    fn diagnostics(&self) -> Value {
        self.render_error.as_ref().map(|e| &e["diagnostics"]).cloned().unwrap_or_else(|| json!([]))
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SceneOwner {
    pub(crate) citizen: String,
    pub(crate) accepted_at: u64,
}

/// Host-supplied identity and seat, independent of authored scene metadata.
/// The caller derives `owner` from broker-attested request provenance.
pub struct SceneMount<'a> {
    pub output: &'a str,
    pub owner: &'a str,
    pub accepted_at: u64,
}

/// One answered request: the reply and the `<host>.scene.changed` wires to
/// publish, in order.
#[derive(Debug)]
pub struct Dispatched {
    pub rc: u8,
    pub body: String,
    pub publish: Vec<String>,
}

#[derive(Default)]
pub struct SceneStore {
    pub(crate) scenes: BTreeMap<String, SceneEntry>,
    pub(crate) removed: Vec<Mounted>,
    revisions: BTreeMap<String, u64>,
    pub(crate) pages: PageRegistry,
    /// The host's one dialog seat. A dialog-kind load takes it, never a page.
    pub(crate) dialog: DialogSlot,
    /// Notices for displaced or departed owners, published before the
    /// request's own summary.
    pub(crate) notices: Vec<Value>,
}

/// `---\ncommand: shell.scene.changed\n---\n<body>`.
pub fn changed_wire(body: &Value) -> String {
    format!("---\ncommand: {CHANGED_COMMAND}\n---\n{body}")
}

impl SceneStore {
    pub fn scene(&self, name: &str) -> Option<&SceneEntry> {
        self.scenes.get(name)
    }

    pub fn scenes(&self) -> impl Iterator<Item = (&str, &SceneEntry)> {
        self.scenes.iter().map(|(name, entry)| (name.as_str(), entry))
    }

    pub fn pages(&self) -> &PageRegistry {
        &self.pages
    }

    /// The dialog seat, if a dialog scene holds it.
    pub fn dialog_seat(&self) -> Option<&DialogSeat> {
        self.dialog.seat()
    }

    /// `Some(true)` for a loaded dialog scene, `Some(false)` for an edge
    /// scene, `None` when no scene of that name is loaded.
    pub fn is_dialog(&self, name: &str) -> Option<bool> {
        self.scenes.get(name).map(|entry| is_dialog(&entry.tree))
    }

    /// Page id and edge of a loaded edge scene; `None` for a dialog or an
    /// unknown scene.
    pub fn edge_page(&self, name: &str) -> Option<(String, Edge)> {
        self.scenes
            .get(name)
            .filter(|entry| !is_dialog(&entry.tree))
            .map(|entry| (page_id(&entry.tree), scene_edge(&entry.tree)))
    }

    /// Record what the renderer drew for `name`.
    pub fn set_mounted(&mut self, name: &str, mounted: Option<Mounted>) {
        if let Some(entry) = self.scenes.get_mut(name) {
            if let Some(mounted) = mounted {
                entry.applied = mounted.revision;
            }
            entry.mounted = mounted;
        }
    }

    /// Every edge scene seated on `output` (Quoin's mount condition) that
    /// `drawn` does not name has taken its current revision: nothing stale
    /// can show, since its surface is built from the store when it does.
    pub fn apply_offscreen(&mut self, output: &str, drawn: impl Fn(&str) -> bool) {
        let pages = &self.pages;
        for (name, entry) in self.scenes.iter_mut() {
            if !is_dialog(&entry.tree) && !drawn(name) && entry.matching_seat(pages, output).is_some() {
                entry.applied = entry.revision;
            }
        }
    }

    /// Record (or clear) a render failure; it is reported in `diagnostics`.
    pub fn set_render_error(&mut self, name: &str, error: Option<Value>) {
        if let Some(entry) = self.scenes.get_mut(name) {
            entry.render_error = error;
        }
    }

    /// Map or unmap a loaded dialog. False for an unknown or edge scene.
    pub fn set_dialog_visible(&mut self, name: &str, visible: bool) -> bool {
        match self.scenes.get_mut(name) {
            Some(entry) if is_dialog(&entry.tree) => {
                entry.visible = visible;
                true
            }
            _ => false,
        }
    }

    /// Mounted content whose scene left the store, for the renderer to
    /// destroy.
    pub fn take_removed(&mut self) -> Vec<Mounted> {
        std::mem::take(&mut self.removed)
    }

    /// Move the seated dialog to `output`. False when `scene` does not hold
    /// the seat.
    pub fn retarget_dialog(&mut self, scene: &str, output: &str) -> bool {
        let Some(mut seat) = self.dialog.seat().filter(|seat| seat.scene == scene).cloned() else {
            return false;
        };
        if seat.output == output {
            return true;
        }
        seat.output = output.to_owned();
        self.dialog.register(seat).is_ok()
    }

    /// `shell.scenes.list`: read-only inventory in scene-name order for the
    /// host output. `citizen` is authored routing metadata; `owner` is the
    /// verified loader. A dialog row adds `kind:"dialog"` and has no page
    /// and no edge.
    pub fn list(&self, output: &str) -> Value {
        Value::Array(
            self.scenes
                .iter()
                .map(|(name, entry)| {
                    let applied = entry.applied;
                    if is_dialog(&entry.tree) {
                        return json!({
                            "name": name,
                            "kind": "dialog",
                            "page": null,
                            "edge": null,
                            "citizen": entry.document.citizen,
                            "owner": entry.owner(),
                            "revision": entry.revision,
                            "applied_revision": applied,
                            "diagnostics": entry.diagnostics(),
                            "model_generation": entry.model_generation,
                            "digest": digest(&entry.tree),
                            "registered": entry.holds_dialog_seat(&self.dialog),
                        });
                    }
                    let seat = entry.matching_seat(&self.pages, output);
                    json!({
                        "name": name,
                        "page": page_id(&entry.tree),
                        "edge": seat.map(|seat| seat.edge.as_str()),
                        "citizen": entry.document.citizen,
                        "owner": entry.owner(),
                        "revision": entry.revision,
                        "applied_revision": applied,
                        "diagnostics": entry.diagnostics(),
                        "model_generation": entry.model_generation,
                        "digest": digest(&entry.tree),
                        "registered": seat.is_some(),
                    })
                })
                .collect(),
        )
    }

    /// Transactional Bus ingress: a rejected candidate never replaces
    /// last-good. `Layout` and `List` are the host's, not the store's.
    pub fn dispatch(&mut self, verb: SceneVerb, body: &str, args: &Value, mount: &mut SceneMount<'_>) -> Dispatched {
        let result = self.request_mounted(verb, body, args, Some(mount));
        // A pre-empted dialog holder hears before the new holder's own
        // summary, so an owner never sees its successor first.
        let mut publish = self.take_notices();
        match result {
            Ok((reply, summary)) => {
                if let Some(summary) = summary {
                    publish.push(changed_wire(&summary));
                }
                Dispatched { rc: 0, body: reply.to_string(), publish }
            }
            Err(error) => {
                let error = refusal(error);
                if matches!(verb, SceneVerb::Load | SceneVerb::Patch) {
                    let name = error["scene"].as_str().or_else(|| args["scene"].as_str());
                    let revision = name.and_then(|name| self.revisions.get(name)).copied().unwrap_or(0);
                    let diagnostics = error.get("diagnostics").cloned().unwrap_or_else(|| json!([error]));
                    let summary = json!({"scene":name,"revision":revision,"ops":0,"diagnostics":diagnostics});
                    publish.push(changed_wire(&summary));
                }
                Dispatched { rc: 10, body: error.to_string(), publish }
            }
        }
    }

    /// The pending notices as wires (owner departures outside a request).
    pub fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices).iter().map(changed_wire).collect()
    }

    #[cfg(test)]
    pub(crate) fn request(&mut self, verb: SceneVerb, body: &str, args: &Value) -> Result<(Value, Option<Value>), Value> {
        self.request_mounted(verb, body, args, None)
    }

    pub(crate) fn request_mounted(
        &mut self,
        verb: SceneVerb,
        body: &str,
        args: &Value,
        mount: Option<&mut SceneMount<'_>>,
    ) -> Result<(Value, Option<Value>), Value> {
        let name = args["scene"].as_str().unwrap_or_default();
        match verb {
            SceneVerb::Validate => {
                let document = scene::parse(body).map_err(|d| json!({"diagnostics":d}))?;
                check_size(&document)?;
                let diagnostics = scene::lint(&document);
                let tree = scene::resolve(&document).map_err(|d| json!({"diagnostics":d}))?;
                validate_templates(&tree)?;
                Ok((json!({"scene":tree.name,"valid":true,"diagnostics":diagnostics}), None))
            }
            SceneVerb::Load => {
                let managed_generation = if args.get("model_generation").is_some() {
                    Some(args["model_generation"].as_u64().filter(|g| *g <= 9_007_199_254_740_990).ok_or_else(
                        || json!({"error_code":"SCENE_MODEL_GENERATION", "message":"model_generation must be an exact nonnegative integer"}),
                    )?)
                } else {
                    None
                };
                let source = args["source"].as_str().unwrap_or(body);
                let document = scene::parse(source).map_err(|d| json!({"diagnostics":d}))?;
                // A pre-empting dialog load displaces whoever holds the name;
                // another owner's model fence must not turn that into a
                // refusal, or a squatter bricks safe mode.
                let preempting = args["preempt_dialog"].as_bool() == Some(true)
                    && mount.is_some()
                    && scene::resolve(&document).is_ok_and(|tree| is_dialog(&tree));
                if let Some(entry) = self.scenes.get(&document.name)
                    && entry.model_generation.is_some()
                    && (!preempting || entry.is_model_authority(mount.as_deref()))
                    && (managed_generation.is_none() || !entry.is_model_authority(mount.as_deref()))
                {
                    return Err(model_authority_refusal(&document.name));
                }
                if managed_generation.is_some() && mount.is_none() {
                    return Err(model_authority_refusal(&document.name));
                }
                let name = document.name.clone();
                // The loader's envelope flag: a dialog load that must win the
                // seat (the Scene Editor), so a squatter cannot brick it.
                let preempt = args["preempt_dialog"].as_bool() == Some(true);
                let result = self.accept(document, mount, true, preempt)?;
                if let Some(entry) = self.scenes.get_mut(&name) {
                    entry.model_generation = managed_generation;
                }
                Ok(result)
            }
            SceneVerb::Describe => {
                let value = if let Some(family) = args["family"].as_str() {
                    json!(scene::describe(family).ok_or_else(|| json!({"error":"unknown family"}))?)
                } else {
                    FAMILIES
                        .into_iter()
                        .map(|family| (family.to_owned(), json!(scene::describe(family).unwrap_or_default())))
                        .collect::<serde_json::Map<_, _>>()
                        .into()
                };
                Ok((value, None))
            }
            SceneVerb::Get | SceneVerb::Watch => {
                let entry = self.scenes.get(name).ok_or_else(|| json!({"error":"unknown scene"}))?;
                let value = if verb == SceneVerb::Watch {
                    json!({"scene":name,"revision":entry.revision,"digest":digest(&entry.tree),
                        "applied_revision":entry.applied,
                        "diagnostics":entry.diagnostics()})
                } else if args["format"] == "source" {
                    json!({"scene":name,"revision":entry.revision,"source":scene::to_source(&entry.document)})
                } else if let Some(path) = args["path"].as_str() {
                    let (id, port) = path.split_once('.').ok_or_else(|| json!({"error":"path must be node.port"}))?;
                    entry
                        .tree
                        .nodes
                        .get(id)
                        .and_then(|n| n.ports.get(port))
                        .cloned()
                        .ok_or_else(|| json!({"error":"unknown path"}))?
                } else {
                    json!(entry.tree)
                };
                Ok((value, None))
            }
            SceneVerb::Patch => {
                let entry = self.scenes.get(name).ok_or_else(|| json!({"error":"unknown scene"}))?;
                let mut document = entry.document.clone();
                let path = args["path"].as_str().unwrap_or_default();
                let value = args.get("value").ok_or_else(|| json!({"error":"value is required"}))?;
                if path == "model" || path.starts_with("model.") {
                    if let Some(generation) = entry.model_generation
                        && (!entry.is_model_authority(mount.as_deref())
                            || generation == 0
                            || args["generation"].as_u64() != Some(generation))
                    {
                        return Err(model_authority_refusal(name));
                    }
                    let result = scene::bindings::reevaluate(&entry.tree, &entry.bindings, path, value)
                        .map_err(|d| json!({"scene":name,"diagnostics":d}))?;
                    document.model = Some(result.tree.model.clone());
                    check_size(&document)?;
                    let prepared = validate_templates(&result.tree)?;
                    if page_id(&result.tree) != page_id(&entry.tree)
                        || scene_edge(&result.tree) != scene_edge(&entry.tree)
                        || is_dialog(&result.tree) != is_dialog(&entry.tree)
                        || (is_dialog(&entry.tree) && dialog_geometry(&result.tree) != dialog_geometry(&entry.tree))
                    {
                        return Err(json!({"scene":name,"error_code":"SUBPANEL_COLLISION",
                            "message":"model patch cannot move a scene mount; unload before moving it"}));
                    }
                    // Commit only after all validation. Keep the compiled
                    // bindings, the loader receipt and last-good ports.
                    let revision = self.revisions.entry(name.into()).or_default();
                    *revision += 1;
                    let revision = *revision;
                    let Some(entry) = self.scenes.get_mut(name) else {
                        return Err(json!({"error":"unknown scene"}));
                    };
                    entry.revision = revision;
                    entry.document = document;
                    entry.tree = result.tree;
                    entry.prepared = prepared;
                    entry.render_error = None;
                    let reply = json!({"scene":name,"revision":revision,"digest":digest(&entry.tree)});
                    let summary = json!({"scene":name,"revision":revision,"ops":result.changed.len(),"diagnostics":result.diagnostics});
                    return Ok((reply, Some(summary)));
                }
                let (id, port) = path.split_once('.').ok_or_else(|| json!({"error":"path must be node.port"}))?;
                let node = document.nodes.get_mut(id).ok_or_else(|| json!({"error":"unknown node"}))?;
                if !scene::describe(&node.widget)
                    .is_some_and(|ports| ports.iter().any(|description| description.path == port))
                {
                    return Err(json!({"error":"unknown port"}));
                }
                if value.is_null() {
                    node.ports.shift_remove(port);
                } else {
                    node.ports.insert(port.into(), value.clone());
                }
                check_size(&document)?;
                self.accept(document, mount, false, false)
            }
            SceneVerb::Unload => {
                let entry = self.scenes.remove(name).ok_or_else(|| json!({"error":"unknown scene"}))?;
                if is_dialog(&entry.tree) {
                    // Only this scene's own seat; a pre-empting successor's
                    // seat is not ours to free.
                    if entry.holds_dialog_seat(&self.dialog) {
                        let _ = self.dialog.release(name);
                    }
                } else if mount.is_some() {
                    self.pages.forget(&page_id(&entry.tree));
                }
                if let Some(mounted) = entry.mounted {
                    self.removed.push(mounted);
                }
                Ok((json!({"scene":name,"unloaded":true}), None))
            }
            SceneVerb::Layout
            | SceneVerb::List
            | SceneVerb::DialogShow
            | SceneVerb::DialogHide
            | SceneVerb::Ping
            | SceneVerb::Info
            | SceneVerb::PropsGet
            | SceneVerb::PanelState
            | SceneVerb::PanelMode
            | SceneVerb::PanelPageSet
            | SceneVerb::PanelOrder
            | SceneVerb::PanelResize
            | SceneVerb::MenuOpen
            | SceneVerb::MenuChoose
            | SceneVerb::MenuClose
            | SceneVerb::Panel { .. }
            | SceneVerb::NotServed => Err(json!({"error_code":"UNIMPLEMENTED",
                "message":"answered by the host, not the scene store"})),
        }
    }

    /// Names of the scenes currently owned by `citizen`.
    pub fn scenes_owned_by(&self, citizen: &str) -> Vec<String> {
        self.scenes
            .values()
            .filter(|entry| entry.owner.as_ref().is_some_and(|owner| owner.citizen == citizen))
            .map(|entry| entry.tree.name.clone())
            .collect()
    }

    /// Distinct local, registered owners: the ones a broker departure can
    /// name. Anonymous and mesh-qualified owners have no local lifetime.
    pub fn live_owners(&self) -> std::collections::BTreeSet<String> {
        self.scenes
            .values()
            .filter_map(|entry| entry.owner.as_ref())
            .filter(|owner| !owner.citizen.is_empty() && !owner.citizen.contains('@'))
            .map(|owner| owner.citizen.clone())
            .collect()
    }

    /// Unload every scene owned by `citizen`.
    pub fn unload_owned_by(&mut self, citizen: &str) -> Vec<String> {
        self.unload_owned_before(citizen, u64::MAX)
    }

    /// A departure only removes content accepted before its receipt. Every
    /// scene it drops gets a `{scene, revision, ops:["unloaded"],
    /// reason:"owner_departed", owner}` notice: an owner that is in fact back
    /// hears its scene is gone and remounts instead of trusting a mount that
    /// no longer exists.
    pub fn unload_owned_before(&mut self, citizen: &str, before: u64) -> Vec<String> {
        let names: Vec<String> = self
            .scenes
            .values()
            .filter(|entry| {
                entry.owner.as_ref().is_some_and(|owner| owner.citizen == citizen && owner.accepted_at < before)
            })
            .map(|entry| entry.tree.name.clone())
            .collect();
        for name in &names {
            if let Some(entry) = self.scenes.remove(name) {
                if entry.holds_dialog_seat(&self.dialog) {
                    let _ = self.dialog.release(name);
                } else if !is_dialog(&entry.tree) {
                    self.pages.forget(&page_id(&entry.tree));
                }
                if let Some(mounted) = entry.mounted {
                    self.removed.push(mounted);
                }
                let revision = self.revisions.get(name).copied().unwrap_or(0);
                self.notices.push(json!({
                    "scene": name,
                    "revision": revision,
                    "ops": ["unloaded"],
                    "reason": "owner_departed",
                    "owner": citizen,
                    "diagnostics": [],
                }));
            }
        }
        names
    }

    fn accept(
        &mut self,
        document: SceneDocument,
        mount: Option<&mut SceneMount<'_>>,
        loading: bool,
        preempt: bool,
    ) -> Result<(Value, Option<Value>), Value> {
        check_size(&document)?;
        let diagnostics = scene::lint(&document);
        if diagnostics.iter().any(|d| d.severity == Severity::Error) {
            return Err(json!({"scene":document.name,"diagnostics":diagnostics}));
        }
        let tree = scene::resolve(&document).map_err(|d| json!({"diagnostics":d}))?;
        let prepared = validate_templates(&tree)?;
        let bindings = scene::bindings::compile(&document).map_err(|d| json!({"diagnostics":d}))?;
        // A declared mount address is unique across scenes, including scenes
        // from the same citizen; a revision cannot rename a live seat. A
        // dialog takes no page, so it is outside the page-id namespace both
        // ways: an edge scene declaring `panel:"scene-editor"` cannot squat
        // the editor.
        let page = page_id(&tree);
        let dialog = is_dialog(&tree);
        if !dialog
            && self.scenes.iter().filter(|(_, entry)| !is_dialog(&entry.tree)).any(|(name, entry)| {
                (name != &tree.name && page_id(&entry.tree) == page)
                    || (name == &tree.name && page_id(&entry.tree) != page)
            })
        {
            return Err(json!({"scene": tree.name, "error_code": "SUBPANEL_COLLISION",
                "error": "scene mount address is occupied or changed; unload before renaming"}));
        }
        // A live scene cannot switch between dialog and edge. A pre-empting
        // dialog load displaces a same-name edge scene instead, so an edge
        // scene named `editor` cannot brick safe mode either.
        if self.scenes.get(&tree.name).is_some_and(|old| is_dialog(&old.tree) != dialog) {
            let Some(caller) = mount.as_deref().filter(|_| dialog && preempt) else {
                return Err(json!({"scene": tree.name, "error_code": "SUBPANEL_COLLISION",
                    "error": "a loaded scene cannot switch between dialog and edge; unload it first"}));
            };
            let by_owner = caller.owner.to_owned();
            if let Some(old) = self.scenes.remove(&tree.name) {
                self.pages.forget(&page_id(&old.tree));
                if let Some(mounted) = old.mounted {
                    self.removed.push(mounted);
                }
                self.notices.push(json!({
                    "scene": tree.name,
                    "revision": old.revision,
                    "ops": ["unloaded"],
                    "reason": "preempted",
                    "by": {"scene": tree.name, "owner": by_owner},
                    "diagnostics": [],
                }));
            }
        }
        let old_owner = self.scenes.get(&tree.name).and_then(|entry| entry.owner.clone());
        let owner = if let Some(mount) = mount {
            // Patching content is mesh-open but does not take ownership or
            // refresh another citizen's lifetime. Loads are owner
            // registrations.
            let fresh = || SceneOwner { citizen: mount.owner.to_owned(), accepted_at: mount.accepted_at };
            let owner = if loading { fresh() } else { old_owner.unwrap_or_else(fresh) };
            if dialog {
                self.seat_dialog(&tree, &owner, mount.output, preempt)?;
            } else {
                self.pages
                    .mount(&page_id(&tree), mount.output, scene_edge(&tree), &owner.citizen, owner.accepted_at)
                    .map_err(|error| {
                        json!({"scene":tree.name, "error_code":"SUBPANEL_COLLISION", "error":error.to_string()})
                    })?;
            }
            Some(owner)
        } else {
            old_owner
        };
        let ops = self.scenes.get(&tree.name).map_or(tree.nodes.len(), |old| scene::diff(&old.tree, &tree).len());
        let revision = self.revisions.entry(tree.name.clone()).or_default();
        *revision += 1;
        let revision = *revision;
        let reply = json!({"scene":tree.name,"revision":revision,"digest":digest(&tree)});
        let summary = json!({"scene":tree.name,"revision":revision,"ops":ops,"diagnostics":diagnostics});
        let old = self.scenes.remove(&tree.name);
        let model_generation = old.as_ref().and_then(|old| old.model_generation);
        let visible = old.as_ref().is_some_and(|old| old.visible);
        let applied = old.as_ref().map_or(0, |old| old.applied);
        let mounted = old.and_then(|old| old.mounted);
        self.scenes.insert(
            tree.name.clone(),
            SceneEntry {
                document,
                bindings,
                tree,
                revision,
                prepared,
                render_error: None,
                mounted,
                applied,
                owner,
                visible,
                model_generation,
            },
        );
        Ok((reply, Some(summary)))
    }

    /// Take (or keep) the dialog seat for `tree`. Refuses `DIALOG_BUSY` when
    /// another scene or owner holds it, unless the load pre-empts; then the
    /// displaced scene is unloaded here and its notice queued. Last in
    /// `accept`: nothing after it can refuse, so a refused load leaves the
    /// incumbent untouched.
    fn seat_dialog(&mut self, tree: &ResolvedScene, owner: &SceneOwner, output: &str, preempt: bool) -> Result<(), Value> {
        let (w, h, title, chrome) = dialog_geometry(tree);
        let seat = DialogSeat {
            scene: tree.name.clone(),
            owner: owner.citizen.clone(),
            accepted_at: owner.accepted_at,
            output: output.to_owned(),
            w,
            h,
            title,
            chrome,
        };
        if !preempt {
            return self.dialog.register(seat).map_err(|error| match error {
                DialogError::Busy { scene, owner } => json!({
                    "scene": tree.name, "error_code": "DIALOG_BUSY",
                    "message": "the dialog seat is held by another scene",
                    "holder": {"scene": scene, "owner": owner},
                }),
                other => json!({"scene": tree.name, "error_code": "SCENE_REFUSED", "message": other.to_string()}),
            });
        }
        let displaced = self.dialog.preempt(seat).map_err(|error| {
            json!({"scene": tree.name, "error_code": "SCENE_REFUSED", "message": error.to_string()})
        })?;
        if let Some(displaced) = displaced {
            // A different scene is unloaded outright; the same scene under a
            // new owner is replaced by this load's own commit.
            if displaced.scene != tree.name
                && let Some(entry) = self.scenes.remove(&displaced.scene)
                && let Some(mounted) = entry.mounted
            {
                self.removed.push(mounted);
            }
            let revision = self.revisions.get(&displaced.scene).copied().unwrap_or(0);
            self.notices.push(json!({
                "scene": displaced.scene,
                "revision": revision,
                "ops": ["unloaded"],
                "reason": "preempted",
                "by": {"scene": tree.name, "owner": owner.citizen},
                "diagnostics": [],
            }));
        }
        Ok(())
    }
}

fn model_authority_refusal(name: &str) -> Value {
    json!({"scene":name, "error_code":"SCENE_MODEL_AUTHORITY",
        "message":"managed model writes require the loader and its current generation; use scenes.model"})
}

/// SHA-256 of the resolved tree's JSON, lower-case hex.
pub fn digest(tree: &ResolvedScene) -> String {
    let bytes = serde_json::to_vec(tree).unwrap_or_default();
    format!("{:x}", Sha256::digest(bytes))
}

fn check_size(document: &SceneDocument) -> Result<(), Value> {
    if scene::to_source(document).len() > scene::MAX_DOCUMENT_BYTES {
        return Err(json!({"scene":document.name,"error_code":"DOCUMENT_TOO_LARGE",
            "message":"aggregate document and model exceeds 256 KiB",
            "diagnostics":[{"severity":"Error","code":"document-too-large","line":1,
                "message":"aggregate document and model exceeds 256 KiB"}]}));
    }
    Ok(())
}

/// Every refusal carries `error_code` (default `SCENE_REFUSED`) and
/// `message`.
pub fn refusal(mut error: Value) -> Value {
    if error.get("error_code").is_none() {
        error["error_code"] = json!("SCENE_REFUSED");
    }
    if error.get("message").is_none() {
        error["message"] = error.get("error").cloned().unwrap_or_else(|| json!("scene validation failed"));
    }
    error
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
