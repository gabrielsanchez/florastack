#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The adjustments as data: their JSON, their defaults and their checks; and that any adjustments the checks accept
//! render without a panic.

use florastack_core::{ColorSpace, Image};
use florastack_edit::{Adjustments, Component, Focus, Global, Local, Mask, Op, Renderer, Request, Shape, Size, Stroke};
use proptest::prelude::*;
use serde_json::json;

#[test]
fn defaults_are_neutral_and_partial_json_takes_them() {
    assert!(Adjustments::default().is_neutral());
    let a: Adjustments = serde_json::from_value(json!({"global": {"exposure": 0.5}})).unwrap();
    assert_eq!(a.global, Global { exposure: 0.5, ..Global::default() });
    assert!(!a.is_neutral());
    // Sharpening's radius alone sharpens nothing.
    let a: Adjustments = serde_json::from_value(json!({"global": {"sharpen_radius": 2.0}})).unwrap();
    assert!(a.is_neutral());
    // A hidden mask, or one whose adjustments are neutral, changes nothing.
    let a: Adjustments = serde_json::from_value(json!({"masks": [
        {"id": 1, "components": [{"shape": {"kind": "whole"}}], "adjust": {"exposure": 1.0}, "visible": false},
        {"id": 2, "components": [{"shape": {"kind": "subject"}}]}
    ]}))
    .unwrap();
    assert!(a.is_neutral());
    // The visible mask's subject still needs the focus: its overlay shows it.
    assert!(a.needs_focus());
}

#[test]
fn json_round_trips_with_every_shape() {
    let shapes = vec![
        Shape::Whole,
        Shape::Linear { start: [0.0, 0.25], end: [0.0, 0.75] },
        Shape::Radial { center: [0.5, 0.5], radius_x: 0.2, radius_y: 0.1, angle: 30.0, feather: 50.0 },
        Shape::Brush { strokes: vec![Stroke { points: vec![[0.1, 0.1], [0.2, 0.25]], erase: true, ..Stroke::default() }] },
        Shape::Luminance { low: 0.2, high: 0.8, low_feather: 0.1, high_feather: 0.1 },
        Shape::Color { samples: vec![[0.6, 0.1, -0.05]], refine: 40.0 },
        Shape::Subject,
        Shape::Background,
        Shape::Depth { near: 0.25, far: 0.5, feather: 0.125 },
    ];
    let a = Adjustments {
        global: Global { exposure: 0.25, temperature: -10.0, vibrance: 20.0, ..Global::default() },
        masks: vec![Mask {
            id: 3,
            name: "Wing".into(),
            refine: 25.0,
            components: shapes.into_iter().map(|shape| Component { op: Op::Intersect, invert: true, shape }).collect(),
            adjust: Local { clarity: 30.0, amount: 75.0, ..Local::default() },
            ..Mask::default()
        }],
    };
    a.validate().unwrap();
    let value = serde_json::to_value(&a).unwrap();
    assert_eq!(value["masks"][0]["components"][8]["shape"], json!({"kind": "depth", "near": 0.25, "far": 0.5, "feather": 0.125}));
    assert_eq!(serde_json::from_value::<Adjustments>(value).unwrap(), a);
}

#[test]
fn unknown_fields_are_refused() {
    for bad in [
        json!({"global": {"exposur": 1.0}}),
        json!({"glob": {}}),
        json!({"masks": [{"id": 1, "components": [{"shape": {"kind": "radial", "center": [0.5, 0.5], "radius": 2}}]}]}),
        json!({"masks": [{"id": 1, "components": [{"shape": {"kind": "sky"}}]}]}),
    ] {
        assert!(serde_json::from_value::<Adjustments>(bad.clone()).is_err(), "{bad}");
    }
}

#[test]
fn new_mask_ids_are_never_reused() {
    let mut a = Adjustments::default();
    assert_eq!(a.next_mask_id(), 1);
    a.masks.push(Mask { id: 7, ..Mask::default() });
    a.masks.push(Mask { id: 2, ..Mask::default() });
    assert_eq!(a.next_mask_id(), 8);
    assert_eq!(a.mask(2).map(|m| m.id), Some(2));
    a.mask_mut(7).unwrap().name = "x".into();
    assert_eq!(a.mask(7).unwrap().name, "x");
}

#[test]
fn sizes_are_bounded() {
    let a = Adjustments { masks: (0..65).map(|id| Mask { id, ..Mask::default() }).collect(), ..Adjustments::default() };
    assert!(a.validate().is_err());
    let mut a = Adjustments::default();
    a.masks.push(Mask { id: 1, name: "n".repeat(65), ..Mask::default() });
    assert!(a.validate().is_err());
    let many = Stroke { points: vec![[0.5, 0.5]; 600_000], ..Stroke::default() };
    let mut a = Adjustments::default();
    a.masks.push(Mask {
        id: 1,
        components: vec![Component { op: Op::Add, invert: false, shape: Shape::Brush { strokes: vec![many.clone(), many] } }],
        ..Mask::default()
    });
    assert!(a.validate().is_err(), "more than a million points");
}

fn finite(lo: f64, hi: f64) -> impl Strategy<Value = f64> {
    lo..=hi
}

fn point() -> impl Strategy<Value = [f64; 2]> {
    [finite(-1.0, 2.0), finite(-1.0, 2.0)]
}

fn shape() -> impl Strategy<Value = Shape> {
    prop_oneof![
        Just(Shape::Whole),
        (point(), point()).prop_map(|(start, end)| Shape::Linear { start, end }),
        (point(), finite(0.0, 2.0), finite(0.0, 2.0), finite(-360.0, 360.0), finite(0.0, 100.0))
            .prop_map(|(center, radius_x, radius_y, angle, feather)| { Shape::Radial { center, radius_x, radius_y, angle, feather } }),
        (proptest::collection::vec(point(), 0..6), finite(0.001, 0.3), finite(0.0, 100.0), finite(0.0, 100.0), any::<bool>())
            .prop_map(|(points, size, feather, flow, erase)| Shape::Brush { strokes: vec![Stroke { points, size, feather, flow, erase }] }),
        (finite(0.0, 0.5), finite(0.5, 1.0), finite(0.0, 1.0), finite(0.0, 1.0))
            .prop_map(|(low, high, low_feather, high_feather)| Shape::Luminance { low, high, low_feather, high_feather }),
        ([finite(0.0, 1.0), finite(-0.4, 0.4), finite(-0.4, 0.4)], finite(0.0, 100.0))
            .prop_map(|(c, refine)| Shape::Color { samples: vec![c], refine }),
        Just(Shape::Subject),
        Just(Shape::Background),
        (finite(0.0, 0.5), finite(0.5, 1.0), finite(0.0, 1.0)).prop_map(|(near, far, feather)| Shape::Depth { near, far, feather }),
    ]
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![Just(Op::Add), Just(Op::Subtract), Just(Op::Intersect)]
}

fn local() -> impl Strategy<Value = Local> {
    (finite(-100.0, 100.0), finite(-5.0, 5.0), finite(-100.0, 100.0), finite(-100.0, 100.0), finite(-100.0, 100.0), finite(0.0, 100.0)).prop_map(
        |(temperature, exposure, contrast, clarity, noise, amount)| Local {
            temperature,
            exposure,
            contrast,
            clarity,
            noise,
            amount,
            ..Local::default()
        },
    )
}

fn global() -> impl Strategy<Value = Global> {
    (finite(-100.0, 100.0), finite(-5.0, 5.0), finite(-100.0, 100.0), finite(-100.0, 100.0), finite(0.0, 150.0), finite(0.0, 100.0)).prop_map(
        |(tint, exposure, highlights, dehaze, sharpening, noise_reduction)| Global {
            tint,
            exposure,
            highlights,
            dehaze,
            sharpening,
            noise_reduction,
            ..Global::default()
        },
    )
}

fn adjustments() -> impl Strategy<Value = Adjustments> {
    let mask = (proptest::collection::vec((op(), any::<bool>(), shape()), 0..4), local(), any::<bool>(), finite(0.0, 100.0));
    (global(), proptest::collection::vec(mask, 0..3)).prop_map(|(global, masks)| Adjustments {
        global,
        masks: masks
            .into_iter()
            .enumerate()
            .map(|(i, (components, adjust, invert, refine))| Mask {
                id: u32::try_from(i).unwrap() + 1,
                invert,
                refine,
                components: components.into_iter().map(|(op, invert, shape)| Component { op, invert, shape }).collect(),
                adjust,
                ..Mask::default()
            })
            .collect(),
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    /// Whatever the checks accept renders, at full size and reduced, to finite samples of the right size.
    #[test]
    fn accepted_adjustments_render(a in adjustments(), w in 1usize..40, h in 1usize..40) {
        let image = Image::from_fn(w, h, 3, ColorSpace::LINEAR, |x, y, c| ((x * 7 + y * 13 + c * 5) % 17) as f32 / 16.0).unwrap();
        let maps = Image::from_fn(w, h, 1, ColorSpace::LINEAR, |x, _, _| x as f32 / w as f32).unwrap();
        let r = Renderer::new(&image).unwrap().with_focus(&Focus { subject: maps.clone(), depth: maps }).unwrap();
        prop_assume!(a.validate().is_ok());
        for request in [Request::FULL, Request { size: Size::Fit { width: 16, height: 16 }, draft: true, overlay: Some(1) }] {
            let out = r.render(&a, &request).unwrap();
            let (ew, eh) = r.output_size(request.size);
            prop_assert_eq!((out.width(), out.height()), (ew, eh));
            prop_assert!(out.as_raw().iter().all(|v| v.is_finite()));
        }
    }
}
