{
  description = "ribb — rust iced booru browser (iced GUI, ffmpeg video, Ruffle SWF)";

  nixConfig = {
    extra-substituters = [ "https://ribb.cachix.org" ];
    extra-trusted-public-keys = [
      "ribb.cachix.org-1:nj76tnyKG03zK7d1VSa3XuSaVJxYrs5l4j42uVoj7RU="
    ];
  };

  # Outputs: `ribb` (native, wrapped), `appimage`, `ribb-windows` (static .exe).
  #
  # The hard part is Ruffle (via iced_ruffle). Three things tamed below:
  #  - `ruffle_core`'s build.rs compiles its playerglobal from `../core/src/...`;
  #    vendoring flattens the crate, so crane patches the vendored git checkout.
  #  - `swf 0.2.2` comes from BOTH crates.io (Ruffle's build-time `rascal`) and
  #    Ruffle's git workspace. Cargo.toml patches the crates.io copy to the same
  #    rev-pinned Ruffle checkout that iced_ruffle uses, so nix vendors one source.
  #  - Windows: ribb only DECODES, so the static ffmpeg needs no x264/openh264;
  #    the gfxcapture WinRT filter is disabled (it drags in mcfgthread link deps).

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    nix-appimage = {
      url = "github:ralismark/nix-appimage";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane = {
      url = "github:ipetkov/crane";
    };
    # Recent stable rust (>=1.95, which Ruffle's `master` needs) + windows-gnu std.
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      nix-appimage,
      crane,
      fenix,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
        lib = pkgs.lib;

        fenixPkgs = fenix.packages.${system};
        rustStable = fenixPkgs.stable.toolchain;
        craneLib = (crane.mkLib pkgs).overrideToolchain rustStable;
        src = craneLib.cleanCargoSource ./.;

        # Ruffle's git workspace is not vendor-safe: ruffle_core/build.rs reaches
        # outside its crate root for core/src/avm{1,2}/globals. Crane follows
        # cargo-vendor semantics and splits git workspace crates into separate
        # directories, so patch the vendored ruffle_core crate at the vendor
        # boundary. The `swf` crates.io patch in Cargo.toml points at this same
        # rev-pinned Ruffle checkout, avoiding the old committed vendor copy.
        cargoVendorDir = craneLib.vendorCargoDeps {
          inherit src;
          overrideVendorGitCheckout =
            ps: drv:
            if lib.any (p: lib.hasPrefix "git+https://github.com/ruffle-rs/ruffle" p.source) ps then
              drv.overrideAttrs (_old: {
                postInstall = ''
                  rc="$out/ruffle_core-0.2.0"
                  substituteInPlace "$rc/build.rs" \
                    --replace-fail 'Path::new("../")' 'Path::new(".")'
                  mkdir "$rc/core"
                  ln -s ../src "$rc/core/src"
                '';
              })
            else
              drv;
        };

        # Decode-only ffmpeg. ribb only ever DECODES webm (matroska → vp8/vp9/av1)
        # and mp4 (mov → h264/hevc) plus their audio (aac/opus/vorbis/mp3/flac) —
        # `is_video` matches only those two extensions; everything else (jpg/png/
        # gif/webp) goes through the `image` crate, never ffmpeg. It never encodes,
        # muxes, filters, or touches the network. So `--disable-everything` + a
        # tight allowlist drops the hundreds of unused codecs/muxers/protocols.
        # The built-in video decoders need no external codec lib, so the
        # encode-only deps (x264, libvpx, …) fall away with them. This is what
        # shrinks the static Windows .exe (and the Linux closure) the most.
        ffmpegDecodeFlags = [
          "--disable-everything"
          "--enable-protocol=file"
          "--enable-demuxer=mov,matroska"
          "--enable-decoder=h264,hevc,vp8,vp9,av1,mpeg4,mjpeg"
          "--enable-decoder=aac,aac_latm,mp3,vorbis,opus,flac,ac3,alac,pcm_s16le,pcm_s16be,pcm_u8,pcm_f32le,pcm_f32be"
          "--enable-parser=h264,hevc,vp8,vp9,av1,mpeg4video,aac,aac_latm,vorbis,opus,mpegaudio,flac,ac3"
          "--enable-bsf=h264_mp4toannexb,hevc_mp4toannexb,vp9_superframe_split,vp9_raw_reorder,av1_frame_split,aac_adtstoasc,mpeg4_unpack_bframes,extract_extradata"
        ];

        # Native decode-only build: ffmpeg-headless (no SDL/X11/etc.) trimmed to
        # the allowlist above. ffmpeg-sys-next links it dynamically.
        ffmpegNative = pkgs.ffmpeg-headless.overrideAttrs (old: {
          configureFlags = (old.configureFlags or [ ]) ++ ffmpegDecodeFlags;
          # ffmpeg's own test binaries (e.g. libavfilter/tests/integral) reference
          # symbols from filters that --disable-everything removed, so the check
          # phase fails to link. We only consume the libs, not ffmpeg's tests.
          doCheck = false;
        });

        # Native libs iced (wgpu) + winit dlopen at runtime by name — not picked up
        # by RUNPATH, so they go on LD_LIBRARY_PATH via the wrapper for a
        # self-contained AppImage. Plus ALSA for cpal audio (video + Ruffle).
        # ffmpegNative is a *direct* dynamic dep (ffmpeg-sys-next), but its store
        # path lands on neither RUNPATH nor the bare LD_LIBRARY_PATH, so the
        # installed/AppImage binary can't find libav*.so.* — hence it goes here
        # too, not just in buildInputs.
        runtimeLibs = with pkgs; [
          vulkan-loader
          wayland
          libxkbcommon
          libGL
          libx11
          libxcursor
          libxi
          libxrandr
          alsa-lib
          ffmpegNative
        ];

        # mesa's lavapipe — software Vulkan driver. A self-contained AppImage ships
        # no GPU driver and replaces the host's /nix, so wgpu would find no adapter
        # and iced falls back to its software renderer, under which the video shader
        # widget (and Ruffle) draw nothing. Bundling lavapipe guarantees an adapter.
        lavapipeIcd = "${pkgs.mesa}/share/vulkan/icd.d/lvp_icd.x86_64.json";
        vulkanFallback = ''
          if [ -n "$VK_ICD_FILENAMES$VK_DRIVER_FILES" ]; then
            export VK_ICD_FILENAMES="''${VK_ICD_FILENAMES:-$VK_DRIVER_FILES}:${lavapipeIcd}"
          else
            export VK_ADD_DRIVER_FILES="''${VK_ADD_DRIVER_FILES:+$VK_ADD_DRIVER_FILES:}${lavapipeIcd}"
          fi
        '';

        # A freedesktop .desktop entry so the native build shows up in app
        # launchers (and can be installed via a NixOS config). No icon ships in
        # the repo, so fall back to a stock freedesktop icon name.
        desktopItem = pkgs.makeDesktopItem {
          name = "ribb";
          desktopName = "ribb";
          comment = "rust iced booru browser";
          exec = "ribb";
          icon = "applications-internet";
          categories = [
            "Graphics"
            "Viewer"
          ];
        };

        commonArgs = {
          pname = "ribb";
          version = "0.1.0";
          inherit src cargoVendorDir;
          cargoExtraArgs = "--locked";

          nativeBuildInputs = [
            pkgs.pkg-config
            # bindgen (ffmpeg-sys-next) needs libclang for the libav bindings.
            pkgs.llvmPackages.libclang
            # Ruffle's build script compiles the AVM2 playerglobal with a JDK.
            pkgs.jdk
          ];

          buildInputs = [
            ffmpegNative
            pkgs.alsa-lib
          ];

          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
          BINDGEN_EXTRA_CLANG_ARGS = "-isystem ${pkgs.glibc.dev}/include -isystem ${pkgs.llvmPackages.libclang.lib}/lib/clang/${lib.versions.major pkgs.llvmPackages.libclang.version}/include";
        };

        cargoArtifacts = craneLib.buildDepsOnly (commonArgs // { doCheck = false; });

        ribb = craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;
            doCheck = false;

            nativeBuildInputs = commonArgs.nativeBuildInputs ++ [
              pkgs.makeWrapper
              # Installs desktopItem into $out/share/applications via postInstall.
              pkgs.copyDesktopItems
            ];

            desktopItems = [ desktopItem ];

            postInstall = ''
              wrapProgram $out/bin/ribb \
                --run ${lib.escapeShellArg vulkanFallback} \
                --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath runtimeLibs}
            '';

            meta = {
              description = "rust iced booru browser";
              mainProgram = "ribb";
              license = lib.licenses.mit;
            };
          }
        );

        appimage = nix-appimage.lib.${system}.mkAppImage {
          program = lib.getExe ribb;
          name = "ribb-${ribb.version}-${system}.AppImage";
        };

        # --- Windows cross — a fully static ribb.exe ---
        # ribb only DECODES video, so the static ffmpeg needs no external codec
        # libs (h264/vp8/vp9 decoders + mp4/webm demuxers are built in) — no x264,
        # no openh264, none of finn's C++-runtime link gymnastics.
        crossPkgs =
          (import nixpkgs {
            inherit system;
            config = {
              allowBroken = true;
              allowUnsupportedSystem = true;
            };
          }).pkgsCross.mingwW64;

        ffmpegStatic =
          (crossPkgs.ffmpeg-headless.override {
            withHeadlessDeps = false;
            buildAvcodec = true;
            buildAvformat = true;
            buildAvfilter = true;
            buildAvutil = true;
            buildSwscale = true;
            buildSwresample = true;
            buildAvdevice = true; # ffmpeg-next probes libavdevice.pc
            buildFfmpeg = false;
            buildFfprobe = false;
            # zlib links as zlib1.dll (nixpkgs' cross zlib is shared), the one
            # non-system import that would break a self-contained exe. ribb's
            # decode targets (h264/mp4, vp9/webm) don't need it, so drop it.
            withZlib = false;
            withOpenh264 = false;
            withGPL = false;
            withUnfree = false;
            withStatic = true;
            withShared = false;
            withVulkan = false;
            withSdl2 = false;
          }).overrideAttrs
            (old: {
              # Same decode-only allowlist as the native build (see ffmpegDecodeFlags).
              # `--disable-everything` builds no filters at all, which also subsumes
              # the old gfxcapture hack: that WinRT screen-capture filter used to drag
              # in mcfgthread/WinRT link deps (undefined _MCF_mutex_* / __MCF_gthr_*)
              # into libavfilter.a; with no filters built it simply can't.
              configureFlags = (old.configureFlags or [ ]) ++ ffmpegDecodeFlags;
            });

        winCraneLib = (crane.mkLib crossPkgs).overrideToolchain (
          p:
          let
            fenixFor = fenix.packages.${p.stdenv.buildPlatform.system};
          in
          fenixFor.combine [
            fenixFor.stable.rustc
            fenixFor.stable.cargo
            fenixFor.targets.x86_64-pc-windows-gnu.stable.rust-std
          ]
        );
        winCC = "${crossPkgs.stdenv.cc}/bin/${crossPkgs.stdenv.cc.targetPrefix}cc";

        winCommonArgs = {
          pname = "ribb";
          version = "0.1.0";
          inherit src cargoVendorDir;
          doCheck = false;
          cargoExtraArgs = "--locked";

          # Build scripts (incl. Ruffle's JDK-free rascal playerglobal) run on the
          # host; the cross toolchain targets Windows.
          depsBuildBuild = [ crossPkgs.stdenv.cc ];
          nativeBuildInputs = [
            pkgs.pkg-config
            pkgs.llvmPackages.libclang
            pkgs.jdk
            pkgs.nasm
            pkgs.cmake
          ];

          # Env vars go in `env` (the cross stdenv already populates some via
          # structuredAttrs, so top-level attrs would collide).
          env = {
            CARGO_BUILD_TARGET = "x86_64-pc-windows-gnu";
            CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER = winCC;
            # Statically link the mingw C runtime / libgcc / winpthread.
            CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS = "-L native=${crossPkgs.windows.pthreads}/lib -C target-feature=+crt-static";
            TARGET_CC = winCC;
            # aws-lc-sys' Windows cross build needs pthread headers for sched.h.
            AWS_LC_SYS_PREBUILT_NASM = "0";
            CFLAGS = "-Wno-stringop-overflow -Wno-array-bounds -Wno-restrict";
            "CFLAGS_x86_64-pc-windows-gnu" = "-I${crossPkgs.windows.pthreads}/include";
            # Link the prebuilt static ffmpeg via host pkg-config reading mingw .pc.
            PKG_CONFIG_ALLOW_CROSS = "1";
            PKG_CONFIG_ALL_STATIC = "1";
            PKG_CONFIG_PATH = "${ffmpegStatic.dev}/lib/pkgconfig";
            LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
            BINDGEN_EXTRA_CLANG_ARGS = "--target=x86_64-w64-mingw32 -isystem ${crossPkgs.stdenv.cc.libc.dev}/include -isystem ${pkgs.llvmPackages.libclang.lib}/lib/clang/${lib.versions.major pkgs.llvmPackages.libclang.version}/include";
          };
        };

        winCargoArtifacts = winCraneLib.buildDepsOnly winCommonArgs;

        ribb-windows = winCraneLib.buildPackage (
          winCommonArgs
          // {
            cargoArtifacts = winCargoArtifacts;

            # nixpkgs' fixup only runs `strip --strip-debug` on binaries, and rustc's
            # own `strip = true` (Cargo.toml) doesn't clear the COFF symbol table on
            # the windows-gnu target — ~44k symbols (~3.7M) survive. Force a real
            # `--strip-all` over $out/bin to drop them.
            stripAllList = [ "bin" ];

            meta = {
              description = "rust iced booru browser (Windows)";
              license = lib.licenses.mit;
            };
          }
        );
      in
      {
        packages = {
          inherit ribb appimage ribb-windows;
          default = ribb;
        };

        apps.default = {
          type = "app";
          program = lib.getExe ribb;
        };
      }
    );
}
