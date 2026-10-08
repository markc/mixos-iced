# Live output scale — compd 0.1.20

`comp.output.scale {output, instance, generation, scale}` is a finite native
ABP request. Read the exact name, allocation UUID and positive topology
generation from `comp.props.get {path:"outputs"}` immediately before admission.
Scale is a finite JSON number in 0.5..4.0. Extra or missing fields are refused.
The reply contains `output`, `instance`, `generation`, actual `scale` and
`changed`. A no-op preserves the generation and schedules no redraw.

An old allocation or topology fence returns `stale_output`. Missing/ambiguous
outputs, missing modes/geometry or a paused session return `output_unavailable`.
A session lock refuses the mutation. Generation exhaustion refuses a change.
Refusals never mutate output scale. The UUID belongs to the actual Smithay
Output allocation: same-name replacement and compositor restart cannot reuse it.

The owner changes only Smithay Output scale, preserving its physical mode,
transform and location. Existing geometry owners refresh each world's usable
areas and maximised/fullscreen/tiled placements; ordinary windows keep logical
state. Existing frame owners aggregate the sharpest scale across outputs,
publish actual fractional-scale events and decided configure sizes, and redraw
output damage. Explicit scale changes bypass the animation debounce once on
the next actual frame, preserving its aggregation and deduplication.

This contract does not change modes, add/remove connectors, select a KMS CRTC
or switch VTs. Those remain native lifecycle responsibilities. Nested native
Wayland protocol/buffer evidence does not prove physical KMS or hotplug.

`tests/desktop/settings_output_scale_lib.mix` extends the real simultaneous
application fixture with an opt-in 1.25 transition and restoration, retained
product state, same PID/window generation, actual captured Ced buffer boundary,
fresh actual fractional protocol events, actual retained Quoin button callbacks
under fractional scale, and strict stale/invalid/no-op checks. The observer
draws at logical size, so the fixture does not claim its physical buffer ratio.
Native inputs run through the existing seat and citizen; no control relay is
introduced. Rust owner/parser
guards and the fixture require compilation and native execution before release
acceptance; source inspection or Mix lint is insufficient.
