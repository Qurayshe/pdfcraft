# pdfcraft-optimize

Layer L4: the PDF Optimizer (execution plan M11.1; Acrobat's Reduce File Size and Optimize PDF ▸
Advanced optimization).

```rust
let report = optimize(&mut doc, &Settings::default())?;   // Reduce File Size's choices
let report = optimize(&mut doc, &Preset::Mobile.settings())?; // smaller, for screens
let report = optimize(&mut doc, &Preset::Presentation.settings())?; // slide decks
// Or with progress: called before each slide, each image and the clean-up; `false` cancels.
let report = optimize_with_progress(&mut doc, &settings, &mut |stage| keep_going)?;
```

- **Presets:** Standard (Reduce File Size's choices: images above 225 ppi to 150 ppi, JPEG
  medium), Mobile (above 144 ppi to 96 ppi, JPEG high) and Presentation (flatten tiled slides,
  no image longer than 1920 px, JPEG 2000 and palette images decoded, JPEG 70), as in Acrobat's
  Settings menu; the Presentation preset is PdfCraft's own.
- **Flatten tiled slides** (`flatten`, off unless the Presentation preset is chosen): a page that
  draws many small clipped image tiles — a slide exported from InDesign, Illustrator, Canva or
  Keynote, whose transparency was flattened into a tile grid — becomes one picture. The page is
  rendered without its text at `max_dimension` pixels into a JPEG, its text objects are drawn over
  it again (still text: searchable, selectable, sharp at any zoom), and the tiles are detached.
  Page `/XObject`s that Type 3 glyph procedures draw are kept, as is every other page resource.
  Left as it is: rotated and `/UserUnit` pages, pages with markup annotations, pages whose form
  XObjects hold text, unreadable content, encrypted documents, and pages where the picture would
  not be smaller. Known limits: text is drawn last, and a clip that clipping text (`Tr` 4–7) put
  on later text is not repeated in the overlay.
- **Merge image fragments** (`fragments`, on by default): some PDF printers draw a page picture
  as thousands of images one pixel row high. Runs of upright 8-bit grey or RGB images in one
  colour space, sharing their left and right edges and stacked edge to edge (at least four,
  drawn with nothing else between them), become one lossless image at their finest resolution,
  which the image settings then resample; the strips are dropped by Clean Up. A real page
  printed this way went from 4.3 MB to 231 KB (Standard) and 161 KB (Mobile). Not merged yet:
  side-by-side tiles, rotated strips, masks. Content arrays use the shared space boundary
  policy, so trailing comments, split operands and graphics state keep their meaning after merging.
- **Images:** the effective resolution of each image is measured where pages draw it (the CTM
  through form XObjects, up to 12 deep; the smallest over all uses).
  Page-local `/UserUnit` scaling is included, so large-format pages are measured in physical inches.
  `max_image_dimension` caps an image's longer side in pixels whatever size the page draws it at,
  which is how a slide defined as 3840 points stops carrying 8K pictures that measure 144 ppi.
  If a use can't be
  measured (forms nested deeper, or drawn too many times), no image is resampled. Colour and grayscale images above a
  threshold are resampled with a bicubic (Catmull-Rom) filter to a target resolution and
  stored as JPEG (Acrobat's quality steps) or ZIP; soft masks are resampled with their image.
  An image is replaced only if the result is smaller. With `extended_formats` on (the
  Presentation preset), JPEG 2000 (`/JPXDecode`, through `hayro-jpeg2000`) and palette
  (`/Indexed`) images are decoded too — a palette to its base colour space — and stored like any
  other image. Left alone: CMYK and other colour spaces,
  image masks, decode arrays, more than 8 bits, JBIG2 and CCITT. A JPEG is read with
  its colour transform (an Adobe marker's, else `/ColorTransform`, ISO 32000-2 §7.4.8), so one
  stored untransformed keeps its colours; one whose marker and `/ColorTransform` disagree is left
  alone, since readers differ on which wins.
- **Target file size** (`target`): with `Settings::target_mb` set, `optimize_to_target` bisects an
  18-rung ladder of (longest side, JPEG quality) and returns the sharpest rung whose saved file
  fits, or the smallest rung when none does. Each rung is a whole optimization of a copy, so a
  search costs at most five of them; a document whose own save already fits is copied unchanged.
- **Discard objects:** thumbnails, alternate images, document tags, print settings.
- **Clean up:** Flate for streams with no filter; for streams with an outer ASCII85 or ASCIIHex
  layer, that layer is removed when the result is smaller (the inner filters keep their encoded
  bytes; streams with `/DecodeParms`, images and object streams keep their ASCII layer. LZW-compressed
  streams, content and images alike, are decoded and stored with Flate instead (Acrobat's "In
  streams that use LZW encoding, use Flate instead"): a predictor carries over to Flate without
  `EarlyChange`, filters applied before the LZW one go with it and those after it keep their
  bytes. A stream whose LZW data doesn't decode cleanly is left as it is. Not yet: inline images.

The engine runs Remove Hidden Information (`pdfcraft-redact`) for the user-data categories,
then this, then merges identical objects and writes a full, compressed save
(`pdfcraft_engine::optimizer::OptimizeJob`, which the UI runs on a worker thread with a
progress bar). JPEG decoding comes from `zune-jpeg` (the decoder behind `image` and the
renderer), JPEG encoding and resampling from the `image` crate. Flattening renders pages through
`pdfcraft-render` (the only way to composite clipped, layered tiles exactly as they are shown),
and JPEG 2000 decoding from `hayro-jpeg2000`, the decoder that renderer already uses.

## Target PDF version: partial compatibility

The target selects the header version, clears the catalog `/Version`, controls object
streams (disabled for PDF 1.4), and checks encryption requirements. It does **not**
detect or convert content newer than the target: optional content, JPEG 2000 images,
16-bit images, OpenType fonts and PDF 2.0 features can remain. A PDF 1.4 header
therefore does not guarantee compatibility with PDF 1.4 readers or validators.
`optimize.compatibility-level` remains partial until content compatibility is checked.
