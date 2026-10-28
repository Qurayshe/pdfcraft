//! Flatten tiled pages: a page made of many image tiles becomes one picture, its text kept.
//!
//! Slides exported from design tools (InDesign, Illustrator, Canva, Keynote) often carry what
//! looks like one picture as a background image many times the screen's size plus hundreds of
//! clipped patches drawn over it (`re W n … /Im Do`), because the tool flattened transparency by
//! cutting the page into tiles. The optimizer measures each tile on its own: every one is small,
//! every one is drawn at about the density the page asks for, and nothing is resampled, although
//! the page as a whole is many times sharper than any screen shows.
//!
//! [`detect_tiled_pages`] finds such pages: a page that draws at least `min_images` images, at
//! least half of them smaller than a quarter of the page. For each, flattening
//!
//! 1. splits the page content into its text objects (`BT … ET`) and everything else,
//! 2. draws the page *without* its text, at `max_dimension` pixels along its longer side, with
//!    the renderer (so clipping, layering, masks and blending come out as they are shown),
//! 3. stores that as one JPEG and draws it, then the text objects again over it, with the
//!    painting operators of everything else turned into `n` so the text keeps the graphics
//!    state (position, colour, clip, font) it had.
//!
//! The text stays text: searchable, selectable, sharp at any zoom. What it can't keep: the
//! stacking of text under later drawing (text is drawn last), so a page where the artwork covers
//! some text looks different. A page is left as it is when it:
//!
//! - is rotated or has a `/UserUnit`, has a comment or other markup annotation (the renderer
//!   draws annotations, which would then show twice), or an unreadable content stream;
//! - draws a form XObject that holds text (the text isn't in the page's own content), or uses a
//!   text render mode that clips (4 to 7);
//! - would not get smaller (the picture is not smaller than the images it replaces);
//! - is in an encrypted document (the renderer is given the document as written).

use std::sync::Arc;

use pdfcraft_content::{Op, parse, serialize_ops};
use pdfcraft_cos::{DecodeBudget, Dict, Document, ObjRef, Object, SaveOptions, Stream, write_full};
use pdfcraft_model::Page;
use pdfcraft_render::{PageRenderer, RenderConfig, RenderRequest, effective_scale};

use crate::images::{PageDraws, content_bytes, jpeg, page_draws};
use crate::{FlattenSettings, OptimizeError, Report, Stage};

/// Most tiles whose neighbours are counted when describing a page (the count is quadratic).
const MAX_TOUCH_TILES: usize = 600;

/// Why a tiled page is left as it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skip {
    /// The page is displayed rotated.
    Rotated,
    /// The page has a `/UserUnit` other than 1.
    UserUnit,
    /// The page has a markup annotation (not a link).
    Annotations,
    /// A form XObject the page draws holds text.
    FormText,
    /// The page uses a text render mode that clips.
    TextClip,
    /// The content stream, or a form's, can't be read back exactly.
    UnreadableContent,
    /// The picture would not be smaller than the images it replaces.
    NoGain,
    /// The renderer could not draw the page.
    RenderFailed,
    /// The document is encrypted.
    Encrypted,
}

impl Skip {
    /// A short phrase for reports.
    pub fn reason(self) -> &'static str {
        match self {
            Skip::Rotated => "rotated page",
            Skip::UserUnit => "page has a UserUnit",
            Skip::Annotations => "page has markup annotations",
            Skip::FormText => "a form XObject holds text",
            Skip::TextClip => "text used as a clipping path",
            Skip::UnreadableContent => "content stream not readable",
            Skip::NoGain => "would not get smaller",
            Skip::RenderFailed => "page could not be rendered",
            Skip::Encrypted => "encrypted document",
        }
    }
}

/// A page made of many image tiles.
#[derive(Clone, Debug, PartialEq)]
pub struct TiledPage {
    /// Position in page order, from 0.
    pub index: usize,
    pub page: ObjRef,
    /// Images the page draws (every drawing counts).
    pub images: usize,
    /// Of those, the ones drawn smaller than a quarter of the page.
    pub tiles: usize,
    /// Pairs of tiles that share an edge: the grid the tiles form (0 when there are over
    /// [`MAX_TOUCH_TILES`] tiles).
    pub touching: usize,
    /// The drawn images' total area over the page's: about 1 for a picture cut in pieces, more
    /// when pieces are drawn over a background.
    pub coverage: f64,
    /// Why flattening leaves the page as it is, if it does.
    pub skip: Option<Skip>,
}

struct Scanned {
    info: TiledPage,
    page: Page,
    /// The page's operators, when it can be flattened.
    ops: Result<Vec<Op>, Skip>,
    /// Encoded bytes of the distinct images the page draws.
    image_bytes: usize,
}

fn area(r: &[f64; 4]) -> f64 {
    ((r[2] - r[0]).max(0.0)) * ((r[3] - r[1]).max(0.0))
}

/// Whether the page has an annotation that is drawn and isn't a link or form control.
fn has_markup_annots(doc: &Document, page: &Page) -> bool {
    let Some(annots) = page.dict.get(b"Annots").map(|a| doc.resolve(a)) else { return false };
    let Some(list) = annots.as_array() else { return false };
    list.iter().any(|a| {
        let a = doc.resolve(a);
        a.as_dict().is_some_and(|d| !matches!(d.name(b"Subtype"), Some(b"Link" | b"Popup" | b"Widget")))
    })
}

/// The page's operators if it can be flattened, or why not.
fn inspect(doc: &Document, page: &Page, draws: &PageDraws) -> Result<Vec<Op>, Skip> {
    if page.rotation(doc) != 0 {
        return Err(Skip::Rotated);
    }
    if (page.user_unit(doc) - 1.0).abs() > 1e-9 {
        return Err(Skip::UserUnit);
    }
    if has_markup_annots(doc, page) {
        return Err(Skip::Annotations);
    }
    if draws.form_text {
        return Err(Skip::FormText);
    }
    if !draws.complete {
        return Err(Skip::UnreadableContent);
    }
    let contents = page.dict.get(b"Contents").ok_or(Skip::UnreadableContent)?;
    let data = content_bytes(doc, contents, &mut DecodeBudget::page()).ok_or(Skip::UnreadableContent)?;
    let parsed = parse(&data);
    if parsed.skipped > 0 || parsed.ambiguous_images > 0 {
        return Err(Skip::UnreadableContent);
    }
    Ok(parsed.ops)
}

/// Every page that draws at least `min_images` images, most of them small, with how it is made
/// and whether flattening could handle it.
fn scan(doc: &Document, min_images: usize) -> Vec<Scanned> {
    let mut out = Vec::new();
    for (index, page) in pdfcraft_model::pages(doc).into_iter().enumerate() {
        let draws = page_draws(doc, page.obj);
        if draws.draws.len() < min_images.max(1) {
            continue;
        }
        let crop = page.crop(doc);
        let page_area = area(&crop);
        if page_area <= 0.0 {
            continue;
        }
        let tiles: Vec<&[f64; 4]> = draws.draws.iter().map(|d| &d.rect).filter(|r| area(r) < 0.25 * page_area).collect();
        if tiles.len() * 2 < draws.draws.len() {
            continue;
        }
        let tol = 0.002 * (crop[2] - crop[0]).max(crop[3] - crop[1]);
        let mut touching = 0;
        if tiles.len() <= MAX_TOUCH_TILES {
            for (i, a) in tiles.iter().enumerate() {
                for b in &tiles[i + 1..] {
                    let overlap_y = a[3].min(b[3]) - a[1].max(b[1]);
                    let overlap_x = a[2].min(b[2]) - a[0].max(b[0]);
                    let side = ((a[2] - b[0]).abs() <= tol || (b[2] - a[0]).abs() <= tol) && overlap_y > tol;
                    let stacked = ((a[3] - b[1]).abs() <= tol || (b[3] - a[1]).abs() <= tol) && overlap_x > tol;
                    touching += usize::from(side || stacked);
                }
            }
        }
        let coverage = draws.draws.iter().map(|d| area(&d.rect)).sum::<f64>() / page_area;
        let mut seen = std::collections::HashSet::new();
        let image_bytes = draws
            .draws
            .iter()
            .filter(|d| seen.insert(d.image))
            .map(|d| match &*doc.get(d.image) {
                Object::Stream(s) => s.raw.len(),
                _ => 0,
            })
            .sum();
        let ops = inspect(doc, &page, &draws);
        let info = TiledPage {
            index,
            page: page.obj,
            images: draws.draws.len(),
            tiles: tiles.len(),
            touching,
            coverage,
            skip: ops.as_ref().err().copied(),
        };
        out.push(Scanned { info, page, ops, image_bytes });
    }
    out
}

/// The pages of `doc` made of many image tiles: at least `min_images` images drawn, at least
/// half of them smaller than a quarter of the page. Each says whether flattening can handle it
/// ([`TiledPage::skip`]).
pub fn detect_tiled_pages(doc: &Document, min_images: usize) -> Vec<TiledPage> {
    scan(doc, min_images).into_iter().map(|s| s.info).collect()
}

/// The operators of a page split in two: everything but its text objects (what the renderer
/// draws to make the picture), and its text objects with the state operators around them but no
/// painting (so they come out as they were).
fn split(ops: &[Op]) -> (Vec<Op>, Vec<Op>) {
    let (mut background, mut text) = (Vec::with_capacity(ops.len()), Vec::new());
    let mut text_block = Vec::new();
    let mut in_text = false;
    for op in ops {
        if op.is("BT") {
            in_text = true;
            text_block.clear();
            text_block.push(op.clone());
            continue;
        }
        if in_text {
            text_block.push(op.clone());
            if op.is("ET") {
                in_text = false;
                let has_clip = text_block.iter().any(|o| o.is("Tr") && o.num(0).is_some_and(|m| m >= 4.0));
                if has_clip {
                    // Clipping text defines the boundary of an image or shading in the background.
                    // Keep the clip in the background so the renderer paints it accurately.
                    background.extend(text_block.iter().cloned());
                    // In the foreground text overlay, convert Tr >= 4 to Tr 3 (invisible text)
                    // so it does not draw duplicate pixels, yet stays searchable and selectable.
                    for o in &text_block {
                        if o.is("Tr") && o.num(0).is_some_and(|m| m >= 4.0) {
                            text.push(Op::new("Tr", vec![Object::Int(3)]));
                        } else {
                            text.push(o.clone());
                        }
                    }
                } else {
                    text.extend(text_block.iter().cloned());
                }
            }
            continue;
        }
        background.push(op.clone());
        match op.op.as_slice() {
            // Painting becomes "end the path": a clip set by `W` before it still applies.
            b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*" | b"S" | b"s" => text.push(Op::new("n", Vec::new())),
            b"sh" | b"Do" | b"BI" => {}
            _ => text.push(op.clone()),
        }
    }
    (background, text)
}

/// A page being flattened: its original `/Contents`, its text operators and its box.
struct Plan {
    index: usize,
    page: ObjRef,
    crop: [f64; 4],
    original: Option<Object>,
    text: Vec<Op>,
    images: usize,
    image_bytes: usize,
}

fn restore(doc: &mut Document, plans: &[Plan]) -> Result<(), OptimizeError> {
    for p in plans {
        if let Some(original) = &p.original {
            doc.update_dict(p.page, |d| d.set(b"Contents".to_vec(), original.clone()))?;
        }
    }
    Ok(())
}

/// A name for the picture that the page's image resources don't use yet.
fn fresh_name(doc: &Document, page: &Page) -> Vec<u8> {
    let xobjects = page.dict.get(b"Resources").map(|r| doc.resolve(r)).and_then(|r| r.as_dict().cloned()).and_then(|r| r.get(b"XObject").map(|x| doc.resolve(x)).and_then(|x| x.as_dict().cloned()));
    let mut k = 0usize;
    loop {
        let name = if k == 0 { b"PCFlat".to_vec() } else { format!("PCFlat{k}").into_bytes() };
        if !xobjects.as_ref().is_some_and(|x| x.contains(&name)) {
            return name;
        }
        k += 1;
    }
}

/// Replace the page's XObjects with only the flattened picture, detaching the hundreds of tiles.
fn set_only_xobject(doc: &mut Document, page: ObjRef, name: &[u8], image: ObjRef) -> Result<(), OptimizeError> {
    let mut x = Dict::new();
    x.set(name.to_vec(), Object::Ref(image));
    let x_ref = doc.add(Object::Dict(x));
    let res = doc.get(page).as_dict().and_then(|d| d.get(b"Resources").cloned());
    match res {
        Some(Object::Ref(r)) => {
            let mut d = doc.get(r).as_dict().cloned().unwrap_or_default();
            d.set(b"XObject".to_vec(), Object::Ref(x_ref));
            let new_res = doc.add(Object::Dict(d));
            doc.update_dict(page, |p| p.set(b"Resources".to_vec(), Object::Ref(new_res)))?;
        }
        Some(Object::Dict(mut d)) => {
            d.set(b"XObject".to_vec(), Object::Ref(x_ref));
            doc.update_dict(page, |p| p.set(b"Resources".to_vec(), Object::Dict(d)))?;
        }
        _ => {
            let mut d = Dict::new();
            d.set(b"XObject".to_vec(), Object::Ref(x_ref));
            doc.update_dict(page, |p| p.set(b"Resources".to_vec(), Object::Dict(d)))?;
        }
    }
    Ok(())
}

/// Premultiplied RGBA8 as RGB8 over white.
fn rgb_over_white(rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len() / 4 * 3);
    for px in rgba.chunks_exact(4) {
        let hole = 255 - px[3];
        out.extend_from_slice(&[px[0].saturating_add(hole), px[1].saturating_add(hole), px[2].saturating_add(hole)]);
    }
    out
}

pub(crate) fn run(doc: &mut Document, settings: &FlattenSettings, report: &mut Report, progress: &mut dyn FnMut(Stage) -> bool) -> Result<(), OptimizeError> {
    let scanned = scan(doc, settings.min_images);
    report.tiled_pages += scanned.len();
    if scanned.is_empty() {
        return Ok(());
    }
    if doc.output_handler().is_some() {
        report.pages_not_flattened += scanned.len();
        return Ok(());
    }
    // Stage 1: each page that can be flattened gets, for now, its content without text, so one
    // serialization of the document lets the renderer draw every page as the picture will be.
    let mut plans = Vec::new();
    for s in scanned {
        let Ok(ops) = s.ops else {
            report.pages_not_flattened += 1;
            continue;
        };
        let (background, text) = split(&ops);
        let original = doc.get(s.page.obj).as_dict().and_then(|d| d.get(b"Contents").cloned());
        let stripped = doc.add(Object::Stream(Stream::flate(Dict::new(), &serialize_ops(&background))));
        doc.update_dict(s.page.obj, |d| d.set(b"Contents".to_vec(), Object::Ref(stripped)))?;
        plans.push(Plan { index: s.info.index, page: s.page.obj, crop: s.page.crop(doc), original, text, images: s.info.images, image_bytes: s.image_bytes });
    }
    if plans.is_empty() {
        return Ok(());
    }
    let bytes = match write_full(doc, &SaveOptions { object_streams: false, ..SaveOptions::default() }) {
        Ok(b) => b,
        Err(e) => {
            restore(doc, &plans)?;
            return Err(e.into());
        }
    };
    let mut renderer = PageRenderer::new(Arc::new(bytes), RenderConfig { hide_comments: true, ..RenderConfig::default() });
    // Stage 2: draw each page, store the picture, put the text back.
    let total = plans.len();
    let all_pages = pdfcraft_model::pages(doc);
    for (done, plan) in plans.iter().enumerate() {
        if !progress(Stage::Flatten { done, total }) {
            restore(doc, &plans[done..])?;
            return Err(OptimizeError::Cancelled);
        }
        let (w, h) = (plan.crop[2] - plan.crop[0], plan.crop[3] - plan.crop[1]);
        let scale = effective_scale(w as f32, h as f32, settings.max_dimension.max(16) as f32 / w.max(h) as f32);
        let page = renderer.render(RenderRequest { page: plan.index, scale, ..RenderRequest::default() });
        let fail = |doc: &mut Document, plan: &Plan| restore(doc, std::slice::from_ref(plan));
        if page.error.is_some() || page.width == 0 || page.height == 0 || page.rgba.len() != page.width as usize * page.height as usize * 4 {
            fail(doc, plan)?;
            report.pages_not_flattened += 1;
            continue;
        }
        let rgb = rgb_over_white(&page.rgba);
        let Some(data) = jpeg(&rgb, 3, page.width, page.height, settings.quality) else {
            fail(doc, plan)?;
            report.pages_not_flattened += 1;
            continue;
        };
        if data.len() >= plan.image_bytes {
            fail(doc, plan)?;
            report.pages_not_flattened += 1;
            continue;
        }
        let mut dict = Dict::new();
        dict.set(b"Type".to_vec(), Object::name("XObject"));
        dict.set(b"Subtype".to_vec(), Object::name("Image"));
        dict.set(b"Width".to_vec(), Object::Int(i64::from(page.width)));
        dict.set(b"Height".to_vec(), Object::Int(i64::from(page.height)));
        dict.set(b"ColorSpace".to_vec(), Object::name("DeviceRGB"));
        dict.set(b"BitsPerComponent".to_vec(), Object::Int(8));
        dict.set(b"Filter".to_vec(), Object::name("DCTDecode"));
        let image = doc.add(Object::Stream(Stream::from_raw(dict, data)));
        let Some(leaf) = all_pages.iter().find(|p| p.obj == plan.page) else {
            fail(doc, plan)?;
            report.pages_not_flattened += 1;
            continue;
        };
        let name = fresh_name(doc, leaf);
        set_only_xobject(doc, plan.page, &name, image)?;
        let mut ops = vec![
            Op::new("q", Vec::new()),
            Op::new("cm", vec![Object::Real(w), Object::Int(0), Object::Int(0), Object::Real(h), Object::Real(plan.crop[0]), Object::Real(plan.crop[1])]),
            Op::new("Do", vec![Object::Name(name)]),
            Op::new("Q", Vec::new()),
        ];
        ops.extend(plan.text.iter().cloned());
        let contents = doc.add(Object::Stream(Stream::flate(Dict::new(), &serialize_ops(&ops))));
        doc.update_dict(plan.page, |d| d.set(b"Contents".to_vec(), Object::Ref(contents)))?;
        report.pages_flattened += 1;
        report.tiles_flattened += plan.images;
    }
    if !progress(Stage::Flatten { done: total, total }) {
        return Err(OptimizeError::Cancelled);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(s: &str, operands: Vec<Object>) -> Op {
        Op::new(s, operands)
    }

    #[test]
    fn text_keeps_its_state_and_loses_the_painting() {
        let ops = vec![
            op("q", vec![]),
            op("re", vec![0.into(), 0.into(), 10.into(), 10.into()]),
            op("W", vec![]),
            op("n", vec![]),
            op("rg", vec![1.into(), 0.into(), 0.into()]),
            op("re", vec![0.into(), 0.into(), 5.into(), 5.into()]),
            op("f", vec![]),
            op("Do", vec![Object::name("Im")]),
            op("BT", vec![]),
            op("Tj", vec![]),
            op("ET", vec![]),
            op("Q", vec![]),
        ];
        let (background, text) = split(&ops);
        assert!(background.iter().all(|o| !o.is("BT") && !o.is("Tj") && !o.is("ET")));
        assert!(background.iter().any(|o| o.is("Do")) && background.iter().any(|o| o.is("f")));
        // The text side keeps the clip and colour, has no Do, and the fill became a no-paint `n`.
        assert!(text.iter().all(|o| !o.is("Do") && !o.is("f")));
        assert!(text.iter().any(|o| o.is("W")) && text.iter().any(|o| o.is("rg")) && text.iter().any(|o| o.is("Tj")));
        assert_eq!(text.iter().filter(|o| o.is("q")).count(), text.iter().filter(|o| o.is("Q")).count());
    }

    #[test]
    fn premultiplied_pixels_go_over_white() {
        assert_eq!(rgb_over_white(&[10, 20, 30, 255, 0, 0, 0, 0, 50, 50, 50, 128]), vec![10, 20, 30, 255, 255, 255, 177, 177, 177]);
    }
}

