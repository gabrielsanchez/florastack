//! The apps depend on florastack-engine alone (AGENTS.md, Layering rules), so it re-exports what they use: this test
//! names each of those types through the engine only, as an app would.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::any::type_name;

use florastack_engine::model::{AlignedFrame, AlignmentRun, Aspects, FolderTrash, FrameId, Group, OutputId, Project, SystemTrash};
use florastack_engine::{
    AlignSettings, AnyImage, AverageSettings, BitDepth, ColorSpace, CommandError, DevelopSettings, DispatchOptions, Dispatched, End, Engine,
    EngineConfig, Event, ExifSummary, Flag, Format, Framing, Image, InlineExecutor, JobState, Kind, Lane, ManualClock, Orientation,
    OrientationPolicy, OutputFormat, OutputSpec, ReadOptions, Registry, StackSettings, Term, TiffCompression, Value, WhiteBalance, branding, json,
};

#[test]
fn an_app_finds_what_it_needs_in_the_engine() {
    let names = [
        type_name::<FrameId>(),
        type_name::<OutputId>(),
        type_name::<Group>(),
        type_name::<Aspects>(),
        type_name::<Project>(),
        type_name::<SystemTrash>(),
        type_name::<BitDepth>(),
        type_name::<OutputFormat>(),
        type_name::<TiffCompression>(),
        type_name::<ExifSummary>(),
        type_name::<ReadOptions>(),
        type_name::<OrientationPolicy>(),
        type_name::<Orientation>(),
        type_name::<Event>(),
        type_name::<JobState>(),
        type_name::<Kind>(),
        type_name::<Lane>(),
        type_name::<Dispatched>(),
        type_name::<CommandError>(),
        type_name::<DispatchOptions>(),
        type_name::<AlignSettings>(),
        type_name::<Framing>(),
        type_name::<Flag>(),
        type_name::<End>(),
        type_name::<Term>(),
        type_name::<AlignmentRun>(),
        type_name::<AlignedFrame>(),
    ];
    assert!(names.iter().all(|n| n.starts_with("florastack_")), "{names:?}");
    let image = Image::from_fn(2, 2, 1, ColorSpace::LINEAR, |_, _, _| 0.5_f32).unwrap();
    let _ = AnyImage::F32(image);
    assert_eq!(DevelopSettings::default().white_balance, WhiteBalance::AsShot);
    assert_eq!(StackSettings::default().average, AverageSettings::default());
    let spec = OutputSpec::new(Format::Tiff).unwrap();
    assert_eq!(spec.depth, BitDepth::U16);
    assert_ne!(branding::DISPLAY_NAME, "");
    let dir = std::env::temp_dir().join(format!("florastack-engine-reexports-{}", std::process::id()));
    let config = EngineConfig {
        temporary_projects: Some(dir),
        trash: std::sync::Arc::new(FolderTrash::new(std::env::temp_dir())),
        executor: std::sync::Arc::new(InlineExecutor),
        clock: std::sync::Arc::new(ManualClock::new()),
        waker: None,
        max_heavy_jobs: 1,
        max_preview_jobs: 1,
    };
    let engine = Engine::with_registry(config, Registry::standard());
    assert!(engine.registry().get("project.new").is_some());
    let _: Value = json!({});
}
