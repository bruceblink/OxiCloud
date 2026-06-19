//! Pure geometry + post-processing for the ONNX face pipeline.
//!
//! Everything here is plain Rust (no `ort`, no `ndarray`) so it compiles in the
//! default build and is exercised by `cargo test` — the error-prone numerical
//! parts (SCRFD anchor decode, NMS, 5-point similarity alignment, the affine
//! warp, normalization) are unit-tested in isolation, while the untestable ONNX
//! session calls live behind the `faces-onnx` feature in `onnx_face_analyzer`.
//!
//! The pipeline mirrors InsightFace's reference implementation:
//! SCRFD detector (distance-to-box anchors over strides 8/16/32) → 5-point
//! similarity transform onto the canonical 112×112 ArcFace template → ArcFace
//! embedder → L2-normalized 512-d vector.

use image::RgbImage;

/// One detected face in **detector-input pixel** coordinates (before scaling
/// back to the original image): an axis-aligned box `[x1, y1, x2, y2]`, the
/// five facial landmarks, and the detector confidence.
#[derive(Debug, Clone, Copy)]
pub struct Detection {
    pub bbox: [f32; 4],
    pub kps: [[f32; 2]; 5],
    pub score: f32,
}

/// A 2×3 affine transform mapping an output/template coordinate to a source
/// coordinate: `src = (a·ox + b·oy + tx, c·ox + d·oy + ty)`. Used to sample the
/// source image when warping an aligned face crop.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub tx: f32,
    pub ty: f32,
}

/// Canonical ArcFace 5-point template for a 112×112 crop
/// (left eye, right eye, nose, left mouth, right mouth).
pub const ARCFACE_TEMPLATE: [[f32; 2]; 5] = [
    [38.2946, 51.6963],
    [73.5318, 51.5014],
    [56.0252, 71.7366],
    [41.5493, 92.3655],
    [70.7299, 92.2041],
];

/// Aligned-crop side length expected by the ArcFace embedder.
pub const ALIGN_SIZE: u32 = 112;

/// Letterbox geometry for the detector: the largest scale that fits a
/// `w0 × h0` image into a `det × det` square without distortion, plus the
/// resulting (possibly smaller) dimensions placed at the top-left.
///
/// Returns `(new_w, new_h, scale)` where `scale = min(det/w0, det/h0)` and
/// detector-space coordinates map back to the original by dividing by `scale`.
pub fn letterbox(w0: u32, h0: u32, det: u32) -> (u32, u32, f32) {
    if w0 == 0 || h0 == 0 {
        return (0, 0, 1.0);
    }
    let scale = (det as f32 / w0 as f32).min(det as f32 / h0 as f32);
    let new_w = ((w0 as f32 * scale).round() as u32).clamp(1, det);
    let new_h = ((h0 as f32 * scale).round() as u32).clamp(1, det);
    (new_w, new_h, scale)
}

/// `NCHW`, RGB, float input tensor for an ONNX model: `(px − mean) · scale`,
/// channel-major (all R, then all G, then all B). Length is `3 · w · h`.
pub fn chw_normalized(img: &RgbImage, mean: f32, scale: f32) -> Vec<f32> {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let mut out = vec![0.0f32; 3 * w * h];
    let plane = w * h;
    for (i, px) in img.pixels().enumerate() {
        out[i] = (px[0] as f32 - mean) * scale;
        out[plane + i] = (px[1] as f32 - mean) * scale;
        out[2 * plane + i] = (px[2] as f32 - mean) * scale;
    }
    out
}

/// Decode one SCRFD feature-map stride into detections, appending those above
/// `threshold` to `out`. All coordinates are in detector-input pixels.
///
/// `scores` is `[n]`, `bbox` is `[n·4]` (left, top, right, bottom *distances*,
/// already multiplied by `stride`), `kps` (when present) is `[n·10]`
/// (5 × (dx, dy) distances, already multiplied by `stride`), where
/// `n = feat_h · feat_w · num_anchors`. Anchor centers follow InsightFace's
/// row-major `mgrid` order with `num_anchors` consecutive duplicates.
#[allow(clippy::too_many_arguments)]
pub fn decode_stride(
    scores: &[f32],
    bbox: &[f32],
    kps: Option<&[f32]>,
    stride: u32,
    feat_h: u32,
    feat_w: u32,
    num_anchors: u32,
    threshold: f32,
    out: &mut Vec<Detection>,
) {
    let stride_f = stride as f32;
    let mut idx = 0usize;
    for y in 0..feat_h {
        for x in 0..feat_w {
            let cx = x as f32 * stride_f;
            let cy = y as f32 * stride_f;
            for _ in 0..num_anchors {
                if idx >= scores.len() {
                    return;
                }
                let score = scores[idx];
                if score >= threshold {
                    let b = idx * 4;
                    if b + 3 < bbox.len() {
                        let det_bbox = [
                            cx - bbox[b],
                            cy - bbox[b + 1],
                            cx + bbox[b + 2],
                            cy + bbox[b + 3],
                        ];
                        let mut det_kps = [[0.0f32; 2]; 5];
                        if let Some(kps) = kps {
                            let k = idx * 10;
                            if k + 9 < kps.len() {
                                for (p, slot) in det_kps.iter_mut().enumerate() {
                                    *slot = [cx + kps[k + p * 2], cy + kps[k + p * 2 + 1]];
                                }
                            }
                        }
                        out.push(Detection {
                            bbox: det_bbox,
                            kps: det_kps,
                            score,
                        });
                    }
                }
                idx += 1;
            }
        }
    }
}

/// Intersection-over-union of two `[x1, y1, x2, y2]` boxes.
pub fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let x1 = a[0].max(b[0]);
    let y1 = a[1].max(b[1]);
    let x2 = a[2].min(b[2]);
    let y2 = a[3].min(b[3]);
    let iw = (x2 - x1).max(0.0);
    let ih = (y2 - y1).max(0.0);
    let inter = iw * ih;
    let area_a = (a[2] - a[0]).max(0.0) * (a[3] - a[1]).max(0.0);
    let area_b = (b[2] - b[0]).max(0.0) * (b[3] - b[1]).max(0.0);
    let union = area_a + area_b - inter;
    if union <= 0.0 { 0.0 } else { inter / union }
}

/// Greedy non-maximum suppression: keep highest-scoring boxes, drop any whose
/// IoU with an already-kept box exceeds `iou_thresh`. Returns the kept
/// detections, highest score first.
pub fn nms(mut dets: Vec<Detection>, iou_thresh: f32) -> Vec<Detection> {
    dets.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut keep: Vec<Detection> = Vec::with_capacity(dets.len());
    for d in dets {
        if keep.iter().all(|k| iou(&k.bbox, &d.bbox) <= iou_thresh) {
            keep.push(d);
        }
    }
    keep
}

/// Least-squares similarity transform (scale + rotation + translation, no
/// shear, no reflection) mapping `src` landmarks onto `dst`, returned as its
/// **inverse** affine (output/template coordinate → source coordinate) ready
/// for backward-warp sampling.
///
/// Solved in closed form via the complex-number formulation: with points as
/// complex numbers, `w = Σ (b'ᵢ · conj(a'ᵢ)) / Σ |a'ᵢ|²` and `t = mean_b −
/// w·mean_a`, which is equivalent to the Umeyama solution InsightFace obtains
/// from `skimage.SimilarityTransform`.
pub fn similarity_transform_inverse(src: &[[f32; 2]; 5], dst: &[[f32; 2]; 5]) -> Affine {
    let n = 5.0f32;
    let (mut max, mut may, mut mbx, mut mby) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    for i in 0..5 {
        max += src[i][0];
        may += src[i][1];
        mbx += dst[i][0];
        mby += dst[i][1];
    }
    max /= n;
    may /= n;
    mbx /= n;
    mby /= n;

    // num = Σ b'·conj(a')  (complex),  den = Σ |a'|²  (real)
    let (mut num_re, mut num_im, mut den) = (0.0f32, 0.0f32, 0.0f32);
    for i in 0..5 {
        let ax = src[i][0] - max;
        let ay = src[i][1] - may;
        let bx = dst[i][0] - mbx;
        let by = dst[i][1] - mby;
        // b' · conj(a') = (bx + i·by)(ax − i·ay)
        num_re += bx * ax + by * ay;
        num_im += by * ax - bx * ay;
        den += ax * ax + ay * ay;
    }
    let den = if den.abs() < 1e-12 { 1e-12 } else { den };
    // w = num/den  (forward scale·rotation)
    let wr = num_re / den;
    let wi = num_im / den;
    // t = mean_b − w·mean_a
    let tr = mbx - (wr * max - wi * may);
    let ti = mby - (wi * max + wr * may);

    // Inverse of the similarity: src = Ainv·(out − t), Ainv = [[wr,wi],[−wi,wr]]/|w|²
    let det = wr * wr + wi * wi;
    let g = if det.abs() < 1e-12 { 0.0 } else { 1.0 / det };
    Affine {
        a: g * wr,
        b: g * wi,
        c: -g * wi,
        d: g * wr,
        tx: -g * (wr * tr + wi * ti),
        ty: g * (wi * tr - wr * ti),
    }
}

/// Warp `img` into an `ALIGN_SIZE × ALIGN_SIZE` aligned face crop using the
/// inverse affine from [`similarity_transform_inverse`], sampling bilinearly
/// and clamping to the image edge.
pub fn warp_to_aligned(img: &RgbImage, inv: &Affine) -> RgbImage {
    let (w, h) = (img.width(), img.height());
    let mut out = RgbImage::new(ALIGN_SIZE, ALIGN_SIZE);
    for oy in 0..ALIGN_SIZE {
        for ox in 0..ALIGN_SIZE {
            let sx = inv.a * ox as f32 + inv.b * oy as f32 + inv.tx;
            let sy = inv.c * ox as f32 + inv.d * oy as f32 + inv.ty;
            let px = bilinear_sample(img, sx, sy, w, h);
            out.put_pixel(ox, oy, px);
        }
    }
    out
}

/// Bilinear RGB sample at floating `(x, y)`, clamping out-of-bounds reads to
/// the nearest edge.
fn bilinear_sample(img: &RgbImage, x: f32, y: f32, w: u32, h: u32) -> image::Rgb<u8> {
    let x = x.clamp(0.0, (w - 1) as f32);
    let y = y.clamp(0.0, (h - 1) as f32);
    let x0 = x.floor() as u32;
    let y0 = y.floor() as u32;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let dx = x - x0 as f32;
    let dy = y - y0 as f32;
    let p00 = img.get_pixel(x0, y0);
    let p10 = img.get_pixel(x1, y0);
    let p01 = img.get_pixel(x0, y1);
    let p11 = img.get_pixel(x1, y1);
    let mut out = [0u8; 3];
    for (ch, slot) in out.iter_mut().enumerate() {
        let top = p00[ch] as f32 * (1.0 - dx) + p10[ch] as f32 * dx;
        let bot = p01[ch] as f32 * (1.0 - dx) + p11[ch] as f32 * dx;
        *slot = (top * (1.0 - dy) + bot * dy).round().clamp(0.0, 255.0) as u8;
    }
    image::Rgb(out)
}

/// In-place L2 normalization. A zero vector is left unchanged.
pub fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-12 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// Variance of the discrete Laplacian over the luminance of an RGB crop — a
/// cheap focus/sharpness proxy (higher = sharper). Used as a face quality
/// score for cover selection and gating.
pub fn laplacian_variance(img: &RgbImage) -> f32 {
    let (w, h) = (img.width() as i64, img.height() as i64);
    if w < 3 || h < 3 {
        return 0.0;
    }
    let lum = |x: i64, y: i64| -> f32 {
        let p = img.get_pixel(x as u32, y as u32);
        0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32
    };
    let mut vals = Vec::with_capacity(((w - 2) * (h - 2)) as usize);
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let l = 4.0 * lum(x, y) - lum(x - 1, y) - lum(x + 1, y) - lum(x, y - 1) - lum(x, y + 1);
            vals.push(l);
        }
    }
    let n = vals.len() as f32;
    if n == 0.0 {
        return 0.0;
    }
    let mean = vals.iter().sum::<f32>() / n;
    vals.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letterbox_fits_and_preserves_aspect() {
        // Landscape 1000×500 into 640 → width-bound, scale 0.64.
        let (nw, nh, s) = letterbox(1000, 500, 640);
        assert_eq!(nw, 640);
        assert_eq!(nh, 320);
        assert!((s - 0.64).abs() < 1e-6);
        // Square fills exactly.
        let (nw, nh, s) = letterbox(800, 800, 640);
        assert_eq!((nw, nh), (640, 640));
        assert!((s - 0.8).abs() < 1e-6);
    }

    #[test]
    fn letterbox_degenerate_is_safe() {
        assert_eq!(letterbox(0, 10, 640), (0, 0, 1.0));
    }

    #[test]
    fn chw_layout_and_normalization() {
        let mut img = RgbImage::new(2, 1);
        img.put_pixel(0, 0, image::Rgb([127, 0, 255]));
        img.put_pixel(1, 0, image::Rgb([128, 255, 0]));
        let t = chw_normalized(&img, 127.5, 1.0 / 128.0);
        // Length = 3 channels × 2 px.
        assert_eq!(t.len(), 6);
        // R plane first, then G, then B (NCHW).
        assert!((t[0] - (127.0 - 127.5) / 128.0).abs() < 1e-6);
        assert!((t[1] - (128.0 - 127.5) / 128.0).abs() < 1e-6);
        assert!((t[2] - (0.0 - 127.5) / 128.0).abs() < 1e-6); // G of px0
        assert!((t[4] - (255.0 - 127.5) / 128.0).abs() < 1e-6); // B of px0
    }

    #[test]
    fn distance_decode_recovers_box_and_kps() {
        // 1×2 grid, stride 8, 1 anchor → cell centers (0,0) then (8,0).
        let scores = [0.9f32, 0.9];
        // distances left/top/right/bottom (already × stride), identical per cell.
        let bbox = [2.0, 1.0, 3.0, 4.0, 2.0, 1.0, 3.0, 4.0];
        let kps: Vec<f32> = vec![
            1.0, 1.0, 2.0, 2.0, 0.0, 0.0, -1.0, 1.0, 1.0, -1.0, // cell 0
            1.0, 1.0, 2.0, 2.0, 0.0, 0.0, -1.0, 1.0, 1.0, -1.0, // cell 1
        ];
        let mut out = Vec::new();
        decode_stride(&scores, &bbox, Some(&kps), 8, 1, 2, 1, 0.5, &mut out);
        assert_eq!(out.len(), 2);
        // Cell 0, center (0,0): box = center ± distances, kps = center + offset.
        assert_eq!(out[0].bbox, [-2.0, -1.0, 3.0, 4.0]);
        assert_eq!(out[0].kps[0], [1.0, 1.0]);
        assert_eq!(out[0].kps[1], [2.0, 2.0]);
        // Cell 1, center (8,0): anchor center advanced by one stride in x.
        assert_eq!(out[1].bbox, [8.0 - 2.0, -1.0, 8.0 + 3.0, 4.0]);
        assert_eq!(out[1].kps[0], [9.0, 1.0]);
    }

    #[test]
    fn decode_thresholds_out_low_scores() {
        let scores = [0.2f32, 0.8];
        let bbox = [0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0];
        let mut out = Vec::new();
        // 1×2 grid, 1 anchor → two cells.
        decode_stride(&scores, &bbox, None, 8, 1, 2, 1, 0.5, &mut out);
        assert_eq!(out.len(), 1);
        assert!((out[0].score - 0.8).abs() < 1e-6);
    }

    #[test]
    fn iou_and_nms() {
        let a = [0.0, 0.0, 10.0, 10.0];
        let b = [0.0, 0.0, 10.0, 10.0];
        assert!((iou(&a, &b) - 1.0).abs() < 1e-6);
        let c = [100.0, 100.0, 110.0, 110.0];
        assert_eq!(iou(&a, &c), 0.0);

        let dets = vec![
            Detection {
                bbox: a,
                kps: [[0.0; 2]; 5],
                score: 0.9,
            },
            Detection {
                bbox: b,
                kps: [[0.0; 2]; 5],
                score: 0.8,
            }, // dup of a
            Detection {
                bbox: c,
                kps: [[0.0; 2]; 5],
                score: 0.7,
            }, // separate
        ];
        let kept = nms(dets, 0.4);
        assert_eq!(kept.len(), 2);
        assert!((kept[0].score - 0.9).abs() < 1e-6);
    }

    #[test]
    fn similarity_identity() {
        let inv = similarity_transform_inverse(&ARCFACE_TEMPLATE, &ARCFACE_TEMPLATE);
        assert!((inv.a - 1.0).abs() < 1e-4);
        assert!(inv.b.abs() < 1e-4);
        assert!(inv.c.abs() < 1e-4);
        assert!((inv.d - 1.0).abs() < 1e-4);
        assert!(inv.tx.abs() < 1e-3);
        assert!(inv.ty.abs() < 1e-3);
    }

    #[test]
    fn similarity_pure_translation() {
        // src = dst shifted by (+10, +5); inverse must map out→src by the same shift.
        let mut src = ARCFACE_TEMPLATE;
        for p in &mut src {
            p[0] += 10.0;
            p[1] += 5.0;
        }
        let inv = similarity_transform_inverse(&src, &ARCFACE_TEMPLATE);
        assert!((inv.a - 1.0).abs() < 1e-4);
        assert!(inv.b.abs() < 1e-4);
        assert!((inv.tx - 10.0).abs() < 1e-3);
        assert!((inv.ty - 5.0).abs() < 1e-3);
    }

    #[test]
    fn warp_identity_preserves_template_region() {
        // A 112×112 gradient warped by identity returns (close to) itself.
        let mut img = RgbImage::new(ALIGN_SIZE, ALIGN_SIZE);
        for y in 0..ALIGN_SIZE {
            for x in 0..ALIGN_SIZE {
                img.put_pixel(x, y, image::Rgb([x as u8, y as u8, 128]));
            }
        }
        let inv = similarity_transform_inverse(&ARCFACE_TEMPLATE, &ARCFACE_TEMPLATE);
        let out = warp_to_aligned(&img, &inv);
        let a = out.get_pixel(40, 60);
        assert!((a[0] as i32 - 40).abs() <= 1);
        assert!((a[1] as i32 - 60).abs() <= 1);
    }

    #[test]
    fn l2_normalize_unit_length() {
        let mut v = vec![3.0f32, 4.0];
        l2_normalize(&mut v);
        assert!((v[0] - 0.6).abs() < 1e-6);
        assert!((v[1] - 0.8).abs() < 1e-6);
        let mut z = vec![0.0f32, 0.0];
        l2_normalize(&mut z); // unchanged, no NaN
        assert_eq!(z, vec![0.0, 0.0]);
    }

    #[test]
    fn laplacian_variance_sharp_vs_flat() {
        let flat = RgbImage::from_pixel(8, 8, image::Rgb([100, 100, 100]));
        assert!(laplacian_variance(&flat) < 1e-3);
        let mut checker = RgbImage::new(8, 8);
        for y in 0..8 {
            for x in 0..8 {
                let v = if (x + y) % 2 == 0 { 0 } else { 255 };
                checker.put_pixel(x, y, image::Rgb([v, v, v]));
            }
        }
        assert!(laplacian_variance(&checker) > 1000.0);
    }
}
