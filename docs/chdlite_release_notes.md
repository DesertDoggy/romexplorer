# chdlite 0.3.2

chdlite is released separately from romexplorer; these notes cover chdlite only.

No changes to chdlite itself. It is released alongside romexplorer 0.2.1.

# chdlite 0.3.1

## Bug fixes

- The Windows arm64 and Linux arm64 builds could not convert Wii U, Xbox and Xbox 360 images
  or read PBP files.

# chdlite 0.3.0

## Changes

- **Completely rewritten in Rust, built on the romexplorer core library.** It replaces the
  C++ command-line tool and the Flutter GUI of 0.2.x; chdman and Dolphin's disc code are the
  same native libraries romexplorer itself ships.
- **RVZ.** GameCube and Wii images are converted to RVZ, and an RVZ extracts back to ISO.
- **Wii U and Xbox.** A .wud or .wux and its key become Cemu's .wua, an Xbox 360 image
  Xenia's .zar, an Xbox image an XISO for xemu. These three keep the game's files but not the
  disc, so chdlite asks first; "Don't ask again" and Settings turn the question off.
- **CSO and PBP.** A .cso extracts to its ISO and a PS1 EBOOT.PBP to cue and bin; Lite and
  Comp make CHDs of them, and Hash hashes what is inside.
- **Lossless inputs only.** A .wua, .zar or XISO is not taken: the disc is already gone.
- **The container is chosen for you.** CD and GD-ROM images become a CD CHD, DVD images a
  DVD CHD, GameCube / Wii images an RVZ, decided from the image itself rather than its name.
- **One window, four modes.** Lite (fast), Comp (smallest), Read and Hash: pick one and drop
  files or folders anywhere in the window. CHD and RVZ settings are in Settings; the output
  folder is in the main window.
- **Four console tools**, one per mode, so a file dropped onto the executable does the right
  thing: `chdlite`, `chdcomp`, `chdread`, `chdhash`. Every option is also a flag (`--help`).
- Several files are converted at once, as before, each in its own worker process, and
  Cancel in the window now stops a CHD creation part-way.
- **Android.** The same four modes as an Android app (arm64), for touch or a controller: tap
  a mode, then choose files or a folder. A run keeps going in the background and shows its
  progress in a notification. A command-line `chdlite` for Android (arm64) is a separate
  download, for a terminal app or `adb shell`.
- Builds for Windows x64 / arm64, Linux x64 / arm64, macOS arm64 and Android arm64. On
  Windows the GUI is a single file.

## Bug fixes

- A conversion cut off by Ctrl-C, a crash or closing the window left a broken file that the
  next run skipped as done. Closing the window now stops the conversions too.
- A PS2 or PSP ISO of a certain size was taken for an audio CD.
- Cancel could be ignored while short jobs were running, and jobs it stopped were not
  counted.
- A settings file that didn't parse was replaced with the defaults. It is now kept as
  `chdlite.toml.bak`.
- Answering No to the lossy question dropped the whole drop; Skip these now converts the
  rest. A big folder no longer freezes the window while it is read.
- `-hs` set the same hunk size for CDs and DVDs; it now goes to whichever it fits.

## Not in this release

- GCZ and WIA output.
- The chdman-compatible command names (`createcd`, `extractcd`, ...) and hard disk / raw CHDs.
- A single-`.bin` extract option: a CD always extracts to one file per track.
