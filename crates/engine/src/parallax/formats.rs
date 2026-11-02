//! Original writers following the Khronos glTF 2.0 specification, sections 3.6–3.9 and 4:
//! <https://registry.khronos.org/glTF/specs/2.0/glTF-2.0.html>.

use super::{Scene, invalid};
use crate::{CommandError, json};
use florastack_codecs::{OutputMetadata, OutputSpec};
use florastack_core::{AnyImage, ColorSpace, Image};

pub(crate) fn png(scene: &Scene) -> Result<Vec<u8>, CommandError> {
    let image = Image::from_vec(scene.width, scene.height, 4, ColorSpace::SRGB, scene.rgba.clone()).map_err(florastack_stack::StackError::from)?;
    // glTF defines base-color samples as sRGB; ICC chunks are not supported by every glTF reader.
    encode(image, florastack_codecs::OutputProfile::Untagged)
}

pub(crate) fn encode_png(image: Image<u8>) -> Result<Vec<u8>, CommandError> {
    encode(image, florastack_codecs::OutputProfile::Srgb)
}

fn encode(image: Image<u8>, profile: florastack_codecs::OutputProfile) -> Result<Vec<u8>, CommandError> {
    let spec = OutputSpec { format: florastack_codecs::OutputFormat::Png, depth: florastack_codecs::BitDepth::U8, profile, extended_range: false };
    Ok(florastack_codecs::write(&AnyImage::U8(image), &spec, &OutputMetadata::default())?.bytes)
}

fn u32_len(n: usize) -> Result<u32, CommandError> {
    u32::try_from(n).map_err(|_| invalid("mesh exceeds the GLB 4 GiB container limit"))
}

/// Self-contained binary glTF with a PNG base-color texture and an unlit, double-sided material.
pub fn glb(scene: &Scene) -> Result<Vec<u8>, CommandError> {
    scene.validate()?;
    let mut binary = Vec::new();
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for vertex in &scene.vertices {
        for (i, &v) in vertex.iter().take(3).enumerate() {
            if let (Some(a), Some(b)) = (min.get_mut(i), max.get_mut(i)) {
                *a = a.min(v);
                *b = b.max(v);
            }
        }
        for value in vertex {
            binary.extend(value.to_le_bytes());
        }
    }
    let vertex_bytes = binary.len();
    for i in &scene.indices {
        binary.extend(i.to_le_bytes());
    }
    let index_bytes = binary.len() - vertex_bytes;
    let texture_offset = binary.len();
    binary.extend(png(scene)?);
    let texture_bytes = binary.len() - texture_offset;
    let value = json!({
        "asset":{"version":"2.0","generator":florastack_core::branding::DISPLAY_NAME},
        "extensionsUsed":["KHR_materials_unlit"],
        "scene":0,"scenes":[{"nodes":[0]}],"nodes":[{"mesh":0}],
        "meshes":[{"primitives":[{"attributes":{"POSITION":0,"TEXCOORD_0":1},"indices":2,"material":0,"mode":4}]}],
        "materials":[{"extensions":{"KHR_materials_unlit":{}},"doubleSided":true,"alphaMode":"BLEND","pbrMetallicRoughness":{"baseColorTexture":{"index":0},"metallicFactor":0.0,"roughnessFactor":1.0}}],
        "textures":[{"source":0,"sampler":0}],"samplers":[{"magFilter":9729,"minFilter":9729,"wrapS":33071,"wrapT":33071}],
        "images":[{"bufferView":2,"mimeType":"image/png"}],
        "buffers":[{"byteLength":binary.len()}],
        "bufferViews":[
            {"buffer":0,"byteOffset":0,"byteLength":vertex_bytes,"byteStride":20,"target":34962},
            {"buffer":0,"byteOffset":vertex_bytes,"byteLength":index_bytes,"target":34963},
            {"buffer":0,"byteOffset":texture_offset,"byteLength":texture_bytes}
        ],
        "accessors":[
            {"bufferView":0,"byteOffset":0,"componentType":5126,"count":scene.vertices.len(),"type":"VEC3","min":min,"max":max},
            {"bufferView":0,"byteOffset":12,"componentType":5126,"count":scene.vertices.len(),"type":"VEC2"},
            {"bufferView":1,"byteOffset":0,"componentType":5125,"count":scene.indices.len(),"type":"SCALAR"}
        ],
        "extras":{"depthConvention":"relative focus depth; positive Z toward viewer; longest image side = 1; not metric"}
    });
    let mut json = serde_json::to_vec(&value).map_err(|source| CommandError::Params { command: "parallax.mesh".into(), source })?;
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    while !binary.len().is_multiple_of(4) {
        binary.push(0);
    }
    let len = u32_len(28 + json.len() + binary.len())?;
    let mut out = Vec::with_capacity(len as usize);
    out.extend(b"glTF");
    out.extend(2_u32.to_le_bytes());
    out.extend(len.to_le_bytes());
    out.extend(u32_len(json.len())?.to_le_bytes());
    out.extend(b"JSON");
    out.extend(json);
    out.extend(u32_len(binary.len())?.to_le_bytes());
    out.extend(b"BIN\0");
    out.extend(binary);
    Ok(out)
}

/// OBJ references fixed, portable companion filenames. OBJ uses bottom-left texture V.
pub fn obj(scene: &Scene) -> Result<Vec<u8>, CommandError> {
    use std::fmt::Write as _;
    scene.validate()?;
    let mut text = String::from("# Relative focus-depth relief; longest image side = 1\nmtllib material.mtl\nusemtl composite\n");
    for [x, y, z, _, _] in &scene.vertices {
        let _ = writeln!(text, "v {x} {y} {z}");
    }
    for [_, _, _, u, v] in &scene.vertices {
        let _ = writeln!(text, "vt {u} {}", 1.0 - v);
    }
    for triangle in scene.indices.as_chunks::<3>().0 {
        let [a, b, c] = [triangle[0] + 1, triangle[1] + 1, triangle[2] + 1];
        let _ = writeln!(text, "f {a}/{a} {b}/{b} {c}/{c}");
    }
    Ok(text.into_bytes())
}
