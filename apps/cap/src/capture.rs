// SPDX-License-Identifier: MIT OR Apache-2.0
//! A capture job runs through the native Bus; completion is a final RPC reply.
//! The compositor applies fenced minimisation before the newly requested frame.
use crate::{bus::BusHandle, document::Document};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Duration,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Screen,
    Window,
    Region,
}
impl Mode {
    pub const ALL: [Self; 3] = [Self::Screen, Self::Window, Self::Region];
    pub fn key(self) -> &'static str {
        match self {
            Self::Screen => "screen",
            Self::Window => "window",
            Self::Region => "region",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub id: u64,
    pub generation: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    #[serde(default)]
    pub mode: Mode,
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default)]
    pub window: Option<Target>,
    #[serde(default = "yes")]
    pub cursor: bool,
    #[serde(default)]
    pub delay: u32,
}
fn yes() -> bool {
    true
}
impl Default for Request {
    fn default() -> Self {
        Self {
            mode: Mode::Screen,
            output: None,
            window: None,
            cursor: true,
            delay: 0,
        }
    }
}
impl Request {
    pub fn validate(&self) -> Result<(), String> {
        if self.delay > 10 {
            return Err("delay must be 0..10 seconds".into());
        }
        if self
            .output
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.len() > 256)
        {
            return Err("invalid output name".into());
        }
        if self.mode == Mode::Window && self.window.is_none() {
            return Err("choose a window first".into());
        }
        if self.mode != Mode::Window && self.window.is_some() {
            return Err("window only applies to window mode".into());
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub target: Target,
    pub title: String,
    pub app_id: String,
    pub focused: bool,
    pub minimized: bool,
}
pub fn windows(value: &Value) -> Vec<Window> {
    value["windows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| {
            Some(Window {
                target: Target {
                    id: row["id"].as_u64()?,
                    generation: row["generation"].as_u64()?,
                },
                title: row["title"].as_str().unwrap_or("").into(),
                app_id: row["app_id"].as_str().unwrap_or("").into(),
                focused: row["focused"].as_bool().unwrap_or(false),
                minimized: row["minimized"].as_bool().unwrap_or(false),
            })
        })
        .collect()
}
pub fn selected_window(windows: &[Window], previous: Option<&Window>) -> Option<Window> {
    previous
        .and_then(|old| windows.iter().find(|w| w.target == old.target))
        .or_else(|| windows.iter().find(|w| w.focused))
        .or_else(|| windows.first())
        .cloned()
}
pub async fn list(bus: &BusHandle, comp: &str) -> Result<Value, String> {
    bus.call(comp, "comp.windows.list", json!({}), Duration::from_secs(5))
        .await
}
pub async fn own_window(bus: &BusHandle, comp: &str) -> Result<Target, String> {
    let rows = list(bus, comp).await?;
    rows["windows"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| {
            row["app_id"] == crate::app::APP_ID
                && row["pid"].as_u64() == Some(u64::from(std::process::id()))
        })
        .and_then(|row| {
            Some(Target {
                id: row["id"].as_u64()?,
                generation: row["generation"].as_u64()?,
            })
        })
        .ok_or_else(|| "Cap window is not yet known to compd".into())
}
pub async fn show(bus: &BusHandle, comp: &str, target: Target) -> Result<Value, String> {
    let restored = bus
        .call(
            comp,
            "comp.window.restore",
            json!(target),
            Duration::from_secs(5),
        )
        .await?;
    if restored["minimized"].as_bool() != Some(false) {
        return Err(format!("Cap restoration was not confirmed: {restored}"));
    }
    let focused = bus
        .call(
            comp,
            "comp.window.focus",
            json!({"id":target.id,"generation":target.generation,"raise":true}),
            Duration::from_secs(5),
        )
        .await?;
    if focused["focused"].as_bool() != Some(true) {
        return Err(format!("Cap activation was refused: {focused}"));
    }
    Ok(focused)
}

#[derive(Debug, Clone)]
pub struct Captured {
    pub document: Document,
    pub path: PathBuf,
    pub metadata: Value,
}

/// The captures root through the shared directory resolver, preserving the
/// exact legacy precedence: app override, apps parent, isolated Var, XDG
/// state, then home state. The generic resolver's additional `MIXOS` fallback
/// is deliberately filtered out here — existing captures are never relocated.
/// Resolution performs no I/O; only the final capture directory is secured.
pub fn media_directory() -> Result<PathBuf, String> {
    let base = media_base(|key| std::env::var_os(key).map(PathBuf::from))
        .ok_or("no absolute application state directory")?;
    let directory = base.join("captures");
    secure_directory(&directory)?;
    Ok(directory)
}

fn media_base(get: impl Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    config::AppDirs::resolve_with("cap", |key| if key == "MIXOS" { None } else { get(key) })
        .map(|dirs| dirs.root().to_owned())
}

fn secure_directory(directory: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)
        .map_err(|e| e.to_string())?;
    // Secure existing installations too. Operate on the opened directory,
    // rejecting a final symlink and directories owned by another account.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(directory)
        .map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    // SAFETY: geteuid has no arguments or memory preconditions.
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err("capture directory belongs to another account".into());
    }
    file.set_permissions(std::fs::Permissions::from_mode(0o700))
        .map_err(|e| e.to_string())
}

/// The selection identity Cap presents for a region capture: the compositor's
/// own process instance, a per-Cap-process random owner capability, and a
/// capture generation that only increases. It is a targeting capability, not
/// broker authentication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Selection {
    pub instance: String,
    pub owner: String,
    pub generation: u64,
}
impl Selection {
    fn new(instance: String, generation: u64) -> Self {
        Self {
            instance,
            owner: owner(),
            generation,
        }
    }
}
/// One random owner capability per Cap process; never reused across captures.
fn owner() -> String {
    static OWNER: OnceLock<uuid::Uuid> = OnceLock::new();
    OWNER.get_or_init(uuid::Uuid::new_v4).to_string()
}
/// The compositor's own process identity, read from its `comp.info` snapshot.
pub async fn comp_instance(bus: &BusHandle, comp: &str) -> Result<String, String> {
    let info = bus
        .call(comp, "comp.info", json!({}), Duration::from_secs(5))
        .await?;
    info["instance"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "compositor info has no instance identity".into())
}
/// A cancellation cleanup obligation: cancel the owner's region selection and
/// restore Cap's own window, both fenced to the exact identities captured
/// when the attempt started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cleanup {
    pub selection: Option<Selection>,
    pub window: Option<Target>,
}
/// One bounded cleanup: `comp.region.cancel` and `comp.window.restore` are
/// submitted in parallel immediately and awaited together. Cancellation is
/// applied before any focus request may be made; Cap requests none here.
pub async fn cleanup(bus: &BusHandle, comp: &str, target: &Cleanup) -> Result<(), String> {
    let cancel = async {
        match &target.selection {
            Some(selection) => {
                bus.call(
                    comp,
                    "comp.region.cancel",
                    json!({"selection": selection}),
                    CLEANUP_BUDGET,
                )
                .await
            }
            None => Ok(json!({})),
        }
    };
    let restore = async {
        match &target.window {
            Some(window) => {
                bus.call(comp, "comp.window.restore", json!(window), CLEANUP_BUDGET)
                    .await
            }
            None => Ok(json!({})),
        }
    };
    let (cancel, restore) = tokio::join!(cancel, restore);
    let mut failures = Vec::new();
    if let Err(error) = cancel {
        failures.push(format!("region cancel: {error}"));
    }
    match restore {
        Err(error) => failures.push(format!("window restore: {error}")),
        Ok(reply) if reply["minimized"].as_bool() != Some(false) => {
            failures.push(format!("window restore unconfirmed: {reply}"));
        }
        Ok(_) => {}
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}
/// The outcome of a retained cleanup retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupOutcome {
    /// Cancellation and restoration were applied.
    Restored,
    /// The compositor instance no longer matches the retained identity;
    /// the target is retired, never retargeted.
    StaleInstance,
}
/// One explicit bounded cleanup retry, fenced to the exact retained identity:
/// the compositor instance must still match before anything is submitted.
pub async fn retry_cleanup(
    bus: &BusHandle,
    comp: &str,
    target: &Cleanup,
) -> Result<CleanupOutcome, String> {
    if let Some(selection) = &target.selection {
        let instance = comp_instance(bus, comp).await?;
        if instance != selection.instance {
            return Ok(CleanupOutcome::StaleInstance);
        }
    }
    cleanup(bus, comp, target)
        .await
        .map(|()| CleanupOutcome::Restored)
}
/// A capture failure with the truth about cancellation: `cleanup` carries the
/// exact identities a later Connected event may retry, never a rebuilt target.
#[derive(Debug, Clone)]
pub struct CaptureError {
    pub message: String,
    pub cleanup: Option<Cleanup>,
}
impl CaptureError {
    pub fn plain(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            cleanup: None,
        }
    }
    pub fn cancelled(cleanup: Option<Cleanup>) -> Self {
        Self {
            message: "cancelled".into(),
            cleanup,
        }
    }
    fn cleanup_failed(target: Cleanup, failure: String) -> Self {
        Self {
            message: format!("cancelled; cleanup failed: {failure}"),
            cleanup: Some(target),
        }
    }
}
impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for CaptureError {}

/// The explicit select timeout Cap requests, below the native mesh response
/// ceiling with room for the compositor's clean-frame allowance.
const REGION_TIMEOUT_MS: u64 = 20_000;
/// Cap's own limit for the select call: the timeout plus delivery slack.
const REGION_LIMIT: Duration = Duration::from_secs(26);
/// One cancellation cleanup budget, shared by cancel and restore.
const CLEANUP_BUDGET: Duration = Duration::from_secs(2);

/// `own` is resolved before minimising. The compositor executes its visibility
/// effect before answering `minimize`; a screenshot forces a fresh full frame.
/// Always restore a window we changed, including cancelled region selections.
/// The selection identity is read before hiding; cancellation races the
/// minimise, select and frame calls and always wins, submitting the native
/// cancel and restore in parallel within one bounded cleanup budget.
pub async fn take(
    bus: BusHandle,
    comp: String,
    request: Request,
    own: Option<Target>,
    directory: PathBuf,
    generation: u64,
    mut cancel: tokio::sync::watch::Receiver<bool>,
) -> Result<Captured, CaptureError> {
    request.validate().map_err(CaptureError::plain)?;
    if *cancel.borrow() {
        return Err(CaptureError::cancelled(None));
    }
    // The compositor identity is read before hiding, on the same configured
    // route the capture will use.
    let instance = comp_instance(&bus, &comp)
        .await
        .map_err(CaptureError::plain)?;
    let selection = Selection::new(instance, generation);
    let cleanup_target = Cleanup {
        // Only a region selection creates an owner overlay to cancel.
        selection: (request.mode == Mode::Region).then(|| selection.clone()),
        window: own.clone(),
    };
    let mut hidden = false;
    let result = async {
        if request.delay > 0 {
            use iced::futures::future::{Either, select};
            let timer = Box::pin(bus.delay(Duration::from_secs(u64::from(request.delay))));
            let cancelled = Box::pin(cancel.changed());
            match select(timer, cancelled).await {
                Either::Left((done, _)) => done.map_err(CaptureError::plain)?,
                Either::Right(_) => {
                    return Err(CaptureError::cancelled(Some(cleanup_target.clone())));
                }
            }
        }
        if *cancel.borrow() {
            return Err(CaptureError::cancelled(Some(cleanup_target.clone())));
        }
        // Hiding races cancellation: a cancel during minimise goes straight
        // to cleanup instead of awaiting the compositor. The window counts as
        // hidden from the moment the minimise is attempted, so a lost
        // acknowledgement still restores.
        if let Some(target) = &own {
            hidden = true;
            let hide = async {
                let reply = bus
                    .call(
                        &comp,
                        "comp.window.minimize",
                        json!(target),
                        Duration::from_secs(5),
                    )
                    .await
                    .map_err(CaptureError::plain)?;
                if reply["minimized"].as_bool() != Some(true) {
                    return Err(CaptureError::plain(
                        "compositor did not confirm Cap minimisation",
                    ));
                }
                Ok(())
            };
            tokio::select! {
                biased;
                _ = cancel.changed() => {
                    return Err(CaptureError::cancelled(Some(cleanup_target.clone())));
                }
                outcome = hide => {
                    outcome?;
                }
            }
        }
        if *cancel.borrow() {
            return Err(CaptureError::cancelled(Some(cleanup_target.clone())));
        }
        let path = directory.join(format!("cap-{}.png", uuid::Uuid::now_v7()));
        let mut args = json!({"path":path,"cursor":request.cursor});
        if let Some(target) = &request.window {
            args["window"] = json!(target)
        } else if let Some(output) = &request.output {
            args["output"] = json!(output)
        }
        if request.mode == Mode::Region {
            let selection_args = request.output.as_ref().map_or_else(
                || json!({"timeout_ms": REGION_TIMEOUT_MS, "selection": selection}),
                |o| json!({"output":o,"timeout_ms": REGION_TIMEOUT_MS, "selection": selection}),
            );
            let select = async {
                let selection = bus
                    .call(&comp, "comp.region.select", selection_args, REGION_LIMIT)
                    .await
                    .map_err(CaptureError::plain)?;
                if selection["status"] != "selected" {
                    return Err(CaptureError::plain(format!(
                        "selection {}",
                        selection["status"].as_str().unwrap_or("failed")
                    )));
                }
                Ok(selection)
            };
            // Cancellation wins over a simultaneously arriving selection.
            let selection = tokio::select! {
                biased;
                _ = cancel.changed() => {
                    return Err(CaptureError::cancelled(Some(cleanup_target.clone())));
                }
                outcome = select => outcome?,
            };
            args["output"] = selection["output"].clone();
            args["region"] = selection["region"].clone();
            args["output_generation"] = selection["output_generation"].clone();
        }
        if *cancel.borrow() {
            return Err(CaptureError::cancelled(Some(cleanup_target.clone())));
        }
        // The frame request races cancellation too: a cancel never leaves a
        // frame request outstanding, and no image is installed afterwards.
        let frame_cancel = cancel.clone();
        let frame = async {
            let metadata = bus
                .call(&comp, "comp.capture.frame", args, Duration::from_secs(8))
                .await
                .map_err(CaptureError::plain)?;
            if *frame_cancel.borrow() {
                return Err(CaptureError::cancelled(Some(cleanup_target.clone())));
            }
            let source = path.clone();
            let document = crate::worker::run(move || Document::open(&source))
                .await
                .map_err(CaptureError::plain)?;
            Ok(Captured {
                document,
                path,
                metadata,
            })
        };
        tokio::select! {
            biased;
            _ = cancel.changed() => {
                Err(CaptureError::cancelled(Some(cleanup_target.clone())))
            }
            outcome = frame => outcome,
        }
    }
    .await;
    match result {
        Ok(captured) => {
            // The existing restoration obligation on the success path.
            if hidden && let Some(target) = own {
                let restored = bus
                    .call(
                        &comp,
                        "comp.window.restore",
                        json!(target),
                        Duration::from_secs(5),
                    )
                    .await;
                match restored {
                    Ok(reply) if reply["minimized"].as_bool() == Some(false) => {}
                    other => {
                        return Err(CaptureError::plain(format!(
                            "{}; Cap restoration failed: {other:?}",
                            "capture finished"
                        )));
                    }
                }
            }
            if *cancel.borrow() {
                Err(CaptureError::cancelled(Some(cleanup_target.clone())))
            } else {
                Ok(captured)
            }
        }
        Err(error) => {
            if let Some(target) = error.cleanup.clone() {
                // Cancellation cleanup: the owner overlay and Cap's window,
                // submitted in parallel within the bounded budget. Failure is
                // reported truthfully and the exact target retained.
                if let Err(failure) = cleanup(&bus, &comp, &target).await {
                    return Err(CaptureError::cleanup_failed(target, failure));
                }
                Err(error)
            } else {
                // Existing obligation: always restore a window we changed.
                if hidden && let Some(target) = own {
                    let restored = bus
                        .call(
                            &comp,
                            "comp.window.restore",
                            json!(target),
                            Duration::from_secs(5),
                        )
                        .await;
                    match restored {
                        Ok(reply) if reply["minimized"].as_bool() == Some(false) => {}
                        other => {
                            return Err(CaptureError::plain(format!(
                                "{}; Cap restoration failed: {other:?}",
                                error
                            )));
                        }
                    }
                }
                Err(error)
            }
        }
    }
}
pub fn absolute(path: &str) -> Result<PathBuf, String> {
    let p = Path::new(path);
    if p.is_absolute() {
        Ok(p.into())
    } else {
        std::env::current_dir()
            .map(|d| d.join(p))
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn media_root_preserves_legacy_precedence_and_never_moves_existing_captures() {
        let vars = [
            ("MIXOS_APP_HOME", "/one"),
            ("MIXOS_APPS_HOME", "/two"),
            ("MIXOS_VAR", "/three"),
            ("MIXOS", "/four"),
            ("XDG_STATE_HOME", "/five"),
            ("HOME", "/six"),
        ];
        let expected = [
            "/one",
            "/two/cap",
            "/three/apps/cap",
            "/five/mixos/apps/cap",
            "/six/.local/state/mixos/apps/cap",
        ];
        for (offset, expected) in expected.into_iter().enumerate() {
            let base = media_base(|key| {
                vars[offset..]
                    .iter()
                    .find(|(name, _)| *name == key)
                    .map(|(_, path)| PathBuf::from(path))
            })
            .unwrap();
            assert_eq!(base, PathBuf::from(expected));
        }
    }
    #[test]
    fn media_root_ignores_the_generic_mixos_var_and_requires_an_absolute_root() {
        // Only MIXOS is set: the deliberate missing fallback must not invent
        // a capture root under the package var.
        assert!(media_base(|key| (key == "MIXOS").then_some(PathBuf::from("/four"))).is_none());
        assert!(media_base(|_| None).is_none());
        assert!(
            media_base(|key| (key == "MIXOS_APP_HOME").then_some(PathBuf::from("relative")))
                .is_none()
        );
    }
    #[test]
    fn existing_capture_directory_is_secured_and_symlinks_are_refused() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("captures");
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        secure_directory(&directory).unwrap();
        assert_eq!(
            std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let link = root.path().join("linked");
        std::os::unix::fs::symlink(&directory, &link).unwrap();
        assert!(secure_directory(&link).is_err());
        let ordinary_file = root.path().join("file");
        std::fs::write(&ordinary_file, "not a directory").unwrap();
        assert!(secure_directory(&ordinary_file).is_err());
    }
    #[tokio::test]
    async fn activation_reports_compositor_focus_refusal() {
        let (bus, mut effects) = BusHandle::response_sink();
        let driver = tokio::spawn(async move {
            let Some(crate::bus::Effect::Call { verb, reply, .. }) = effects.recv().await else {
                panic!("restore")
            };
            assert_eq!(verb, "comp.window.restore");
            reply.send(Ok(json!({"minimized":false}))).unwrap();
            let Some(crate::bus::Effect::Call { verb, reply, .. }) = effects.recv().await else {
                panic!("focus")
            };
            assert_eq!(verb, "comp.window.focus");
            reply
                .send(Ok(json!({"focused":false,"refused":"exclusive_layer"})))
                .unwrap();
        });
        let error = show(
            &bus,
            "comp.test",
            Target {
                id: 1,
                generation: 2,
            },
        )
        .await
        .unwrap_err();
        assert!(error.contains("exclusive_layer"));
        driver.await.unwrap();
    }
    #[test]
    fn refreshed_window_metadata_preserves_the_selected_identity() {
        let before =
            windows(&json!({"windows":[{"id":3,"generation":9,"title":"Old","focused":true}]}));
        let after = windows(
            &json!({"windows":[{"id":1,"generation":1,"focused":true},{"id":3,"generation":9,"title":"New","minimized":true}]}),
        );
        let selected = selected_window(&after, before.first()).unwrap();
        assert_eq!(selected.target, before[0].target);
        assert_eq!(selected.title, "New");
        assert!(selected.minimized);
        assert!(!selected.focused);
    }
    #[tokio::test]
    async fn lost_minimise_reply_still_restores_the_fenced_window() {
        let (bus, mut effects) = BusHandle::response_sink();
        let target = Target {
            id: 4,
            generation: 9,
        };
        let expected = target.clone();
        let driver = tokio::spawn(async move {
            // The compositor identity is read before hiding.
            let Some(crate::bus::Effect::Call { verb, reply, .. }) = effects.recv().await else {
                panic!("expected info")
            };
            assert_eq!(verb, "comp.info");
            reply.send(Ok(json!({"instance": "itest"}))).unwrap();
            let Some(crate::bus::Effect::Call {
                verb, args, reply, ..
            }) = effects.recv().await
            else {
                panic!("expected minimise")
            };
            assert_eq!(verb, "comp.window.minimize");
            assert_eq!(args, json!(expected));
            reply.send(Err("acknowledgement lost".into())).unwrap();
            let Some(crate::bus::Effect::Call {
                verb, args, reply, ..
            }) = effects.recv().await
            else {
                panic!("expected restore")
            };
            assert_eq!(verb, "comp.window.restore");
            assert_eq!(args, json!(expected));
            reply.send(Ok(json!({"minimized":false}))).unwrap();
        });
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let result = take(
            bus,
            "comp.test".into(),
            Request::default(),
            Some(target),
            PathBuf::from("/tmp"),
            1,
            rx,
        )
        .await;
        assert_eq!(result.unwrap_err().message, "acknowledgement lost");
        driver.await.unwrap();
    }
    #[tokio::test]
    async fn cancellation_during_delay_never_hides_cap() {
        let (bus, mut effects) = BusHandle::response_sink();
        let (tx, rx) = tokio::sync::watch::channel(false);
        let driver = tokio::spawn(async move {
            let Some(crate::bus::Effect::Call { verb, reply, .. }) = effects.recv().await else {
                panic!("expected info")
            };
            assert_eq!(verb, "comp.info");
            let before_delay = std::time::Instant::now();
            reply.send(Ok(json!({"instance": "itest"}))).unwrap();
            let Some(crate::bus::Effect::Delay { when, reply, .. }) = effects.recv().await else {
                panic!("expected delay first")
            };
            assert!(when >= before_delay + Duration::from_secs(10));
            assert!(when <= std::time::Instant::now() + Duration::from_secs(10));
            tx.send(true).unwrap();
            drop(reply);
            // The cancelled screen capture restores the unchanged window.
            let Some(crate::bus::Effect::Call { verb, reply, .. }) = effects.recv().await else {
                panic!("expected restore")
            };
            assert_eq!(verb, "comp.window.restore");
            reply.send(Ok(json!({"minimized":false}))).unwrap();
            assert!(effects.recv().await.is_none());
        });
        let result = take(
            bus,
            "comp.test".into(),
            Request {
                delay: 10,
                ..Default::default()
            },
            Some(Target {
                id: 4,
                generation: 9,
            }),
            PathBuf::from("/tmp"),
            1,
            rx,
        )
        .await;
        assert_eq!(result.unwrap_err().message, "cancelled");
        driver.await.unwrap();
    }
    #[tokio::test]
    async fn cancellation_during_minimise_never_starts_region_selection() {
        let (bus, mut effects) = BusHandle::response_sink();
        let (tx, rx) = tokio::sync::watch::channel(false);
        let driver = tokio::spawn(async move {
            let Some(crate::bus::Effect::Call { verb, reply, .. }) = effects.recv().await else {
                panic!("expected info")
            };
            assert_eq!(verb, "comp.info");
            reply.send(Ok(json!({"instance": "itest"}))).unwrap();
            let Some(crate::bus::Effect::Call { verb, reply, .. }) = effects.recv().await else {
                panic!("minimise")
            };
            assert_eq!(verb, "comp.window.minimize");
            tx.send(true).unwrap();
            drop(reply);
            // The cancelled region capture submits cancel and restore.
            let mut verbs = Vec::new();
            let mut replies = Vec::new();
            for _ in 0..2 {
                let Some(crate::bus::Effect::Call { verb, reply, .. }) = effects.recv().await
                else {
                    panic!("cleanup")
                };
                verbs.push(verb);
                replies.push(reply);
            }
            assert!(verbs.contains(&"comp.region.cancel".to_string()));
            assert!(verbs.contains(&"comp.window.restore".to_string()));
            for reply in replies {
                reply.send(Ok(json!({"minimized":false}))).unwrap();
            }
            assert!(effects.recv().await.is_none());
        });
        let result = take(
            bus,
            "comp.test".into(),
            Request {
                mode: Mode::Region,
                ..Default::default()
            },
            Some(Target {
                id: 1,
                generation: 2,
            }),
            PathBuf::from("/tmp"),
            1,
            rx,
        )
        .await;
        assert_eq!(result.unwrap_err().message, "cancelled");
        driver.await.unwrap();
    }
    #[tokio::test]
    async fn cancellation_during_frame_restores_without_replacing_document() {
        let (bus, mut effects) = BusHandle::response_sink();
        let (tx, rx) = tokio::sync::watch::channel(false);
        let driver = tokio::spawn(async move {
            let Some(crate::bus::Effect::Call { verb, reply, .. }) = effects.recv().await else {
                panic!("expected info")
            };
            assert_eq!(verb, "comp.info");
            reply.send(Ok(json!({"instance": "itest"}))).unwrap();
            let Some(crate::bus::Effect::Call { verb, reply, .. }) = effects.recv().await else {
                panic!("minimise")
            };
            assert_eq!(verb, "comp.window.minimize");
            reply.send(Ok(json!({"minimized":true}))).unwrap();
            let Some(crate::bus::Effect::Call { verb, reply, .. }) = effects.recv().await else {
                panic!("frame")
            };
            assert_eq!(verb, "comp.capture.frame");
            tx.send(true).unwrap();
            drop(reply);
            let Some(crate::bus::Effect::Call { verb, reply, .. }) = effects.recv().await else {
                panic!("restore")
            };
            assert_eq!(verb, "comp.window.restore");
            reply.send(Ok(json!({"minimized":false}))).unwrap();
            assert!(effects.recv().await.is_none());
        });
        let result = take(
            bus,
            "comp.test".into(),
            Request::default(),
            Some(Target {
                id: 1,
                generation: 2,
            }),
            PathBuf::from("/tmp"),
            1,
            rx,
        )
        .await;
        assert_eq!(result.unwrap_err().message, "cancelled");
        driver.await.unwrap();
    }
    #[tokio::test]
    async fn cancellation_during_selection_submits_cancel_and_restore_before_the_held_select() {
        let (bus, mut effects) = BusHandle::response_sink();
        let (tx, rx) = tokio::sync::watch::channel(false);
        let driver = tokio::spawn(async move {
            let Some(crate::bus::Effect::Call { verb, reply, .. }) = effects.recv().await else {
                panic!("expected info")
            };
            assert_eq!(verb, "comp.info");
            reply.send(Ok(json!({"instance": "itest"}))).unwrap();
            let Some(crate::bus::Effect::Call { verb, reply, .. }) = effects.recv().await else {
                panic!("minimise")
            };
            assert_eq!(verb, "comp.window.minimize");
            reply.send(Ok(json!({"minimized":true}))).unwrap();
            let (select_reply, select_args) = match effects.recv().await {
                Some(crate::bus::Effect::Call {
                    verb, args, reply, ..
                }) if verb == "comp.region.select" => (reply, args),
                other => panic!("select: {other:?}"),
            };
            // The explicit bounded identity accompanies the select.
            assert_eq!(select_args["timeout_ms"], 20_000);
            assert_eq!(select_args["selection"]["instance"], "itest");
            assert_eq!(select_args["selection"]["generation"], 7);
            assert!(
                !select_args["selection"]["owner"]
                    .as_str()
                    .unwrap()
                    .is_empty()
            );
            // Cancel while the selection is held: both cleanup calls arrive
            // before the held select completes.
            tx.send(true).unwrap();
            tx.send(true).unwrap(); // duplicate cancel is a no-op
            let mut verbs = Vec::new();
            let mut replies = Vec::new();
            for _ in 0..2 {
                let Some(crate::bus::Effect::Call { verb, reply, .. }) = effects.recv().await
                else {
                    panic!("cleanup")
                };
                verbs.push(verb);
                replies.push(reply);
            }
            assert!(verbs.contains(&"comp.region.cancel".to_string()));
            assert!(verbs.contains(&"comp.window.restore".to_string()));
            for reply in replies {
                reply.send(Ok(json!({"minimized":false}))).unwrap();
            }
            assert!(
                effects.recv().await.is_none(),
                "a duplicate cancel must not repeat the cleanup"
            );
            // The held select completes late; its receiver is already gone.
            let _ = select_reply.send(Ok(json!({"status": "cancelled"})));
        });
        let result = take(
            bus,
            "comp.test".into(),
            Request {
                mode: Mode::Region,
                ..Default::default()
            },
            Some(Target {
                id: 1,
                generation: 2,
            }),
            PathBuf::from("/tmp"),
            7,
            rx,
        )
        .await;
        assert_eq!(result.unwrap_err().message, "cancelled");
        driver.await.unwrap();
    }
    #[tokio::test]
    async fn failed_cleanup_reports_the_failure_and_retains_the_exact_target() {
        let (bus, mut effects) = BusHandle::response_sink();
        let (tx, rx) = tokio::sync::watch::channel(false);
        let driver = tokio::spawn(async move {
            let Some(crate::bus::Effect::Call { verb, reply, .. }) = effects.recv().await else {
                panic!("expected info")
            };
            assert_eq!(verb, "comp.info");
            reply.send(Ok(json!({"instance": "itest"}))).unwrap();
            let Some(crate::bus::Effect::Call { verb, reply, .. }) = effects.recv().await else {
                panic!("minimise")
            };
            assert_eq!(verb, "comp.window.minimize");
            reply.send(Ok(json!({"minimized":true}))).unwrap();
            let Some(crate::bus::Effect::Call { verb, reply, .. }) = effects.recv().await else {
                panic!("select")
            };
            assert_eq!(verb, "comp.region.select");
            let _hold = reply;
            tx.send(true).unwrap();
            let mut verbs = Vec::new();
            let mut replies = Vec::new();
            for _ in 0..2 {
                let Some(crate::bus::Effect::Call { verb, reply, .. }) = effects.recv().await
                else {
                    panic!("cleanup")
                };
                verbs.push(verb);
                replies.push(reply);
            }
            assert!(verbs.contains(&"comp.region.cancel".to_string()));
            assert!(verbs.contains(&"comp.window.restore".to_string()));
            for reply in replies {
                reply.send(Err("broker gone".into())).unwrap();
            }
        });
        let result = take(
            bus,
            "comp.test".into(),
            Request {
                mode: Mode::Region,
                ..Default::default()
            },
            Some(Target {
                id: 1,
                generation: 2,
            }),
            PathBuf::from("/tmp"),
            3,
            rx,
        )
        .await;
        let error = result.unwrap_err();
        assert!(
            error.message.contains("cancelled; cleanup failed"),
            "{}",
            error.message
        );
        let cleanup = error.cleanup.expect("the exact target is retained");
        assert_eq!(cleanup.selection.unwrap().generation, 3);
        assert_eq!(
            cleanup.window,
            Some(Target {
                id: 1,
                generation: 2
            })
        );
        driver.await.unwrap();
    }
    #[test]
    fn invalid_requests_are_refused() {
        assert!(
            Request {
                delay: 11,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            Request {
                mode: Mode::Window,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(serde_json::from_value::<Request>(json!({"mode":"screen","extra":true})).is_err());
    }
    #[test]
    fn window_identity_is_never_guessed() {
        assert!(windows(&json!({"windows":[{"id":1}]})).is_empty());
        let rows =
            windows(&json!({"windows":[{"id":3,"generation":9,"title":"Editor","focused":true}]}));
        assert_eq!(
            rows[0].target,
            Target {
                id: 3,
                generation: 9
            }
        );
    }
}
