# ShiftDrop

Drag a file, hold **Shift**, drop it on a format. A local file converter for GNOME on Wayland.

```
Shift while dragging          format wheel: video, image, audio, PDF, text, subtitles
Shift + Ctrl while dragging   tools wheel:  compress, metadata, resize, rotate, trim, normalize, PDF, merge
Files right-click             Scripts > ShiftDrop: the same wheel, pick by clicking (right-click switches)
drop on the centre / Esc      cancel
```

Outputs go next to the original and never overwrite anything (`photo.jpg`, `photo 2.jpg`, `clip compressed.mp4`).
If that folder is read-only, they go to `~/ShiftDrop` instead.
Drops are only ever taken as a copy, so the file you dragged stays where it was.
Cancel in the status pop-up stops a job and removes what it wrote. A job killed some other way
(`pkill`, logout) has its half-written file removed the next time the app starts.

## How it works

```
extension/   GNOME Shell extension. Sees drags start and end and the Shift key (Wayland apps can't),
             asks the app to show the wheel over D-Bus, puts the wheel under the pointer
src/
  main.rs      command line, or the app when started without a command
  app.rs       GTK4 wheel window (a plain drop target) + status pop-up + trim/resize dialogs
  wheel.rs     cairo drawing and hit-testing for the wheel
  formats.rs   what a file is and what it can become (rules per category)
  media.rs     what's inside a media file (ffprobe), whether GPU H.264 works
  convert.rs   conversion -> list of commands (ffmpeg, magick, pdftoppm, soffice, heif-enc)
  tools.rs     tool -> list of commands
  jobs.rs      runs the commands, ffmpeg progress, removes half-written files on failure
```

## Install

Needs GNOME 49+ on Wayland, a Rust toolchain (`cargo`), plus:

```
sudo apt install libgtk-4-dev ffmpeg imagemagick ghostscript poppler-utils libreoffice-writer
./install.sh
```

GNOME only notices a new extension at login. Either log out and back in, or load it into the running
shell: Alt+F2, `lg`, Evaluator tab, paste:

```js
(async () => { const em = Main.extensionManager, uuid = 'shiftdrop@jayhemnani'; const ext = em.createExtensionObject(uuid, imports.gi.Gio.File.new_for_path(imports.gi.GLib.get_home_dir() + '/.local/share/gnome-shell/extensions/' + uuid), 2); await em.loadExtension(ext); })()
```

`gnome-extensions info shiftdrop@jayhemnani` should then say `State: ACTIVE`. `./uninstall.sh` removes everything it added.

Changes to the app apply on the next drag after `./install.sh` and `pkill -f "shiftdrop --gapplication-service"`.
Changes to `extension.js` need a logout: the shell keeps the first copy it loaded.

## Use without the wheel

```
shiftdrop targets clip.mp4
shiftdrop convert --to webm clip.mp4
shiftdrop tool trim clip.mp4 --start 2 --end 9
```

## Tests

```
cargo test     # planned commands + real conversions on tiny generated files, a few seconds
```

## Debugging the drag

`touch ~/.cache/shiftdrop-debug`, then watch `journalctl --user -f -o cat | grep shiftdrop`. Each drag logs
`drag-begin` -> `shift at` -> `show-wheel ok` -> `wheel placed` -> `drag carries` -> `drop at` -> the job result.
The first missing line is where it broke. `rm ~/.cache/shiftdrop-debug` to stop.

## Speed

Measured on this machine (8 cores, Intel GPU), 1080x1920 60 fps phone clip. Quality is SSIM against the
source: 1.0 is identical, above ~0.98 looks the same.

```
container change, codec already fits      copy, no re-encode     116s clip -> WebM 1.4s, MKV 0.4s, M4A 0.2s   SSIM 1.000
re-encode to MP4/MOV/MKV                   GPU H.264 qp 26        busiest 20s: 6.0s, 31.5 MB, 0.989
                                           (CPU x264 fallback)    busiest 20s: 36.4s, 25.6 MB, 0.990
re-encode to WebM                          AV1 (SVT, preset 8)    20s: 35.6s, 5.6 MB, 0.993   (VP9 was 75.5s, 6.8 MB, 0.992)
compress video                             CPU x264 faster crf28  20s: 16.7s, 4.0 MB, 0.987   (GPU is ~40% bigger at equal SSIM)
16 screenshots -> WebP / AVIF / JPG        4 at a time            0.47s / 2.2s / 0.22s   (one at a time: 1.29s / 5.4s / 0.58s)
```

The Intel driver offers only fixed-qp for GPU H.264, so it can't adapt to busy footage; qp 26 keeps sizes
reasonable. HDR (PQ/HLG) is kept when copying or writing AV1, and tone-mapped to SDR for H.264, Xvid and WMV.
Rotated phone videos stay upright. Removing metadata keeps the colour profile; with `jpegtran` installed
(`libjpeg-turbo-progs`) JPEGs are cleaned losslessly.

## Not yet

Crop, redact, annotate and other tools that need a canvas. Archives. A cancel button for long jobs.
