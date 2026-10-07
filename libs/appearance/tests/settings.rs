// SPDX-License-Identifier: MIT OR Apache-2.0
use appearance::settings::Projection;
use settings::{Desktop, Diagnostic};
use toolkit::fonts::{FontChoice, FontSelection};

fn effective() -> settings::Effective {
    settings::resolve(&Desktop::default())
        .unwrap()
        .remove("app:ced")
        .unwrap()
}
fn checked_font(_: &str, _: &design::ResolvedTypeRecord) -> Result<FontSelection, Diagnostic> {
    Ok(FontSelection {
        font: iced_core::Font::DEFAULT,
        choice: FontChoice::Declared,
    })
}
#[test]
fn projection_reuses_compiler_mapping_and_prepares_every_type() {
    let e = effective();
    let projection = Projection::new(&e).unwrap();
    let theme = appearance::Theme::embedded();
    assert_eq!(projection.tokens(), appearance::tokens(&theme));
    let mut names = Vec::new();
    let ready = projection
        .prepare(|name, r| {
            names.push(name.to_owned());
            checked_font(name, r)
        })
        .unwrap();
    assert_eq!(
        names,
        e.design.typography.keys().cloned().collect::<Vec<_>>()
    );
    assert_eq!(
        ready.theme(),
        toolkit::Theme::new(appearance::tokens(&theme)).with_semantic(appearance::semantic(&theme))
    );
    assert_eq!(
        ready.typography().get("ui").unwrap().size,
        e.design.typography["ui"].font_size as f32
    );
    assert!(
        ready
            .font_choices()
            .values()
            .all(|c| *c == FontChoice::Declared)
    );
}
#[test]
fn common_preferences_change_only_their_shared_defaults() {
    let mut e = effective();
    let before = Projection::new(&e).unwrap().prepare(checked_font).unwrap();
    e.ui.density = 0.5;
    e.ui.text_scale = 1.5;
    e.ui.reduced_motion = true;
    e.design.typography.get_mut("ui").unwrap().line_height = Some(20.0);
    let after = Projection::new(&e).unwrap().prepare(checked_font).unwrap();
    assert_eq!(after.tokens().palette, before.tokens().palette);
    assert_eq!(
        after.tokens().metrics.spacing.md,
        before.tokens().metrics.spacing.md * 0.5
    );
    assert_eq!(
        after.tokens().metrics.text.md,
        before.tokens().metrics.text.md * 1.5
    );
    assert_eq!(
        after.typography().get("ui").unwrap().line_height,
        Some(30.0)
    );
    assert_eq!(
        after.tokens().metrics.radius,
        before.tokens().metrics.radius
    );
    assert!(after.reduced_motion());
}

#[test]
fn every_read_button_key_survives_wire_order_and_uses_prepared_typography() {
    let mut effective = effective();
    effective.design.buttons.reverse();
    effective.ui.text_scale = 1.5;
    effective.ui.density = 0.75;
    let prepared = Projection::new(&effective)
        .unwrap()
        .prepare(checked_font)
        .unwrap();
    for variant in design::ButtonVariant::ALL {
        for size in design::ButtonSize::ALL {
            for interaction in design::InteractionState::ALL {
                for focus_visible in [false, true] {
                    let key = design::ButtonCellKey {
                        variant,
                        size,
                        interaction,
                        focus_visible,
                    };
                    let expected = effective
                        .design
                        .buttons
                        .iter()
                        .find(|cell| {
                            cell.variant == variant.name()
                                && cell.size == size.name()
                                && cell.interaction == interaction.name()
                                && cell.focus_visible == focus_visible
                        })
                        .unwrap();
                    assert_eq!(prepared.button(key), expected);
                    for part in design::ButtonPart::ALL {
                        assert_eq!(
                            prepared.button_text(key, part).size,
                            effective.design.typography[&expected.typography[part.name()]].font_size
                                as f32
                                * 1.5
                        );
                    }
                }
            }
        }
    }
    assert_eq!(prepared.density(), 0.75);
    let duplicate = effective.design.buttons[0].clone();
    effective.design.buttons[1] = duplicate;
    assert!(Projection::new(&effective).is_err());
}
#[test]
fn partial_wrong_unit_nonfinite_and_future_projection_fail_visibly() {
    let cases: Vec<fn(&mut settings::Effective)> = vec![
        |e| e.design.schema = 2,
        |e| {
            e.design.pairs.remove("base");
        },
        |e| e.design.pairs.get_mut("base").unwrap().rendered_surface[0] = f64::NAN,
        |e| e.design.metrics.get_mut("radius").unwrap().kind = "ratio".into(),
        |e| {
            e.design.typography.remove("ui");
        },
        |e| e.design.typography.get_mut("mono").unwrap().generic = "unknown".into(),
        |e| e.design.typography.get_mut("ui").unwrap().font_size = f64::MAX,
        |e| {
            e.design.scales.get_mut("spacing").unwrap().truncate(9);
        },
        |e| e.ui.density = 0.0,
        |e| e.ui.text_scale = f64::INFINITY,
        |e| e.design.metrics.get_mut("type.compact").unwrap().value = 0.0,
        |e| {
            e.ui.text_scale = 3.0;
            e.design.metrics.get_mut("type.compact").unwrap().value = 4096.0;
        },
        |e| {
            e.design.buttons[0]
                .typography
                .insert("label".into(), "missing".into());
        },
        |e| {
            e.design.buttons[1] = e.design.buttons[0].clone();
        },
        |e| e.design.buttons[0].interaction = "unknown".into(),
        |e| {
            e.design.buttons[0].typography.remove("label");
        },
        |e| {
            e.design.buttons.pop();
        },
    ];
    for mutate in cases {
        let mut e = effective();
        mutate(&mut e);
        assert!(Projection::new(&e).is_err());
    }
}
#[test]
fn any_font_failure_prevents_a_usable_stage() {
    let projection = Projection::new(&effective()).unwrap();
    assert!(
        projection
            .prepare(|name, r| {
                if name == "mono" {
                    Err(Diagnostic::new("font_unavailable", name, "missing font"))
                } else {
                    checked_font(name, r)
                }
            })
            .is_err()
    );
}
#[test]
fn app_and_shell_use_the_same_mapper_for_matching_inputs() {
    let resolved = settings::resolve(&Desktop::default()).unwrap();
    let app = Projection::new(&resolved["app:ced"])
        .unwrap()
        .prepare(checked_font)
        .unwrap();
    let shell = Projection::new(&resolved["desktop"])
        .unwrap()
        .prepare(checked_font)
        .unwrap();
    assert_eq!(app.tokens(), shell.tokens());
    assert_eq!(app.typography(), shell.typography());
    assert_eq!(app.theme(), shell.theme());
}
