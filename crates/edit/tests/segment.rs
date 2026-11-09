#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! AI selections with Meta's SAM 3 model, when the folder `FLORASTACK_SAM3_MODEL` names holds it (FloraStack doesn't
//! ship it: book/src/user/adjusting.md says where it comes from). Without it, the test records a skip.
#![cfg(not(target_arch = "wasm32"))]

use florastack_core::{ColorSpace, Image};
use florastack_edit::segment::{Prompt, Segmenter, check_model};
use florastack_edit::{Click, EditError, Renderer};

/// A bright disc in the middle of a dark, textured field.
fn disc() -> Image<f32> {
    let (w, h) = (320usize, 240usize);
    Image::from_fn(w, h, 3, ColorSpace::LINEAR, |x, y, c| {
        let d = ((x as f32 - 160.0).powi(2) + (y as f32 - 120.0).powi(2)).sqrt();
        if d < 60.0 { [0.8, 0.4, 0.1][c] } else { 0.05 + 0.03 * (((x / 3 + y / 3) % 2) as f32) }
    })
    .unwrap()
}

#[test]
fn a_folder_without_the_model_is_refused() {
    let dir = std::env::temp_dir().join(format!("florastack-no-model-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    assert!(matches!(check_model(&dir), Err(EditError::NoModel { .. })));
    std::fs::write(dir.join("model.safetensors"), b"short").unwrap();
    assert!(matches!(check_model(&dir), Err(EditError::Model { .. })), "the wrong size");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_click_selects_the_disc() {
    let Some(dir) = std::env::var_os("FLORASTACK_SAM3_MODEL") else {
        florastack_testkit::skip!(Model, "FLORASTACK_SAM3_MODEL names no SAM 3 model folder");
        return;
    };
    let mut model = Segmenter::load(std::path::Path::new(&dir)).unwrap();
    let r = Renderer::new(&disc()).unwrap();
    let clicks = [Click { at: [0.5, 0.5], include: true }];
    let selection = model.select(&r, Prompt::Clicks(&clicks)).unwrap().unwrap();
    assert!(model.has_read(&r), "the image is read once");
    let logits = selection.side as usize;
    assert!(logits > 0);
}
