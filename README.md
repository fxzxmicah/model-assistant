# Model Assistant

Model Assistant is a GNOME desktop application for launching local AI model runtimes from a `MODELS_ROOT` layout.

## Features

- startup validation for `MODELS_ROOT`, `MODELS_ROOT/Runner/<rootfs>`, and `MODELS_ROOT/Files/assistant.toml`
- model, runtime, and mode selection from a TOML configuration file
- isolated runner helper process and `chroot`-based model launch
- per-model console output and interactive input when the selected mode supports it
- GNOME desktop integration through a desktop file, AppStream metadata, D-Bus activation, and system-wide dconf shortcut defaults

## Expected layout

```text
MODELS_ROOT/
├── Files/
│   ├── assistant.toml
│   └── <model files>
└── Runner/
    ├── dev/
    ├── proc/
    ├── tmp/
    ├── usr/
    └── ...
```

## Build dependencies

The Rust sources currently use these crate lines:

- `gtk4 = 0.11.3`
- `libadwaita = 0.9.1`
- `nix = 0.30`
- `serde = 1`
- `toml = 0.9`
- `vte = 0.15`

You also need system development packages for GTK 4 and libadwaita.

## Configuration notes

- Runtime-family settings live under `runtimes.<name>`.
- Model-specific runtime mappings live under `models.<name>.runtimes.<runtime>`.
- The configured model files mount point inside the runner is fixed as `/mnt`.
- The process launch path is `chroot <MODELS_ROOT/Runner> <configured executable> ...`.
- The example configuration file is available at `examples/assistant.toml`.

## Desktop integration

The application ID is `org.gnome.ModelAssistant`.

This repository ships:

- `data/org.gnome.ModelAssistant.desktop`
- `data/org.gnome.ModelAssistant.metainfo.xml`
- `data/org.gnome.ModelAssistant.service.in`
- `data/icons/hicolor/scalable/apps/org.gnome.ModelAssistant.svg`
- `data/dconf/db/distro.d/00-model-assistant-shortcuts`
- `data/dconf/db/distro.d/locks/00-model-assistant-shortcuts`

The D-Bus service file uses an install-time `@bindir@` placeholder so downstream packages can substitute the correct binary directory.

## Fedora packaging

A Fedora-style RPM spec file is provided as `spec/model-assistant.spec`.
It installs the application binaries, desktop metadata, AppStream metadata, the D-Bus activation file, the application icon, the dconf defaults and locks, the license text, and the example configuration as documentation.
