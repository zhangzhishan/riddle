# Kobo Elipsa 2E Port Implementation Plan

> **For Hermes:** Implement this plan task-by-task, keeping host tests green after each change.

**Goal:** Port riddle's ink-disappear-and-handwritten-reply experience to Kobo Elipsa 2E (Condor) as a safe standalone NickelMenu application.

**Architecture:** Keep the state machine, oracle, memory, ink rasterization, and handwriting synthesis portable. Add a Kobo-only device layer: a tiny C shim around FBInk for framebuffer mapping/partial refresh, and a Rust evdev parser for the Elipsa 2E's combined multitouch/stylus stream. Launch through NickelMenu while temporarily pausing Nickel, with a trap that always resumes it.

**Tech Stack:** Rust 2021, FBInk C API, Linux evdev, armv7-unknown-linux-gnueabihf, NickelMenu shell launcher.

---

### Task 1: Make geometry and pixel format portable

**Objective:** Run the drawing core at Elipsa 2E resolution and on an 8-bit grayscale framebuffer.

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/fb.rs`
- Modify: `src/surface.rs`
- Modify: `src/display.rs`
- Test: `src/surface.rs`

**Steps:**
1. Add a `kobo` feature and set Condor geometry to 1404×1872 when enabled.
2. Add `PixFmt::Gray8`, including pixel writes, luma, inversion, snapshots, and pastes.
3. Add focused Gray8 tests.
4. Run `cargo test` and `cargo test --features kobo --no-run`.
5. Commit.

### Task 2: Add the FBInk display backend

**Objective:** Map the framebuffer and issue fast/balanced/full partial refreshes without duplicating FBInk's ABI structs in Rust.

**Files:**
- Create: `native/kobo_fbink_shim.c`
- Create: `native/kobo_fbink_shim.h`
- Create: `src/kobo_display.rs`
- Modify: `src/display.rs`
- Modify: `build.rs`
- Modify: `Cargo.toml`

**Steps:**
1. Implement an opaque C context using `fbink_open`, `fbink_init`, `fbink_get_state`, `fbink_get_fb_pointer`, and `fbink_refresh`.
2. Validate Condor/1404×1872 at runtime and expose width, height, stride, bpp, and buffer pointer.
3. Select A2/DU for ink, GL16/AUTO for balanced updates, and flashing GC16 for full refresh.
4. Wrap the shim in a Rust `KoboDisplay` with RAII cleanup.
5. Compile the C shim only under the `kobo` feature and link an externally built static FBInk.
6. Run host tests and a cross-link smoke build.
7. Commit.

### Task 3: Parse Kobo Stylus 2 evdev input

**Objective:** Convert the Elipsa 2E combined MT stylus stream into screen-space pressure samples.

**Files:**
- Refactor: `src/pen.rs`
- Modify: `src/main.rs`
- Test: `src/pen.rs`

**Steps:**
1. Stop assuming a 24-byte `input_event`; use the target's native `timeval` layout.
2. Parse `ABS_MT_SLOT`, tracking ID, position X/Y, tool type, and `ABS_MT_PRESSURE`.
3. Handle Condor's non-mirrored X and mirrored Y transform, with axis maxima read through `EVIOCGABS`.
4. Treat `BTN_STYLUS` as eraser and `BTN_STYLUS2` as highlighter/future tool input without confusing finger slots for ink.
5. Add synthetic event-stream tests for pen down/move/up, pressure, Y mirror, and side-button behavior.
6. Run tests and commit.

### Task 4: Package a reversible NickelMenu launcher

**Objective:** Make installation and exit safe even if riddle crashes.

**Files:**
- Create: `kobo/launch.sh`
- Create: `kobo/install.sh`
- Create: `kobo/uninstall.sh`
- Create: `kobo/nm/riddle-kobo`
- Create: `scripts/build-kobo.sh`
- Modify: `README.md`

**Steps:**
1. Pause Nickel with SIGSTOP immediately before launch and always SIGCONT it in an EXIT/INT/TERM trap.
2. Add a visible emergency restore command and documented SSH recovery.
3. Stage binary, FBInk runtime, font, config example, launcher, and NickelMenu entry into `dist/riddle-kobo/`.
4. Make install idempotent and uninstall non-destructive to user memories.
5. Shellcheck/syntax-check scripts and commit.

### Task 5: Verify on host and Elipsa 2E

**Objective:** Prove both the portable behavior and exact device calibration.

**Files:**
- Create: `scripts/kobo-probe.sh`
- Create: `docs/elipsa-2e-verification.md`

**Steps:**
1. Build and run all Rust tests on macOS.
2. Cross-build ARMv7 release and inspect ELF architecture/dependencies.
3. Connect Elipsa 2E over USB, install the probe, and capture firmware, FBInk state, input names, axes, and stylus event codes.
4. Calibrate transforms from four corner taps; update only constants proven wrong.
5. Install bundle, test writing latency, ink dissolve, reply animation, five-finger exit, Nickel restoration, suspend/wake, and Wi-Fi reconnect.
6. Commit and push the verified branch.
