{
  pkgs,
  lib,
  config,
  inputs,
  ...
}:

let
  # Native libs winit (Wayland/X11) + wgpu (Vulkan/GL) dlopen at runtime, plus
  # dbus for rfd's native file-dialog portal.
  runtimeLibs = with pkgs; [
    vulkan-loader
    wayland
    libxkbcommon
    libGL
    libx11
    libxcursor
    libxi
    libxrandr
    dbus
    # libav* shared libs for ffmpeg-next (video decode + audio resample).
    ffmpeg-full
  ];
in
{
  packages = [
    pkgs.git
    # Ruffle's `ruffle_core` build script compiles the AVM2 playerglobal
    # (ActionScript 3 standard library) with a Java-based compiler at build time.
    pkgs.jdk
    # cpal's alsa-sys needs ALSA headers + pkg-config to build the audio backend.
    pkgs.pkg-config
    pkgs.alsa-lib
    # rfd's file dialog: native portal needs a running xdg-desktop-portal; zenity
    # is rfd's reliable fallback when no portal is available.
    pkgs.zenity
    # ffmpeg-next links the system libav* via pkg-config; ffmpeg-full carries the
    # decoders/demuxers (h264/vp9/mp4/webm) and swresample we need.
    pkgs.ffmpeg-full
    # ffmpeg-sys-next runs bindgen, which needs libclang to parse the libav headers.
    pkgs.llvmPackages.libclang
  ];

  languages.rust = {
    enable = true;
    # Ruffle's `master` uses `if let` guards, stabilized in Rust 1.95.
    # devenv's pinned nixpkgs ships 1.94, so pull a recent stable via rust-overlay.
    channel = "stable";
  };

  # Must be on the loader path or `cargo run` panics with NoWaylandLib etc.
  env.LD_LIBRARY_PATH = lib.makeLibraryPath runtimeLibs;
  # bindgen (ffmpeg-sys-next) needs to find libclang.so to generate libav bindings.
  env.LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
  # bindgen doesn't inherit the cc-wrapper's include paths, so point it at glibc's
  # headers (errno.h etc.) and clang's builtin headers explicitly.
  env.BINDGEN_EXTRA_CLANG_ARGS = "-isystem ${pkgs.glibc.dev}/include -isystem ${pkgs.llvmPackages.libclang.lib}/lib/clang/${lib.versions.major pkgs.llvmPackages.libclang.version}/include";

  git-hooks.hooks = {
    clippy = {
      enable = true;
      # Check all targets (incl. the example), not just the lib, and fail on warnings.
      settings.extraArgs = "--all-targets";
      settings.denyWarnings = true;
    };
    rustfmt.enable = true;
    nixfmt.enable = true;
    actionlint.enable = true;
  };
}
