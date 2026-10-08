//! Bounded native desktop worlds through the existing finite engine control lane.
use comp_model::reply::ControlReply;
use dispatcher::state::state::RedrawReason;
use dispatcher::wire::trait_::surface_event::SurfaceHandle;
use serde_json::json;
use world::camera::transform::translate::slot;
use world::state::Loop;

pub fn list(lp: &Loop) -> ControlReply {
    let locked = world::comp::session_lock::active(lp);
    let worlds: Vec<_> = lp.inner.worlds.ids().into_iter().map(|id| {
        let native = lp.inner.worlds.get(id);
        let windows: Vec<_> = if locked { Vec::new() } else {
            lp.inner.space_of(id).state.elements().filter_map(|window| {
                let handle = SurfaceHandle::of_window(window)?;
                let record = lp.inner.comp.registry.surface_rows().find(|record| record.handle() == &handle)?;
                let location = lp.inner.space_of(id).state.element_location(window)?;
                let target = policy::tiling::Target { id: record.id(), generation: record.generation() };
                Some(json!({"id":record.id().0,"generation":record.generation(),"uuid":record.uuid(),
                    "pid":record.pid(),"mapped":record.mapped(),"minimized":record.minimized(),
                    "local":{"x":location.x,"y":location.y},
                    "decided":slot::decided_size(window).map(|size|[size.w,size.h]),
                    "tile_normal":lp.inner.comp.tiles.member(target).map(|member|json!({"x":member.normal.x,"y":member.normal.y,"width":member.normal.width,"height":member.normal.height}))}))
            }).collect()
        };
        json!({"id":id,"name":native.name,"active":id == lp.inner.worlds.active_id(),
            "spawn_target":id == lp.inner.worlds.spawn_target(),"windows":windows})
    }).collect();
    ControlReply::Body(
        json!({"active":lp.inner.worlds.active_id(),"spawn_target":lp.inner.worlds.spawn_target(),
        "limit":world::world::manager::manager::MAX_DESKTOP_WORLDS,"worlds":worlds}),
    )
}

pub fn create(lp: &mut Loop) -> ControlReply {
    let Some(id) = lp.inner.create_desktop_world() else {
        return ControlReply::refused(
            "world_capacity",
            json!({"limit":world::world::manager::manager::MAX_DESKTOP_WORLDS}),
        );
    };
    lp.inner.comp.settings_changed("windows", "world.create");
    ControlReply::Body(json!({"id":id,"created":true,"active":false,"spawn_target":false}))
}

pub fn activate(lp: &mut Loop, id: uuid::Uuid) -> ControlReply {
    if !lp.inner.worlds.contains(id) {
        return ControlReply::refused("unknown_world", json!({"id":id}));
    }
    if lp
        .inner
        .worlds
        .get(id)
        .storage()
        .try_get(&world::host::space::base::SPACE)
        .is_none()
    {
        return ControlReply::refused("unsupported_state", json!({"id":id,"reason":"not_spatial"}));
    }
    if crate::input::exclusive_layer(lp)
        || crate::input::human_keyboard_owned(&lp.state.seat.seat)
        || lp.inner.comp.interactive.is_some()
        || lp.inner.comp.region.run.is_some()
        || lp
            .state
            .seat
            .seat
            .get_pointer()
            .is_some_and(|pointer| pointer.is_grabbed())
    {
        return ControlReply::refused("busy", json!({"id":id,"reason":"seat_owned"}));
    }
    let changed = lp.inner.worlds.active_id() != id || lp.inner.worlds.spawn_target() != id;
    if changed {
        lp.inner.switch_to_world(id);
        lp.inner.comp.settings_changed("windows", "world.activate");
        crate::control::refresh_usable(lp);
        lp.state.schedule_redraw(RedrawReason::WindowState);
    }
    ControlReply::Body(
        json!({"id":id,"changed":changed,"active":lp.inner.worlds.active_id(),"spawn_target":lp.inner.worlds.spawn_target()}),
    )
}
