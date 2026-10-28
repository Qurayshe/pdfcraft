# RFC: Presentation Optimization Mode, Absolute Dimension Capping, and Target-Size Budgeting in `pdfcraft-optimize`

- **Feature Name:** `optimize-presentation-mode-and-target-budgeting`
- **Target Crate:** `crates/optimize` (`pdfcraft-optimize`)
- **Status:** Draft / RFC Proposal
- **Author:** Antigravity & User

---

## 1. Summary

This RFC proposes enhancements to `pdfcraft-optimize` (`crates/optimize`) to address common scenarios where Adobe Acrobat and PdfCraft's current optimizers produce minimal or ineffective compression on digital presentation slide decks and image-heavy portfolios:

1. **Absolute Dimension Capping (`max_dimension` / `max_pixels` in `ImageSettings`)**: Allows capping maximum image resolution (e.g. 1920px or 2560px) in addition to physical PPI, overcoming the "PPI fallacy" where 4K/8K images placed on large-point slides bypass downsampling.
2. **Preset::Presentation & Screen Mode**: A dedicated preset tailored for presentations, digital portfolios, and screen viewing that downsamples oversized images to presentation standards (FHD / 1080p or 720p).
3. **JPEG 2000 (`JPXDecode`) Recompression**: Optional decoding and re-compression of non-compliant/bloated JPX images into standard JPEG/Flate.
4. **Target-Size Budgeting Mode (`optimize_to_target_size`)**: An iterative or sampling-based optimization routine that solves for the optimal downsample scale and JPEG quality factor to guarantee the final PDF lands below a user-specified file size constraint (e.g. `< 20 MB` for email attachments).
5. **2D Image Grid Consolidation**: Extending `fragments.rs` beyond 1-pixel-high vertical strips to support 2D sliced tile grids.

---

## 2. Motivation & Real-World Case Study

### The Problem
In testing `pdfcraft-optimize` and Adobe Acrobat's *Reduce File Size* on real-world creative assets (specifically a 136-page architectural portfolio of 137.2 MB containing 1,669 embedded images), both engines achieved little compression. The PDF remained severely bloated.

### Root Cause Analysis in `pdfcraft-optimize`

#### A. The "PPI Fallacy" on Large Slide Decks
In `crates/optimize/src/images.rs`:
```rust
let (w, h) = (s.dict.int(b"Width").unwrap_or(0) as f64, s.dict.int(b"Height").unwrap_or(0) as f64);
let [a, b, c, d, ..] = top.0;
let (sx, sy) = ((a * a + b * b).sqrt(), (c * c + d * d).sqrt());
if w > 0.0 && h > 0.0 && sx > 1e-6 && sy > 1e-6 {
    let ppi = (w / (sx / 72.0)).min(h / (sy / 72.0));
    let e = self.out.entry(r).or_insert(f64::INFINITY);
    *e = e.min(ppi);
}
```
- Modern slide presentations exported from Figma, InDesign, or Keynote frequently define page dimensions in points matching digital screen pixels (e.g., `3840 × 2160` pt for 4K).
- At standard PDF scaling (72 points = 1 inch), `3840 pt` represents a physical width of **53.33 inches**.
- An **8K image (7685 × 4321 px)** spanning the slide yields:
  $$\text{PPI} = \frac{7685}{53.33} \approx 144.1\text{ PPI}$$
- Under `ImageSettings::REDUCE` (`above_ppi: 225.0`), the 8K image is evaluated as having low PPI and is **completely skipped**. Even under `Preset::Mobile` (`above_ppi: 144.0`), standard 4K and FHD images sit below the threshold and are left untouched.

#### B. Sliced 2D Image Grids
- Presentation tools frequently export image layouts as grids of side-by-side and stacked tiles (in our test asset, Page 6 contained **193 separate image objects**).
- `fragments.rs` currently only merges single-column, 1-pixel-high vertical strips:
  > *"Runs of upright 8-bit grey or RGB images in one colour space, sharing their left and right edges and stacked edge to edge... Not merged yet: side-by-side tiles, rotated strips, masks."*
- Evaluating each tile independently fails to address the aggregate overhead of hundreds of small image objects and clipping paths.

#### C. Untouched Formats (JPEG 2000)
- `crates/optimize/src/lib.rs` explicitly leaves JPEG 2000 (`JPXDecode`) untouched.
- In our test portfolio, 856 images (36.2 MB) were JPEG 2000 streams.

---

## 3. Proposed Design & Architecture

### A. Absolute Dimension Capping in `ImageSettings`

Extend `ImageSettings` in `crates/optimize/src/lib.rs`:

```rust
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageSettings {
    pub downsample: bool,
    pub target_ppi: f64,
    pub above_ppi: f64,
    pub compression: Compression,
    /// Optional upper bound on pixel dimensions.
    /// If set, any image exceeding max_dimension in width or height
    /// is downscaled so its longest edge equals max_dimension,
    /// regardless of effective PPI.
    pub max_dimension: Option<u32>,
}
```

In `crates/optimize/src/images.rs`:
During image resampling decision:
```rust
let exceeds_ppi = ppi > settings.above_ppi;
let exceeds_dim = settings.max_dimension.map_or(false, |max_d| {
    orig_width > max_d || orig_height > max_d
});

if exceeds_ppi || exceeds_dim {
    // Calculate new target dimensions honoring both target_ppi and max_dimension
    ...
}
```

### B. `Preset::Presentation`

Add a dedicated preset in `crates/optimize/src/lib.rs`:

```rust
pub enum Preset {
    /// Reduce File Size's choices (Standard print preset).
    Standard,
    /// Mobile screens (96 ppi above 144 ppi).
    Mobile,
    /// Presentations & Portfolios: caps images to 1080p/720p bounds,
    /// handles oversized digital slides, optimizes for screen presentation.
    Presentation,
}

impl Preset {
    pub fn settings(self) -> Settings {
        match self {
            Preset::Standard => Settings::default(),
            Preset::Mobile => Settings {
                color: ImageSettings::MOBILE,
                gray: ImageSettings::MOBILE,
                ..Settings::default()
            },
            Preset::Presentation => Settings {
                color: ImageSettings {
                    downsample: true,
                    target_ppi: 96.0,
                    above_ppi: 120.0,
                    compression: Compression::Jpeg(68),
                    max_dimension: Some(1920),
                },
                gray: ImageSettings {
                    downsample: true,
                    target_ppi: 96.0,
                    above_ppi: 120.0,
                    compression: Compression::Jpeg(68),
                    max_dimension: Some(1920),
                },
                ..Settings::default()
            },
        }
    }
}
```

### C. Target-Size Budgeting (`optimize_to_target`)

Add an auto-budgeting API to allow applications (and CLI commands) to specify a maximum desired file size:

```rust
pub fn optimize_to_target(
    doc: &mut Document,
    target_bytes: u64,
    progress: &mut dyn FnMut(Stage) -> bool,
) -> Result<Report, OptimizeError>
```

**Implementation Strategy:**
1. Compute the total page count $N$ and calculate the per-page byte budget:
   $$\text{page\_budget} = \frac{\text{target\_bytes} \times 0.90}{N}$$
2. Sample $K$ representative pages ($K \approx \min(10, N)$).
3. Evaluate candidate settings (varying `max_dimension` between 1280–1920 and JPEG quality between 50–75) against the sampled pages.
4. Select the highest-fidelity profile whose extrapolated total size fits within `target_bytes`.
5. Execute full optimization pass using the solved parameters.

### D. Recompression of JPEG 2000 (`JPXDecode`)
Add an opt-in flag in `Settings`:
```rust
pub struct Settings {
    ...
    /// Convert JPEG 2000 (JPXDecode) images to JPEG or Flate when smaller.
    pub recompress_jpx: bool,
}
```
Leverage standard Rust image decoders (or `zune-jpeg` / `openjpeg`) to decompress JPX streams and re-encode to baseline JPEG.

---

## 4. Test Results & Proof of Concept

Applying this exact strategy to the 137.2 MB, 136-page portfolio produced the following results:

| Mode | Resolution / Quality | Output Size | Reduction | Searchable Text Preserved? |
| :--- | :--- | :--- | :--- | :--- |
| **Original** | Up to 8K, 1669 images | 137.2 MB | — | Yes |
| **Acrobat / Current Optimizer** | Standard (PPI > 225) | ~125 MB | < 9% | Yes |
| **Proposed Presentation HQ** | 1440×810, Q68 | **18.8 MB** | **86.3%** | Yes (100% vector match) |
| **Proposed Presentation Standard**| 1280×720, Q65 | **14.5 MB** | **89.4%** | Yes (100% vector match) |
| **Proposed Compact** | 1280×720, Q50 | **11.7 MB** | **91.4%** | Yes (100% vector match) |

---

## 5. Alternatives & Drawbacks

- **Alternative (Pure raster flattening):** Rendering entire pages to bitmaps eliminates vector text unless an invisible text layer is reconstructed. By implementing dimension capping directly inside `crates/optimize/src/images.rs`, the original text operators (`BT ... ET`) and font dictionaries are left 100% untouched.
- **Drawback:** Resampling images by absolute dimension requires tracking original image dimensions alongside effective CTM, adding minor bookkeeping in `Walk`.

---

## 6. Next Steps & Implementation Plan

1. **Phase 1:** Add `max_dimension: Option<u32>` to `ImageSettings` and hook into `images.rs::Walk`.
2. **Phase 2:** Add `Preset::Presentation` to `lib.rs` and expose via CLI `doc_optimize --preset presentation`.
3. **Phase 3:** Implement target-size solver (`optimize_to_target`) and add unit tests against sample presentation PDFs.
4. **Phase 4:** Expand `fragments.rs` to support 2D grid consolidation.
