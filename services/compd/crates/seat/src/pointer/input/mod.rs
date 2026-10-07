pub mod axis;
pub mod button;
pub mod chrome;
pub mod constraint;
pub mod extent;
pub mod motion;
pub mod native_axis;
pub mod native_motion;
pub mod native_press;
pub mod pinch;
pub mod tablet;
pub mod touch;

/// Reconcile the stationary human pointer through its existing routing owner.
/// No accumulator, velocity, corner sample or relative delta is manufactured.
/// False retains a pending geometry notification until input can resume.
pub fn retarget_stationary(lp: &mut world::state::Loop) -> bool {
    use dispatcher::wire::trait_::wire_trait::WireTrait;
    use world::state::state::CoordinateTrait;
    if matches!(lp.inner.status_session, world::state::state::StatusSession::Paused)
        || world::comp::session_lock::active(lp)
        || WireTrait::active_output(&lp.inner).is_none()
        || lp.inner.host_space().state.outputs().next().is_none()
        || constraint::constraint_active(lp)
        || lp.inner.surface().registry.as_ref().is_some_and(|registry| registry.pointer_grab().is_some())
    {
        return false;
    }
    let Some(pointer) = lp.state.seat.seat.get_pointer() else { return false };
    if pointer.is_grabbed() {
        return false;
    }
    let location = pointer.current_location();
    let time = lp.inner.start_time.elapsed().as_millis() as u32;
    native_motion::dispatch::dispatch(
        lp, time, smithay::utils::SERIAL_COUNTER.next_serial(), pointer,
        location, None, false,
    );
    true
}
