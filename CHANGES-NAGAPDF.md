# NagaPDF Changes (based on pdfcraft 0.21.1)

This branch (`brand/nagapdf`) applies the internal **NagaPDF** rebranding and Windows portable packaging on top of `feature/optimize-presentation-latest`:

- **Brand & Icons**:
  - Replaced application icon with `0logo/darknagalogo.ico` (Windows PE resource) and `0logo/DarkNagaLogo.png` (window/taskbar icon, top-left chrome Home button, Home banner logo, About dialog).
  - Renamed product to **NagaPDF** (company/authors: **NAGA Architects**).
- **UI & Menu Cleanup**:
  - Removed Discord button from top chrome bar and community/ArtCraft card from Home screen.
  - Simplified Help menu (`crates/ui-egui/src/menus.rs` & `crates/engine/src/commands.rs`) to **Keyboard shortcuts** and **About NagaPDF**.
  - Simplified `Dialog::About` to show the Naga logo, **NagaPDF version 0.2.0**, `based on pdfcraft <version>`, and a Close button.
  - Disabled startup update check and removed the update-check toggle from Preferences.
- **Portable Mode & Storage**:
  - Updated portable markers to `portable.txt` / `NagaPDF.portable` and portable folder to `NagaPDFData`.
  - Updated settings, log directory, and single-instance lock path to `NagaPDF`.
- **Binaries & Automation**:
  - Desktop binary: `nagapdf.exe` (`apps/pdfcraft`)
  - CLI binary: `nagapdfcli.exe` (`apps/pdfcraft-cli`), with community/external links removed from `--help` and `--version`.
  - Added `build-nagapdf-portable.bat` to build both release binaries with Windows resources (`PDFCRAFT_REQUIRE_WINRES=1`) and package `dist/release/nagapdf-0.2.0-windows-x64-portable/` and `.zip`.
