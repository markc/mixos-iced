//! The Mix Scenes host (scene-host) on compd's loop.
//!
//! Opt-in (decision S2): started only when the `scene_host` preference is
//! on. Its Bus worker wakes the loop through a calloop ping, like the comp
//! port; `service` drains it from the post-dispatch closure and does nothing
//! unless the ping fired. The scenes' surfaces are kept in step by
//! `scene_host::per_frame`, registered as a frames frame hook (frames
//! sits below scene-host and cannot name it).

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use world::state::Loop;
use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::calloop::ping::make_ping;

pub struct Scenes {
    pending: Rc<Cell<bool>>,
}

impl Scenes {
    /// `None` when `scene_host` is off. The fallback name is
    /// `--scene-service`, else the `scene_service` preference; the noded URL
    /// is the comp port's (`MIXOS_NODED_URL`, else the loopback broker).
    pub fn start(lp: &Loop, cli_service: Option<&str>, handle: &LoopHandle<'static, Loop>) -> Option<Self> {
        let preference = &lp.inner.preference;
        if !preference.scene_host {
            model::info!("Mix Scenes host off (preference scene_host = false)");
            return None;
        }
        let config = scene_host::HostConfig {
            owner_version: env!("CARGO_PKG_VERSION").into(),
            service_override: cli_service.map(str::to_owned).or_else(|| preference.scene_service.clone()),
            noded_url: comp_service::default_noded_url(),
        };
        let (ping, source) = match make_ping() {
            Ok(pair) => pair,
            Err(error) => {
                model::warn!("Mix Scenes host not started: wake source: {error}");
                return None;
            }
        };
        let pending = Rc::new(Cell::new(false));
        let flag = Rc::clone(&pending);
        if let Err(error) = handle.insert_source(source, move |_, _, _| flag.set(true)) {
            model::warn!("Mix Scenes host not started: wake source: {error}");
            return None;
        }
        let waker: scene_host::Waker = Arc::new(move || ping.ping());
        if let Err(error) = scene_host::start(config, waker) {
            model::warn!("Mix Scenes host not started: {error}");
            return None;
        }
        frames::scene::hooks::register_frame_hook(scene_host::per_frame);
        world::comp::scenes::register_pointer_hook(scene_host::pointer_motion);
        world::comp::scenes::register_button_hook(scene_host::pointer_button);
        Some(Self { pending })
    }

    /// Answer what the worker and the surfaces delivered (a frame is
    /// scheduled, `RedrawReason::Publish`, when a scene changed). A no-op
    /// unless the ping fired.
    pub fn service(&mut self, lp: &mut Loop) {
        if self.pending.replace(false) {
            scene_host::service(lp);
        }
    }

    pub fn shutdown(self) {
        scene_host::shutdown();
    }
}
