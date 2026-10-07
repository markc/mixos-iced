//! The engine-side scene host: drains the port on compd's loop, answers each
//! request from the store, and sweeps departed owners.
//!
//! Runs on the compositor thread only. It does the caller-provenance check, the owner
//! attestation and the per-request receipt.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use serde_json::{Value, json};

use crate::port::{HostConfig, Inbound, Port, Request, Waker};
use crate::render::{Action, Surface, Wiring};
use crate::store::{SceneMount, SceneStore};
use crate::verb::SceneVerb;

/// What one `service` pass did.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Serviced {
    /// What is mounted or drawn may have changed: the renderer reconciles
    /// and the frame is scheduled (`RedrawReason::Publish`).
    pub changed: bool,
    /// Requests answered.
    pub answered: usize,
}

/// The host's request semantics, without a Bus: the store, the receipt
/// sequence and the departure sweep. [`SceneHost`] wraps it with the port.
#[derive(Default)]
pub struct Host {
    pub store: SceneStore,
    /// Quoin's panel model per output: which page each edge shows.
    pub panels: crate::panels::Panels,
    receipt: u64,
    live: Option<BTreeSet<String>>,
    /// The seated dialog's last drawn size `(scene, w, h)`: its envelope
    /// fitted to the usable zone (render.rs `dialog_rect`), which is what
    /// the dialog notice reports, as Quoin reports its seat after the clamp.
    dialog_fit: Option<(String, f32, f32)>,
    /// The conf.mix `shell.panel.order` writes (`None`: Quoin's path,
    /// [`crate::conf::conf_mix_path`]).
    pub conf_mix: Option<std::path::PathBuf>,
    menu_calls: Vec<crate::menu::Extra>,
}

impl Host {
    fn conf_mix(&self) -> std::path::PathBuf {
        self.conf_mix
            .clone()
            .unwrap_or_else(crate::conf::conf_mix_path)
    }

    /// Take conf.mix's declared page order (host start). A missing file
    /// declares nothing; an unreadable or invalid one is logged and ignored,
    /// as Quoin falls back to its defaults.
    pub fn load_declared(&mut self) {
        let path = self.conf_mix();
        self.panels.menu_conf = Some(path.clone());
        match crate::conf::read_declared(&path)
            .map_err(|error| error.to_string())
            .and_then(|declared| {
                self.panels
                    .declare(declared)
                    .map_err(|refusal| refusal.message)
            }) {
            Ok(()) => {}
            Err(error) => tracing::warn!(
                "scene host: conf.mix {} not applied: {error}",
                path.display()
            ),
        }
    }

    /// Model visibility precedes the render pass that creates the surface.
    /// In particular, a newly pinned edge is mapped at fraction zero, when
    /// render::targets still excludes it. Layout must wait for its surface.
    fn mapped_output(&self, scene: &str) -> Option<String> {
        let entry = self.store.scene(scene)?;
        let tree = entry.tree();
        if crate::mount::is_dialog(tree) {
            return self
                .store
                .dialog_seat()
                .filter(|seat| seat.scene == scene && entry.visible())
                .map(|seat| seat.output.clone());
        }
        let page = crate::mount::page_id(tree);
        let seat = self.store.pages().seat(&page)?;
        let panel = self
            .panels
            .state(&seat.output, crate::mount::scene_edge(tree));
        (panel["visible"] == true && panel["page"].as_str() == Some(page.as_str()))
            .then(|| seat.output.clone())
    }
}

/// Measures a loaded scene for `shell.scene.layout`: `(store, scene, node)`.
pub type Layout<'a> = dyn FnMut(&SceneStore, &str, Option<&str>) -> Result<Value, Value> + 'a;

/// One answered request: the reply and the topic wires, in order.
pub struct Answer {
    pub rc: u8,
    pub body: String,
    pub publish: Vec<String>,
    pub changed: bool,
}

impl Host {
    /// Answer one request for the host output `output`. `live_generation` is
    /// the port's current connection generation (`None` with no port): a
    /// request admitted on an earlier connection is refused unapplied, so a
    /// caller that saw its connection drop and retried never gets its load or
    /// patch applied twice. `layout` measures a loaded scene for
    /// `shell.scene.layout` (the renderer's; `Err` is a refusal body).
    pub fn answer(
        &mut self,
        request: &Request,
        output: &str,
        live_generation: Option<u64>,
        layout: &mut Layout<'_>,
    ) -> Answer {
        if let Err(error) = verify_caller_provenance(request) {
            let body = json!({"error_code":"SCENE_PROVENANCE", "message":format!("scene caller provenance: {error}")});
            return Answer {
                rc: 10,
                body: body.to_string(),
                publish: Vec::new(),
                changed: false,
            };
        }
        if live_generation.is_some_and(|generation| generation != request.generation) {
            let body = json!({"error_code":"SCENE_STALE_CONNECTION",
                "message":"scene request belongs to a stale scene host connection"});
            return Answer {
                rc: 10,
                body: body.to_string(),
                publish: Vec::new(),
                changed: false,
            };
        }
        self.receipt = self
            .receipt
            .checked_add(1)
            .expect("receipt sequence exhausted");
        let args: Value = serde_json::from_str(&request.body).unwrap_or(Value::Null);
        match request.verb {
            SceneVerb::List => Answer {
                rc: 0,
                body: self.store.list(output).to_string(),
                publish: Vec::new(),
                changed: false,
            },
            SceneVerb::Layout => {
                let scene = argument(request, &args, "scene").unwrap_or_default();
                let (rc, body) = if self.store.scene(scene).is_none() {
                    (
                        10,
                        json!({"error_code":"NOT_FOUND", "message":format!("no scene named {scene} is loaded"), "scene":scene}),
                    )
                } else {
                    match layout(&self.store, scene, argument(request, &args, "node")) {
                        Ok(body) => (0, body),
                        Err(body) => (10, body),
                    }
                };
                Answer {
                    rc,
                    body: body.to_string(),
                    publish: Vec::new(),
                    changed: false,
                }
            }
            SceneVerb::NotServed => {
                let body = json!({"error_code":"not_served",
                    "message":format!("{} is not served by compd's scene host yet", request.command)});
                Answer {
                    rc: 10,
                    body: body.to_string(),
                    publish: Vec::new(),
                    changed: false,
                }
            }
            SceneVerb::Ping => Answer {
                rc: 0,
                body: json!({"service":"shell", "status":"ok"}).to_string(),
                publish: Vec::new(),
                changed: false,
            },
            SceneVerb::Info => {
                // Only what this host serves, bare (`dialog.show`), so a loader's
                // feature check reads the truth.
                let verbs: Vec<String> = SceneVerb::names("shell")
                    .into_iter()
                    .map(|name| name.trim_start_matches("shell.").to_owned())
                    .collect();
                let body = json!({"service":"shell", "contract":"shell.v1", "props":["get"], "verbs":verbs});
                Answer {
                    rc: 0,
                    body: body.to_string(),
                    publish: Vec::new(),
                    changed: false,
                }
            }
            SceneVerb::PropsGet => {
                let snapshot = self
                    .panels
                    .snapshot(output, dialog_notice(&self.store, self.dialog_fit.as_ref()));
                let path = argument(request, &args, "path").filter(|path| !path.is_empty());
                let found = match path {
                    None => Some(snapshot),
                    Some(path) => path
                        .split('.')
                        .try_fold(snapshot, |node, key| node.get(key).cloned()),
                };
                match found {
                    Some(value) => Answer {
                        rc: 0,
                        body: value.to_string(),
                        publish: Vec::new(),
                        changed: false,
                    },
                    None => {
                        let body = json!({"error_code":"UNKNOWN_PATH", "message":format!("no shell prop at {}", path.unwrap_or_default())});
                        Answer {
                            rc: 10,
                            body: body.to_string(),
                            publish: Vec::new(),
                            changed: false,
                        }
                    }
                }
            }
            SceneVerb::MenuOpen | SceneVerb::MenuChoose | SceneVerb::MenuClose => {
                let refused = |code: &str, message: &str| Answer {
                    rc: 10,
                    body: json!({"error_code":code, "message":message}).to_string(),
                    publish: Vec::new(),
                    changed: false,
                };
                let result = if request.verb == SceneVerb::MenuOpen {
                    let Some(corner) = argument(request, &args, "corner").and_then(|name| {
                        edges::Corner::ALL
                            .into_iter()
                            .find(|corner| crate::menu::corner_name(*corner) == name)
                    }) else {
                        return refused(
                            "BAD_ARGUMENT",
                            "corner must be top-left, bottom-left, bottom-right or top-right",
                        );
                    };
                    self.panels.menu_conf = Some(self.conf_mix());
                    self.panels.open_menu(output, corner).map(|_| None)
                } else {
                    let Some(serial) = args.get("serial").and_then(Value::as_u64) else {
                        return refused(
                            "BAD_ARGUMENT",
                            "menu serial is required (read props.get menu.serial)",
                        );
                    };
                    let input = if request.verb == SceneVerb::MenuClose {
                        crate::menu::Input::Close
                    } else {
                        let Some(index) = args
                            .get("index")
                            .and_then(Value::as_u64)
                            .and_then(|i| usize::try_from(i).ok())
                        else {
                            return refused(
                                "BAD_ARGUMENT",
                                "menu choice requires an integer item index",
                            );
                        };
                        crate::menu::Input::Choose(index)
                    };
                    self.panels.menu_input(serial, input)
                };
                match result {
                    Ok(extra) => {
                        self.menu_calls.extend(extra);
                        Answer { rc: 0, body: json!({"accepted":true, "menu":self.panels.menu().map(crate::menu::Menu::snapshot)}).to_string(),
                            publish: Vec::new(), changed: true }
                    }
                    Err(error) => refused(error.code, &error.message),
                }
            }
            SceneVerb::PanelState | SceneVerb::PanelMode | SceneVerb::PanelPageSet => {
                let refused = |code: &str, message: &str, panels: Option<&Value>| {
                    let mut body = json!({"error_code":code, "message":message});
                    if let Some(panels) = panels {
                        body["panels"] = panels.clone();
                    }
                    Answer {
                        rc: 10,
                        body: body.to_string(),
                        publish: Vec::new(),
                        changed: false,
                    }
                };
                let Some(edge) =
                    argument(request, &args, "edge").and_then(crate::panels::parse_edge)
                else {
                    return refused(
                        "BAD_ARGUMENT",
                        "edge must be left, bottom, right or top",
                        None,
                    );
                };
                if request.verb == SceneVerb::PanelState {
                    return Answer {
                        rc: 0,
                        body: self.panels.state(output, edge).to_string(),
                        publish: Vec::new(),
                        changed: false,
                    };
                }
                let applied = if request.verb == SceneVerb::PanelMode {
                    let Some(mode) = argument(request, &args, "mode") else {
                        return refused(
                            "BAD_ARGUMENT",
                            "panel.mode requires a mode argument",
                            None,
                        );
                    };
                    self.panels.set_mode(output, edge, mode)
                } else {
                    let Some(id) = argument(request, &args, "id") else {
                        return refused("BAD_ARGUMENT", "page.set requires an id argument", None);
                    };
                    self.panels.page_set(output, edge, id)
                };
                let panels = self.panels.snapshot(output, Value::Null)["panels"].clone();
                match applied {
                    Ok(()) => {
                        let body = json!({"accepted":true, "applied":true, "panels":panels});
                        Answer {
                            rc: 0,
                            body: body.to_string(),
                            publish: Vec::new(),
                            changed: true,
                        }
                    }
                    Err(refusal) => refused(refusal.code, &refusal.message, Some(&panels)),
                }
            }
            SceneVerb::PanelOrder => {
                // Frozen in shell-verbs.json: `{edges}` as written, or
                // INVALID_ARGUMENT / CONFIG_WRITE. One atomic conf.mix write,
                // then every carousel takes the merged order.
                let refused = |body: Value| Answer {
                    rc: 10,
                    body: body.to_string(),
                    publish: Vec::new(),
                    changed: false,
                };
                let order = match crate::conf::parse_order(&args) {
                    Ok(order) => order,
                    Err(refusal) => return refused(refusal.body()),
                };
                let declared = match crate::conf::write_order(&self.conf_mix(), &order) {
                    Ok(declared) => declared,
                    Err(refusal) => return refused(refusal.body()),
                };
                if let Err(refusal) = self.panels.declare(declared) {
                    return refused(json!({"error_code":refusal.code, "message":refusal.message}));
                }
                let edges: serde_json::Map<String, Value> = order
                    .into_iter()
                    .map(|(edge, pages)| (crate::conf::edge_name(edge).to_owned(), json!(pages)))
                    .collect();
                Answer {
                    rc: 0,
                    body: json!({"edges": edges}).to_string(),
                    publish: Vec::new(),
                    changed: true,
                }
            }
            SceneVerb::PanelResize => {
                let refused = |body: Value| Answer {
                    rc: 10,
                    body: body.to_string(),
                    publish: Vec::new(),
                    changed: false,
                };
                let Some(edge) =
                    argument(request, &args, "edge").and_then(crate::panels::parse_edge)
                else {
                    return refused(json!({"error":"edge must be left, bottom, right or top"}));
                };
                let Some(thickness_px) = number_argument(&args, "thickness_px") else {
                    return refused(crate::panels::resize_range_refusal(edge));
                };
                match self.panels.resize(output, edge, thickness_px as f32) {
                    Ok(()) => Answer {
                        rc: 0,
                        body: json!({"accepted":true}).to_string(),
                        publish: Vec::new(),
                        changed: true,
                    },
                    Err(body) => refused(body),
                }
            }
            SceneVerb::Panel { verb, corner } => {
                // Quoin: corner.hide uses the unified refusal shape, the
                // older verbs `{error}`.
                let unified = corner && verb == crate::panels::PanelVerb::Hide;
                let refused = |code: &str, message: &str, extra: Option<(&str, Value)>| {
                    let mut body = if unified || code != "BAD_ARGUMENT" {
                        json!({"error_code":code, "message":message})
                    } else {
                        json!({"error":message})
                    };
                    if let Some((key, value)) = extra {
                        body[key] = value;
                    }
                    Answer {
                        rc: 10,
                        body: body.to_string(),
                        publish: Vec::new(),
                        changed: false,
                    }
                };
                let edge = if corner {
                    match argument(request, &args, "corner").and_then(crate::panels::corner_edge) {
                        Some(edge) => edge,
                        None => {
                            let code = if unified {
                                "INVALID_ARGUMENT"
                            } else {
                                "BAD_ARGUMENT"
                            };
                            return refused(
                                code,
                                "corner must be top-left, bottom-left, bottom-right or top-right",
                                None,
                            );
                        }
                    }
                } else {
                    match argument(request, &args, "edge").and_then(crate::panels::parse_edge) {
                        Some(edge) => edge,
                        None => {
                            return refused(
                                "BAD_ARGUMENT",
                                "edge must be left, bottom, right or top",
                                None,
                            );
                        }
                    }
                };
                if let Err(refusal) = self.panels.input(output, edge, verb) {
                    return refused(
                        refusal.code,
                        &refusal.message,
                        Some(("edge", json!(edge.as_str()))),
                    );
                }
                let applied_pin = !corner && verb == crate::panels::PanelVerb::Pin;
                if verb != crate::panels::PanelVerb::Hide && !applied_pin {
                    // Quoin acknowledges these on acceptance; the state is
                    // read back from props.get / panel.changed.
                    return Answer {
                        rc: 0,
                        body: json!({"accepted":true}).to_string(),
                        publish: Vec::new(),
                        changed: true,
                    };
                }
                let panels = self.panels.snapshot(output, Value::Null)["panels"].clone();
                if (applied_pin
                    && self.panels.mode(output, edge) == "pinned"
                    && panels[edge.as_str()]["visible"] == true)
                    || (!applied_pin && self.panels.hidden(output, edge))
                {
                    let body = json!({"accepted":true, "applied":true, "panels":panels});
                    Answer {
                        rc: 0,
                        body: body.to_string(),
                        publish: Vec::new(),
                        changed: true,
                    }
                } else if applied_pin {
                    let mut answer = refused(
                        "PANEL_NOT_APPLIED",
                        "panel command was superseded or could not apply",
                        Some(("panels", panels)),
                    );
                    answer.changed = true;
                    answer
                } else {
                    // A pinned or docked edge is a persistent mode, which hide
                    // never changes: say so rather than accept and do nothing.
                    let name = edge.as_str();
                    let mode = self.panels.mode(output, edge);
                    let message = format!(
                        "the {name} edge is {mode}; {} only conceals a transient reveal. Use shell.panel.mode {{edge:\"{name}\", mode:\"hidden\"}} to hide it",
                        request.command
                    );
                    let mut answer =
                        refused("PANEL_NOT_APPLIED", &message, Some(("panels", panels)));
                    answer.changed = true;
                    answer
                }
            }
            SceneVerb::DialogShow | SceneVerb::DialogHide => {
                // Frozen in shell-verbs.json: `{scene, visible, applied}`, or
                // NOT_FOUND / NOT_DIALOG.
                let scene = argument(request, &args, "scene").unwrap_or_default();
                let visible = request.verb == SceneVerb::DialogShow;
                let refused = |body: Value| Answer {
                    rc: 10,
                    body: body.to_string(),
                    publish: Vec::new(),
                    changed: false,
                };
                match self.store.is_dialog(scene) {
                    None => refused(json!({"error_code":"NOT_FOUND",
                        "message":format!("no scene named {scene} is loaded"), "scene":scene})),
                    Some(false) => refused(json!({"error_code":"NOT_DIALOG",
                        "message":format!("scene {scene} is an edge page, not a dialog"), "scene":scene})),
                    Some(true) => {
                        // Shown on the selected output, as Quoin maps it.
                        if visible {
                            self.panels.close_menu();
                            self.store.retarget_dialog(scene, output);
                        }
                        self.store.set_dialog_visible(scene, visible);
                        let body = json!({"scene":scene, "visible":visible, "applied":true});
                        Answer {
                            rc: 0,
                            body: body.to_string(),
                            publish: Vec::new(),
                            changed: true,
                        }
                    }
                }
            }
            verb => {
                let owner = attested_owner(request, self.receipt);
                let mut mount = SceneMount {
                    output,
                    owner: &owner,
                    accepted_at: self.receipt,
                };
                let done = self.store.dispatch(verb, &request.body, &args, &mut mount);
                let changed = done.rc == 0 && verb.mutates();
                Answer {
                    rc: done.rc,
                    body: done.body,
                    publish: done.publish,
                    changed,
                }
            }
        }
    }

    /// A registry diff: every local owner that was registered and no longer
    /// is loses the scenes it loaded before now. The first diff only sets
    /// the baseline, so a citizen never seen live is never swept.
    /// Returns the notices to publish.
    pub fn services_live(&mut self, live: BTreeSet<String>) -> Vec<String> {
        let previous = self.live.replace(live);
        let Some(previous) = previous else {
            return Vec::new();
        };
        let current = self.live.as_ref().expect("just set");
        let before = self.receipt.saturating_add(1);
        for owner in self.store.live_owners() {
            if previous.contains(&owner) && !current.contains(&owner) {
                let scenes = self.store.unload_owned_before(&owner, before);
                if !scenes.is_empty() {
                    tracing::info!(
                        "scene host: {owner} left the Bus; unloaded {}",
                        scenes.join(",")
                    );
                }
            }
        }
        self.store.take_notices()
    }
}

/// The dialog seat as Quoin's `dialog_bus::notice`: `{scene, visible, w, h,
/// output}`, or null with no dialog loaded. `w`/`h` are the drawn size when
/// `fit` names this scene (fitted to the usable zone), else the seat's.
pub fn dialog_notice(store: &SceneStore, fit: Option<&(String, f32, f32)>) -> Value {
    let Some(seat) = store.dialog_seat() else {
        return Value::Null;
    };
    let visible = store
        .scene(&seat.scene)
        .is_some_and(|entry| entry.visible());
    let (w, h) = match fit {
        Some((scene, w, h)) if *scene == seat.scene => (json!(w), json!(h)),
        _ => (json!(seat.w), json!(seat.h)),
    };
    json!({"scene":seat.scene, "visible":visible, "w":w, "h":h, "output":seat.output})
}

/// The engine's scene host: [`Host`] plus its Bus port and its surfaces.
pub struct SceneHost {
    pub host: Host,
    port: Port,
    service: Option<String>,
    refused: Option<String>,
    surfaces: BTreeMap<String, Surface>,
    /// Layout requests for mapped scenes whose first render is still due.
    /// Keep the admitted request intact for provenance/reconnect checks.
    pending_layouts: Vec<Request>,
    /// Renderer identity -> scene/Quoin connector name. Keep EDID identity
    /// at the compositor boundary; the shell model and wire use connectors.
    output_names: BTreeMap<String, String>,
    /// Each scene's last drawn rect (logical px on its output).
    placed: BTreeMap<String, (f32, f32, f32, f32)>,
    wiring: Wiring,
    actions: Receiver<Action>,
    menu_surface: Option<crate::menu::Surface>,
    settings: application::presentation::native::Session<crate::appearance::Look>,
    appearance_generation: u64,
}

impl SceneHost {
    /// Start the port (only when the `scene_host` preference
    /// is on; the caller decides).
    pub fn start(config: HostConfig, waker: Waker) -> Result<Self, String> {
        let port = Port::start(config, Arc::clone(&waker))?;
        let consumer = settings::consumer::Consumer::for_shell(port.settings_binding())
            .map_err(|error| format!("shell settings: {error:?}"))?;
        let mut settings = application::presentation::native::Session::new(consumer);
        let (_, jobs) = settings.handle(
            application::presentation::native::Event::Wake,
            port.settings_generation(),
        );
        port.settings_jobs(jobs);
        let (sender, actions) = std::sync::mpsc::channel();
        let wiring = Wiring {
            sink: port.event_sink(),
            actions: sender,
            waker,
        };
        // conf.mix's page order, as Quoin reads it at start.
        let mut host = Host::default();
        host.panels.load_state();
        host.load_declared();
        Ok(Self {
            host,
            port,
            service: None,
            refused: None,
            surfaces: BTreeMap::new(),
            pending_layouts: Vec::new(),
            output_names: BTreeMap::new(),
            placed: BTreeMap::new(),
            wiring,
            actions,
            menu_surface: None,
            settings,
            appearance_generation: 0,
        })
    }

    /// The name the host registered as, once it has.
    pub fn service(&self) -> Option<&str> {
        self.service.as_deref()
    }

    /// Why the host is off (the broker refused the name), if it is.
    pub fn refused(&self) -> Option<&str> {
        self.refused.as_deref()
    }

    pub fn store(&self) -> &SceneStore {
        &self.host.store
    }

    pub fn store_mut(&mut self) -> &mut SceneStore {
        &mut self.host.store
    }

    pub(crate) fn output_name<'a>(&'a self, key: &'a str) -> &'a str {
        self.output_names.get(key).map_or(key, String::as_str)
    }

    pub(crate) fn ensure_output(&mut self, key: String, name: &str, logical: (f32, f32)) {
        self.output_names.insert(key, name.to_owned());
        self.host.panels.ensure(name, logical);
        self.host.panels.sync(&self.host.store);
    }

    /// Send a scene event to a document's citizen (see [`Port::emit`]).
    pub fn emit(&self, citizen: &str, verb: &str, body: Value) -> bool {
        self.port.emit(citizen, verb, body)
    }

    /// Drain everything the worker and the surfaces delivered. Called from
    /// the loop's post-dispatch hook after the waker fired.
    pub fn service_port(&mut self, lp: &mut world::state::Loop) -> Serviced {
        let output = lp.inner.active_output().clone();
        let name = output.name();
        let output = name.as_str();
        let mut serviced = Serviced::default();
        for event in self.port.take_settings() {
            let (changed, jobs) = self.settings.handle(event, self.port.settings_generation());
            self.port.settings_jobs(jobs);
            if changed.is_some() {
                let look = self
                    .settings
                    .host()
                    .presentation()
                    .expect("activated presentation")
                    .content();
                let style = decor::window::installed()
                    .map_or(decor::ChromeStyle::Mac, |theme| theme.deco.style);
                decor::window::install(look.chrome(style));
                self.appearance_generation = self.appearance_generation.wrapping_add(1);
                serviced.changed = true;
            }
        }
        // The panel model for this output, at its logical size.
        {
            let monitor = lp.inner.active_output();
            if let Some(mode) = monitor.current_mode() {
                let size = monitor.current_transform().transform_size(mode.size);
                let scale = monitor.current_scale().fractional_scale().max(0.1);
                self.ensure_output(
                    lp.inner.active_output_key(),
                    output,
                    (size.w as f32 / scale as f32, size.h as f32 / scale as f32),
                );
            }
        }
        while let Ok(action) = self.actions.try_recv() {
            match action {
                Action::HideDialog(scene) => {
                    serviced.changed |= self.host.store.set_dialog_visible(&scene, false)
                }
                Action::EscapeEdge(scene) => {
                    if let Some(surface) = self.surfaces.get(&scene)
                        && lp
                            .inner
                            .surface()
                            .registry
                            .as_ref()
                            .and_then(|r| r.keyboard_focus())
                            == Some(surface.handle())
                        && let Some(entry) = self.host.store.scene(&scene)
                    {
                        self.host
                            .panels
                            .escape(surface.output(), crate::mount::scene_edge(entry.tree()));
                        crate::render::release_edge_keyboard(lp, &scene);
                        // Click-focused fields have no autofocus prior to
                        // restore, but still give up their registry focus.
                        if let Some(registry) = lp.inner.surface_mut().registry.as_mut()
                            && registry.keyboard_focus() == Some(surface.handle())
                        {
                            registry.set_keyboard_focus(None);
                        }
                        serviced.changed = true;
                    }
                }
                Action::EdgeFocus(scene, _) => {
                    if let Some(surface) = self.surfaces.get(&scene) {
                        // Read current membership instead of replaying a
                        // queued event from an earlier focus move.
                        let focused = lp
                            .inner
                            .surface()
                            .registry
                            .as_ref()
                            .and_then(|r| r.keyboard_focus());
                        if self
                            .menu_surface
                            .as_ref()
                            .is_some_and(|menu| Some(menu.handle) == focused)
                            && self
                                .host
                                .panels
                                .menu()
                                .is_some_and(|menu| menu.output == surface.output())
                        {
                            // A spawned popup temporarily owns the keyboard;
                            // keep the panel's focus holder for its return.
                            continue;
                        }
                        let edge = self
                            .surfaces
                            .iter()
                            .find(|(_, other)| {
                                other.output() == surface.output()
                                    && Some(other.handle()) == focused
                            })
                            .and_then(|(name, _)| self.host.store.scene(name))
                            .filter(|entry| !crate::mount::is_dialog(entry.tree()))
                            .filter(|entry| {
                                let edge = crate::mount::scene_edge(entry.tree());
                                !self.host.panels.hidden(surface.output(), edge)
                                    && self.host.panels.state(surface.output(), edge)["page"]
                                        .as_str()
                                        == Some(crate::mount::page_id(entry.tree()).as_str())
                            })
                            .map(|entry| crate::mount::scene_edge(entry.tree()));
                        self.host.panels.focus(surface.output(), edge);
                        serviced.changed = true;
                    }
                }
                Action::Menu(serial, input) => match self.host.panels.menu_input(serial, input) {
                    Ok(extra) => {
                        self.host.menu_calls.extend(extra);
                        serviced.changed = true;
                    }
                    Err(error) => {
                        tracing::debug!("scene host: menu input refused: {}", error.message)
                    }
                },
            }
        }
        let mut layouts = std::mem::take(&mut self.pending_layouts).into_iter();
        while let Some(message) = layouts
            .next()
            .map(Inbound::Request)
            .or_else(|| self.port.try_recv())
        {
            match message {
                Inbound::Registered(name) => self.service = Some(name),
                Inbound::Refused(reason) => self.refused = Some(reason),
                Inbound::Request(request) => {
                    let generation = self.port.connection_generation();
                    if request.verb == SceneVerb::Layout
                        && verify_caller_provenance(&request).is_ok()
                        && generation.is_none_or(|live| live == request.generation)
                    {
                        let args: Value =
                            serde_json::from_str(&request.body).unwrap_or(Value::Null);
                        if let Some(scene) = argument(&request, &args, "scene")
                            && self.host.mapped_output(scene).is_some()
                            && !self.surfaces.contains_key(scene)
                        {
                            self.pending_layouts.push(request);
                            serviced.changed = true;
                            continue;
                        }
                    }
                    let (surfaces, placed) = (&self.surfaces, &self.placed);
                    let mut layout = |store: &SceneStore, scene: &str, node: Option<&str>| {
                        crate::render::layout(store, surfaces, placed, &mut *lp, scene, node)
                    };
                    let answer = self.host.answer(&request, output, generation, &mut layout);
                    // A load or unload changes the pages the edges carry.
                    self.host.panels.sync(&self.host.store);
                    // A pre-empted owner hears before the reply's own caller.
                    for wire in answer.publish {
                        self.port.publish(wire);
                    }
                    self.port.reply(&request, answer.rc, answer.body);
                    serviced.changed |= answer.changed;
                    serviced.answered += 1;
                }
                Inbound::Live(live) => {
                    let notices = self.host.services_live(live);
                    self.host.panels.sync(&self.host.store);
                    serviced.changed |= !notices.is_empty();
                    for wire in notices {
                        self.port.publish(wire);
                    }
                }
            }
        }
        for extra in self.host.menu_calls.drain(..) {
            if !self
                .port
                .emit(&extra.target, &extra.verb, json!({"args":extra.args}))
            {
                tracing::warn!(
                    "scene host: corner menu Bus call {} to {} not queued",
                    extra.verb,
                    extra.target
                );
            }
        }
        self.publish_panels(output);
        serviced
    }

    /// `<service>.panel.changed` once per change of each output's edges or the
    /// dialog seat (never on a timer), while the Bus is connected.
    fn publish_panels(&mut self, output: &str) {
        let Some(generation) = self.port.connection_generation() else {
            return;
        };
        let dialog = dialog_notice(&self.host.store, self.host.dialog_fit.as_ref());
        for wire in self.host.panels.notices(output, dialog, Some(generation)) {
            self.port.publish_panel(wire);
        }
    }

    /// Advance the panel model to now (motion, grace): what the frame clock
    /// owes it next. A changed edge publishes.
    /// A tick that moved a reserved zone (an edge docked, undocked or resized)
    /// answers `Animate` at least once, so one more loop pass follows and
    /// the usable area, read on that pass (`exclusive_zones`), catches up.
    pub fn tick_panels(&mut self, output: &str) -> crate::panels::Wake {
        for (owner, name) in self.host.panels.popup_commands() {
            self.port
                .emit(&owner, "scenes.toggle", json!({"name": name}));
        }
        let before = self.host.panels.zones(output);
        let wake = self.host.panels.tick();
        self.publish_panels(output);
        if wake != crate::panels::Wake::Animate && self.host.panels.zones(output) != before {
            return crate::panels::Wake::Animate;
        }
        wake
    }

    /// One output's frame: create, update or destroy the scenes' surfaces.
    pub fn render(
        &mut self,
        state: &mut world::state::Loop,
        renderer: &mut smithay::backend::renderer::gles::GlesRenderer,
        size: smithay::utils::Size<i32, smithay::utils::Physical>,
    ) {
        // The prepared authority presentation supplies scene/dialog/menu
        // defaults; embedded scene design remains available during bootstrap.
        let palette = decor::window::installed()
            .map(|theme| theme.palette)
            .unwrap_or_else(|| {
                decor::ChromeTheme::from_source(decor::ChromeStyle::Mac, None).palette
            });
        let prepared = self
            .settings
            .host()
            .presentation()
            .map(|presentation| Arc::clone(&presentation.content().prepared));
        // Return the old menu's keyboard before a newly shown dialog grabs
        // it, so that dialog remembers the application's original focus.
        if self.host.panels.menu().is_none() {
            crate::menu::reconcile(
                &mut self.host.panels,
                &mut self.menu_surface,
                palette,
                prepared.clone(),
                self.appearance_generation,
                &self.wiring,
                state,
                renderer,
                false,
            );
        }
        let prior: BTreeSet<_> = self.surfaces.values().map(Surface::handle).collect();
        crate::render::reconcile(
            &mut self.host.store,
            &mut self.host.panels,
            &mut self.surfaces,
            &mut self.placed,
            palette,
            prepared.clone(),
            self.appearance_generation,
            &self.wiring,
            state,
            renderer,
            size,
        );
        let restack = self
            .surfaces
            .values()
            .any(|surface| !prior.contains(&surface.handle()));
        crate::menu::reconcile(
            &mut self.host.panels,
            &mut self.menu_surface,
            palette,
            prepared,
            self.appearance_generation,
            &self.wiring,
            state,
            renderer,
            restack,
        );
        let seat = self.host.store.dialog_seat().map(|seat| seat.scene.clone());
        self.host.dialog_fit =
            seat.and_then(|scene| self.placed.get(&scene).map(|&(_, _, w, h)| (scene, w, h)));
        // Read back on the next loop pass, after this frame has applied the
        // new instance's pending resize/scale and ticked it. A request that
        // raced panel.state must measure that surface, not an empty layout.
        if !self.pending_layouts.is_empty() {
            (self.wiring.waker)();
        }
    }

    pub fn finish(self) {
        self.port.finish();
    }

    /// Every scene surface drawn on `output`, as layer surfaces (see
    /// [`SceneSurface`]), in scene-name order.
    pub fn surfaces(&self, output: &str) -> Vec<SceneSurface> {
        let zones = self.host.panels.zones(output);
        let mut surfaces: Vec<_> = self
            .surfaces
            .iter()
            .filter(|(_, surface)| surface.output() == output || surface.output().is_empty())
            .filter_map(|(name, _)| {
                let entry = self.host.store.scene(name)?;
                entry.mounted()?;
                let &(x, y, width, height) = self.placed.get(name)?;
                let dialog = crate::mount::is_dialog(entry.tree());
                let edge = (!dialog).then(|| crate::mount::scene_edge(entry.tree()));
                // Quoin's layers: the dialog and an undocked (pinned or
                // sliding) page are Overlay; a docked page is Top and
                // reserves its thickness.
                let docked = edge.and_then(|edge| {
                    zones
                        .iter()
                        .find(|(zone, _)| *zone == edge)
                        .map(|(_, px)| *px)
                });
                let interactivity = if dialog || crate::render::takes_keyboard(entry.tree()) {
                    "on_demand"
                } else {
                    "none"
                };
                Some(SceneSurface {
                    id: scene_surface_id(name),
                    scene: name.clone(),
                    x,
                    y,
                    width,
                    height,
                    stratum: if docked.is_some() { "top" } else { "overlay" },
                    interactivity,
                    edge,
                    exclusive_zone: docked.unwrap_or(0.0),
                })
            })
            .collect();
        if let Some(menu) = self.host.panels.menu().filter(|menu| menu.output == output) {
            let (x, y, width, height) = menu.rect;
            surfaces.push(SceneSurface {
                id: scene_surface_id(crate::menu::SCENE),
                scene: crate::menu::SCENE.into(),
                x,
                y,
                width,
                height,
                stratum: "overlay",
                interactivity: "exclusive",
                edge: None,
                exclusive_zone: 0.0,
            });
        }
        surfaces
    }

    /// The scene whose surface is `handle`, if any.
    pub fn scene_of(&self, handle: ui::HandleId) -> Option<&str> {
        if self
            .menu_surface
            .as_ref()
            .is_some_and(|surface| surface.handle == handle)
        {
            return Some(crate::menu::SCENE);
        }
        self.surfaces
            .iter()
            .find(|(_, surface)| surface.handle() == handle)
            .map(|(name, _)| name.as_str())
    }
}

/// The id a scene surface has in comp.props (`surfaces` rows and
/// `focus.keyboard`): `scene:<name>`.
pub fn scene_surface_id(scene: &str) -> String {
    format!("scene:{scene}")
}

const WIRE_OWNED_HEADERS: &[&str] = &[
    "bus",
    "type",
    "id",
    "from",
    "to",
    "command",
    "args",
    "json",
    "reply-to",
    "ttl",
    "error",
    "timestamp",
    "rc",
];

/// Quoin's argument precedence: caller headers first, transport names only
/// from the body (in particular, a page id is never the correlation id).
fn argument<'a>(request: &'a Request, args: &'a Value, name: &str) -> Option<&'a str> {
    if !WIRE_OWNED_HEADERS
        .iter()
        .any(|owned| owned.eq_ignore_ascii_case(name))
        && let Some(value) = request.headers.get(name)
    {
        return Some(value);
    }
    args.get(name)?.as_str()
}

/// Quoin's `number_argument`: the body only, never a header (unlike
/// [`argument`]); a JSON number or a numeric string, finite.
fn number_argument(args: &Value, name: &str) -> Option<f64> {
    let field = args.get(name)?;
    let number = field
        .as_f64()
        .or_else(|| field.as_str().and_then(|text| text.parse::<f64>().ok()))?;
    number.is_finite().then_some(number)
}

/// One scene surface, shaped as comp.props reports a layer surface. Quoin
/// draws scenes on layer surfaces, so its `surfaces` rows and
/// `focus.keyboard` name them; compd draws them as screen iced surfaces,
/// and without these rows an agent reading comp.props would not see them
/// at all, and would read the keyboard as still on the window under a
/// dialog that holds it.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneSurface {
    /// [`scene_surface_id`].
    pub id: String,
    pub scene: String,
    /// Logical px relative to the OUTPUT's origin (add it for global).
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// `overlay` (the dialog, an undocked page) or `top` (a docked page).
    pub stratum: &'static str,
    /// `on_demand` (the dialog, a scene with a text field) or `none`.
    pub interactivity: &'static str,
    /// An edge page's edge; `None` for the dialog.
    pub edge: Option<crate::seat::Edge>,
    /// Logical px a docked page reserves, else 0.
    pub exclusive_zone: f32,
}

impl SceneSurface {
    /// The comp.props `surfaces` row, `origin` being the output's logical
    /// origin. Fields comp.props has and a scene has no answer for (app_id,
    /// title, decoration, foreign_id) are left to the caller's defaults.
    pub fn row(&self, origin: (f32, f32)) -> Value {
        json!({
            "id": self.id,
            "role": "layer",
            "mapped": true,
            "scene": self.scene,
            "x": origin.0 + self.x,
            "y": origin.1 + self.y,
            "width": self.width,
            "height": self.height,
            "layer": {
                "stratum": self.stratum,
                "interactivity": self.interactivity,
                "exclusive_zone": self.exclusive_zone,
                "binding": "explicit",
                "namespace": "quoin-scene",
                "edge": self.edge.map(crate::seat::Edge::as_str),
            },
        })
    }
}

/// A registered local sender is the broker-restamped `from`; an attested
/// mesh identity is qualified so a remote service cannot alias a local
/// owner; an anonymous caller gets a distinct untracked owner per
/// acceptance. Caller-authored metadata never chooses ownership.
pub fn attested_owner(request: &Request, receipt: u64) -> String {
    if let (Some(peer), Some(service)) = (
        request.headers.get("broker_peer"),
        request.headers.get("broker_service"),
    ) {
        return format!("{service}@{peer}");
    }
    if request.from.is_empty() {
        format!("anonymous@{receipt}")
    } else {
        request.from.clone()
    }
}

/// Exactly one broker-stamped origin. The mesh lane is open (the mesh is the
/// trust boundary); a local request that spells the broker's own identity
/// headers cannot be adjudicated and is refused. Not an authorization gate:
/// a well-formedness check on who the broker says sent it.
pub fn verify_caller_provenance(request: &Request) -> Result<(), &'static str> {
    let origins: Vec<&str> = request
        .headers
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case("broker_origin"))
        .map(|(_, value)| value.as_str())
        .collect();
    if origins == ["mesh"] {
        return Ok(());
    }
    let asserted = request.headers.keys().any(|name| {
        name.eq_ignore_ascii_case("source_peer")
            || name.eq_ignore_ascii_case("permissions")
            || name.eq_ignore_ascii_case("signed_ident")
    });
    if asserted {
        return Err("a local request may not assert the broker's identity headers");
    }
    if origins != ["local"] {
        return Err("no single broker-stamped origin");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    const EDGE: &str = "---\nscene: 1\nname: notes\ncitizen: notes-citizen\nwindow: {\"kind\":\"edge\",\"edge\":\"right\"}\n---\n```mix\nroot: {widget: \"column\", children: []}\n```\n";

    fn request(verb: SceneVerb, from: &str, headers: &[(&str, &str)], body: Value) -> Request {
        Request {
            verb,
            from: from.into(),
            command: format!("shell.{verb:?}").to_lowercase(),
            id: Some("1".into()),
            body: body.to_string(),
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<BTreeMap<_, _>>(),
            generation: 1,
        }
    }

    const LOCAL: &[(&str, &str)] = &[("broker_origin", "local")];

    #[test]
    fn arguments_use_caller_headers_but_never_transport_headers() {
        let args = json!({"edge":"left", "id":"scene-notes", "thickness_px":240});
        let request = request(
            SceneVerb::PanelResize,
            "x",
            &[("edge", "right"), ("id", "99"), ("thickness_px", "360")],
            args.clone(),
        );
        assert_eq!(argument(&request, &args, "edge"), Some("right"));
        assert_eq!(argument(&request, &args, "id"), Some("scene-notes"));
        assert_eq!(argument(&request, &Value::Null, "id"), None);
        assert_eq!(
            number_argument(&args, "thickness_px"),
            Some(240.0),
            "a number is read from the body only, as in Quoin"
        );
    }

    #[test]
    fn resize_numbers_accept_strings_and_refuse_non_finite_or_missing_values_with_the_range() {
        let mut host = Host::default();
        for value in [json!(240), json!("240")] {
            let args = json!({"thickness_px":value});
            assert_eq!(number_argument(&args, "thickness_px"), Some(240.0));
        }
        for value in [
            Value::Null,
            json!(true),
            json!("nope"),
            json!("NaN"),
            json!("inf"),
            json!("1e999"),
            json!(10),
        ] {
            let request = request(
                SceneVerb::PanelResize,
                "x",
                LOCAL,
                json!({"edge":"bottom", "thickness_px":value}),
            );
            let answer = host.answer(&request, "DP-1", Some(1), &mut no_layout);
            assert_eq!((answer.rc, answer.changed), (10, false));
            assert_eq!(
                serde_json::from_str::<Value>(&answer.body).unwrap(),
                json!({
                    "error":"thickness_px must be a number in 24..=200 for the bottom edge", "range_px":[24.0, 200.0],
                })
            );
        }
        let missing = request(
            SceneVerb::PanelResize,
            "x",
            &[("broker_origin", "local"), ("edge", "bottom")],
            Value::Null,
        );
        let answer = host.answer(&missing, "DP-1", Some(1), &mut no_layout);
        assert_eq!(
            serde_json::from_str::<Value>(&answer.body).unwrap(),
            crate::panels::resize_range_refusal(crate::seat::Edge::Bottom)
        );
        assert_eq!(
            number_argument(&json!({"thickness_px":240}), "thickness_px"),
            Some(240.0),
            "a thickness_px header is ignored"
        );
    }

    #[test]
    fn a_header_routed_pin_returns_the_applied_panels_snapshot() {
        let mut host = Host::default();
        host.panels.ensure("DP-1", (1280.0, 800.0));
        host.answer(
            &request(SceneVerb::Load, "loader", LOCAL, json!({"source":EDGE})),
            "DP-1",
            Some(1),
            &mut no_layout,
        );
        host.panels.sync(&host.store);
        let pin = request(
            SceneVerb::Panel {
                verb: crate::panels::PanelVerb::Pin,
                corner: false,
            },
            "x",
            &[("broker_origin", "local"), ("edge", "right")],
            json!({"edge":"left"}),
        );
        let answer = host.answer(&pin, "DP-1", Some(1), &mut no_layout);
        assert_eq!((answer.rc, answer.changed), (0, true));
        assert_eq!(
            serde_json::from_str::<Value>(&answer.body).unwrap(),
            json!({
                "accepted":true, "applied":true, "panels":host.panels.snapshot("DP-1", Value::Null)["panels"],
            })
        );
        assert_eq!(host.panels.mode("DP-1", crate::seat::Edge::Right), "pinned");
    }

    #[test]
    fn bus_menu_open_choose_close_reports_state_and_fences_stale_choices() {
        let mut host = Host::default();
        host.panels.ensure("DP-1", (1280.0, 800.0));
        host.answer(
            &request(SceneVerb::Load, "loader", LOCAL, json!({"source":EDGE})),
            "DP-1",
            Some(1),
            &mut no_layout,
        );
        host.panels.sync(&host.store);
        let open = request(
            SceneVerb::MenuOpen,
            "agent",
            LOCAL,
            json!({"corner":"bottom-right"}),
        );
        let answer = host.answer(&open, "DP-1", Some(1), &mut no_layout);
        assert_eq!((answer.rc, answer.changed), (0, true));
        let body: Value = serde_json::from_str(&answer.body).unwrap();
        assert_eq!(body["menu"]["edge"], "right");
        assert_eq!(body["menu"]["items"][2]["checked"], true);
        let serial = body["menu"]["serial"].as_u64().unwrap();
        let props = host.answer(
            &request(SceneVerb::PropsGet, "agent", LOCAL, json!({"path":"menu"})),
            "DP-1",
            Some(1),
            &mut no_layout,
        );
        assert_eq!(
            serde_json::from_str::<Value>(&props.body).unwrap(),
            body["menu"]
        );
        let choose = request(
            SceneVerb::MenuChoose,
            "agent",
            LOCAL,
            json!({"serial":serial, "index":1}),
        );
        assert_eq!(host.answer(&choose, "DP-1", Some(1), &mut no_layout).rc, 0);
        assert_eq!(host.panels.mode("DP-1", crate::seat::Edge::Right), "docked");
        assert!(host.panels.menu().is_none());
        assert_eq!(host.answer(&choose, "DP-1", Some(1), &mut no_layout).rc, 10);
        host.answer(&open, "DP-1", Some(1), &mut no_layout);
        let serial = host.panels.menu().unwrap().serial;
        let close = request(
            SceneVerb::MenuClose,
            "agent",
            LOCAL,
            json!({"serial":serial}),
        );
        assert_eq!(host.answer(&close, "DP-1", Some(1), &mut no_layout).rc, 0);
        assert!(host.panels.menu().is_none());
    }

    fn no_layout(_: &SceneStore, scene: &str, _: Option<&str>) -> Result<Value, Value> {
        Err(json!({"error_code":"UNIMPLEMENTED", "scene": scene}))
    }

    #[test]
    fn mounted_loader_pages_register_the_shared_corner_toggle_route() {
        for (name, edge, point) in [
            ("launcher", "left", (1.0, 1.0)),
            ("calendar", "right", (1279.0, 799.0)),
        ] {
            let mut host = Host::default();
            host.panels.ensure("DP-1", (1280.0, 800.0));
            let source = EDGE.replace("notes", name).replace("right", edge);
            let answer = host.answer(
                &request(SceneVerb::Load, "scenes", LOCAL, json!({"source":source})),
                "DP-1",
                Some(1),
                &mut no_layout,
            );
            assert_eq!(answer.rc, 0);
            host.panels.sync(&host.store);
            let config = edges::CornerDetectorConfig::new(
                10.0,
                std::time::Duration::from_millis(200),
                1500.0,
            )
            .unwrap();
            host.panels.pointer("DP-1", Some(point), config);
            assert!(host.panels.pointer_button(0x110, true, false));
            assert!(host.panels.pointer_button(0x110, false, false));
            assert_eq!(
                host.panels.popup_commands(),
                vec![("scenes".into(), name.into())]
            );
            assert_eq!(
                host.panels
                    .mode("DP-1", crate::panels::parse_edge(edge).unwrap()),
                "hidden"
            );
        }
    }

    #[test]
    fn a_newly_mapped_page_needs_layout_even_before_its_first_render_target() {
        let mut host = Host::default();
        host.panels.ensure("DP-1", (1280.0, 800.0));
        host.answer(
            &request(SceneVerb::Load, "loader", LOCAL, json!({"source": EDGE})),
            "DP-1",
            Some(1),
            &mut no_layout,
        );
        host.panels.sync(&host.store);
        assert_eq!(host.mapped_output("notes"), None);
        host.panels
            .set_mode("DP-1", crate::seat::Edge::Right, "pinned")
            .unwrap();
        assert_eq!(host.mapped_output("notes").as_deref(), Some("DP-1"));
        assert!(
            crate::render::targets(&host.store, &host.panels).is_empty(),
            "fraction zero: no surface can be created yet"
        );

        let calendar = EDGE.replace("notes", "calendar");
        host.answer(
            &request(
                SceneVerb::Load,
                "loader",
                LOCAL,
                json!({"source": calendar}),
            ),
            "DP-1",
            Some(1),
            &mut no_layout,
        );
        host.panels.sync(&host.store);
        host.panels
            .page_set("DP-1", crate::seat::Edge::Right, "scene-calendar")
            .unwrap();
        assert_eq!(
            host.mapped_output("notes"),
            None,
            "inactive pages still answer unmapped immediately"
        );
        assert_eq!(host.mapped_output("calendar").as_deref(), Some("DP-1"));
        host.answer(
            &request(
                SceneVerb::Unload,
                "loader",
                LOCAL,
                json!({"scene":"calendar"}),
            ),
            "DP-1",
            Some(1),
            &mut no_layout,
        );
        assert_eq!(
            host.mapped_output("calendar"),
            None,
            "an unload releases a deferred layout to NOT_FOUND"
        );
    }

    #[test]
    fn provenance_is_one_broker_origin_mesh_open() {
        let ok = |headers: &[(&str, &str)]| {
            verify_caller_provenance(&request(SceneVerb::Get, "a", headers, Value::Null))
        };
        assert!(ok(LOCAL).is_ok());
        assert!(ok(&[("broker_origin", "mesh")]).is_ok());
        assert!(
            ok(&[("broker_origin", "mesh"), ("signed_ident", "x")]).is_ok(),
            "mesh is the trust boundary"
        );
        assert!(ok(&[]).is_err());
        assert!(ok(&[("broker_origin", "local"), ("source_peer", "x")]).is_err());
        assert!(ok(&[("broker_origin", "remote")]).is_err());
    }

    #[test]
    fn a_refused_provenance_spends_no_receipt_and_changes_nothing() {
        let mut host = Host::default();
        let answer = host.answer(
            &request(SceneVerb::Load, "loader", &[], json!({"source": EDGE})),
            "DP-1",
            Some(1),
            &mut no_layout,
        );
        assert_eq!(answer.rc, 10);
        let body: Value = serde_json::from_str(&answer.body).unwrap();
        assert_eq!(body["error_code"], "SCENE_PROVENANCE");
        assert!(
            host.store.scene("notes").is_none() && !answer.changed && answer.publish.is_empty()
        );
        assert_eq!(host.receipt, 0);
    }

    #[test]
    fn owners_are_attested_and_each_request_takes_a_receipt() {
        let mut host = Host::default();
        let answer = host.answer(
            &request(SceneVerb::Load, "loader", LOCAL, json!({"source": EDGE})),
            "DP-1",
            Some(1),
            &mut no_layout,
        );
        assert_eq!(answer.rc, 0, "{}", answer.body);
        assert!(answer.changed && answer.publish.len() == 1);
        let seat = host.store.pages().seat("scene-notes").unwrap();
        assert_eq!(
            (seat.owner.as_str(), seat.accepted_at, seat.output.as_str()),
            ("loader", 1, "DP-1")
        );
        let mesh = [
            ("broker_origin", "mesh"),
            ("broker_peer", "beta"),
            ("broker_service", "editor"),
        ];
        assert_eq!(
            attested_owner(&request(SceneVerb::Get, "editor", &mesh, Value::Null), 9),
            "editor@beta"
        );
        assert_eq!(
            attested_owner(&request(SceneVerb::Get, "", LOCAL, Value::Null), 9),
            "anonymous@9"
        );
        // Reads take a receipt but change nothing.
        let list = host.answer(
            &request(SceneVerb::List, "x", LOCAL, Value::Null),
            "DP-1",
            Some(1),
            &mut no_layout,
        );
        assert_eq!((list.rc, list.changed), (0, false));
        assert_eq!(
            serde_json::from_str::<Value>(&list.body).unwrap()[0]["registered"],
            true
        );
        assert_eq!(host.receipt, 2);
    }

    #[test]
    fn a_request_from_an_earlier_connection_is_refused_unapplied() {
        let mut host = Host::default();
        let load = request(SceneVerb::Load, "loader", LOCAL, json!({"source": EDGE}));
        let stale = host.answer(&load, "DP-1", Some(2), &mut no_layout);
        assert_eq!(stale.rc, 10);
        assert_eq!(
            serde_json::from_str::<Value>(&stale.body).unwrap()["error_code"],
            "SCENE_STALE_CONNECTION"
        );
        assert!(host.store.scene("notes").is_none() && !stale.changed && stale.publish.is_empty());
        assert_eq!(host.receipt, 0, "a stale request spends no receipt");
        // The same request on the live connection applies.
        assert_eq!(host.answer(&load, "DP-1", Some(1), &mut no_layout).rc, 0);
    }

    #[test]
    fn dialog_show_and_hide_answer_the_frozen_bodies() {
        let frozen: Value =
            serde_json::from_str(include_str!("../tests/fixtures/shell-verbs.json")).unwrap();
        let body = |answer: &Answer| serde_json::from_str::<Value>(&answer.body).unwrap();
        let mut host = Host::default();
        let missing = host.answer(
            &request(SceneVerb::DialogShow, "x", LOCAL, json!({"scene":"editor"})),
            "DP-1",
            Some(1),
            &mut no_layout,
        );
        assert_eq!(
            body(&missing),
            frozen["shell.dialog.show"]["refusals"]["NOT_FOUND"]
        );
        let panel = EDGE.replace("name: notes", "name: panel");
        host.answer(
            &request(SceneVerb::Load, "loader", LOCAL, json!({"source": panel})),
            "DP-1",
            Some(1),
            &mut no_layout,
        );
        let edge = host.answer(
            &request(SceneVerb::DialogHide, "x", LOCAL, json!({"scene":"panel"})),
            "DP-1",
            Some(1),
            &mut no_layout,
        );
        assert_eq!(
            body(&edge),
            frozen["shell.dialog.hide"]["refusals"]["NOT_DIALOG"]
        );
        let dialog = frozen["shell.scene.load"]["request"]["source"]
            .as_str()
            .unwrap();
        host.answer(
            &request(SceneVerb::Load, "scenes", LOCAL, json!({"source": dialog})),
            "DP-1",
            Some(1),
            &mut no_layout,
        );
        assert!(
            !host.store.scene("editor").unwrap().visible(),
            "a loaded dialog waits unmapped"
        );
        for (verb, name) in [
            (SceneVerb::DialogShow, "shell.dialog.show"),
            (SceneVerb::DialogHide, "shell.dialog.hide"),
        ] {
            let answer = host.answer(
                &request(verb, "x", LOCAL, json!({"scene":"editor"})),
                "HDMI-A-1",
                Some(1),
                &mut no_layout,
            );
            assert_eq!((answer.rc, answer.changed), (0, true));
            assert_eq!(body(&answer), frozen[name]["reply"]);
            assert_eq!(
                host.store.scene("editor").unwrap().visible(),
                verb == SceneVerb::DialogShow
            );
        }
        assert_eq!(
            host.store.dialog_seat().unwrap().output,
            "HDMI-A-1",
            "shown on the selected output"
        );
    }

    #[test]
    fn layout_refuses_an_unknown_scene_and_asks_the_renderer_for_a_loaded_one() {
        let mut host = Host::default();
        let missing = host.answer(
            &request(SceneVerb::Layout, "x", LOCAL, json!({"scene":"editor"})),
            "DP-1",
            Some(1),
            &mut no_layout,
        );
        let frozen: Value =
            serde_json::from_str(include_str!("../tests/fixtures/shell-verbs.json")).unwrap();
        assert_eq!(missing.rc, 10);
        assert_eq!(
            serde_json::from_str::<Value>(&missing.body).unwrap(),
            frozen["shell.scene.layout"]["refusals"]["NOT_FOUND"]
        );
        host.answer(
            &request(SceneVerb::Load, "loader", LOCAL, json!({"source": EDGE})),
            "DP-1",
            Some(1),
            &mut no_layout,
        );
        // A loaded scene is measured by the renderer's reader, narrowed by `node`.
        let mut asked = Vec::new();
        let mut reader =
            |_: &SceneStore, scene: &str, node: Option<&str>| -> Result<Value, Value> {
                asked.push((scene.to_owned(), node.map(str::to_owned)));
                Ok(json!({"scene": scene, "visible": true}))
            };
        let measured = host.answer(
            &request(
                SceneVerb::Layout,
                "x",
                LOCAL,
                json!({"scene":"notes","node":"root"}),
            ),
            "DP-1",
            Some(1),
            &mut reader,
        );
        assert_eq!((measured.rc, measured.changed), (0, false));
        assert_eq!(
            serde_json::from_str::<Value>(&measured.body).unwrap()["visible"],
            true
        );
        assert_eq!(asked, [("notes".to_owned(), Some("root".to_owned()))]);
        let refused = host.answer(
            &request(SceneVerb::Layout, "x", LOCAL, json!({"scene":"notes"})),
            "DP-1",
            Some(1),
            &mut no_layout,
        );
        assert_eq!(refused.rc, 10);
    }

    #[test]
    fn a_scene_surface_row_is_a_layer_row_in_global_coordinates() {
        let dialog = SceneSurface {
            id: scene_surface_id("editor"),
            scene: "editor".into(),
            x: 200.0,
            y: 90.0,
            width: 880.0,
            height: 620.0,
            stratum: "overlay",
            interactivity: "on_demand",
            edge: None,
            exclusive_zone: 0.0,
        };
        assert_eq!(
            dialog.row((1280.0, 0.0)),
            json!({"id":"scene:editor", "role":"layer", "mapped":true, "scene":"editor",
                "x":1480.0, "y":90.0, "width":880.0, "height":620.0,
                "layer":{"stratum":"overlay", "interactivity":"on_demand", "exclusive_zone":0.0,
                    "binding":"explicit", "namespace":"quoin-scene", "edge":null}})
        );
        let panel = SceneSurface {
            edge: Some(crate::seat::Edge::Bottom),
            stratum: "top",
            exclusive_zone: 52.0,
            ..dialog
        };
        assert_eq!(panel.row((0.0, 0.0))["layer"]["edge"], "bottom");
    }

    #[test]
    fn only_a_seen_owner_that_leaves_is_swept() {
        let mut host = Host::default();
        host.answer(
            &request(SceneVerb::Load, "loader", LOCAL, json!({"source": EDGE})),
            "DP-1",
            Some(1),
            &mut no_layout,
        );
        // The first diff is the baseline even when the owner is absent.
        assert!(
            host.services_live(BTreeSet::from(["comp".to_string()]))
                .is_empty()
        );
        assert!(host.store.scene("notes").is_some());
        assert!(
            host.services_live(BTreeSet::from(["comp".to_string(), "loader".to_string()]))
                .is_empty()
        );
        let notices = host.services_live(BTreeSet::from(["comp".to_string()]));
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("owner_departed"), "{}", notices[0]);
        assert!(host.store.scene("notes").is_none());
        assert!(host.store.pages().seat("scene-notes").is_none());
    }
}
