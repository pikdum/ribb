{
  description = "ribb — rust iced booru browser (iced GUI, ffmpeg video, Ruffle SWF)";

  # Outputs: `ribb` (native, wrapped), `appimage`, `ribb-windows` (static .exe).
  #
  # The hard part is Ruffle (via iced_ruffle). Three things tamed below:
  #  - `ruffle_core`'s build.rs compiles its playerglobal from `../core/src/...`;
  #    vendoring flattens the crate, so the cargoDeps wrapper repoints it.
  #  - `swf 0.2.2` comes from BOTH crates.io (Ruffle's build-time `rascal`) and
  #    Ruffle's git workspace — a duplicate cargo/nix can't vendor offline. We
  #    dedup by pointing both at one vendored copy (vendor/swf) via a path
  #    [patch] in Cargo.toml (a git [patch] can't resolve against an offline
  #    vendor). iced_ruffle is pinned to a ruffle rev so that copy stays in sync.
  #  - Windows: ribb only DECODES, so the static ffmpeg needs no x264/openh264;
  #    the gfxcapture WinRT filter is disabled (it drags in mcfgthread link deps).

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    nix-appimage = {
      url = "github:ralismark/nix-appimage";
      inputs.nixpkgs.follows = "nixpkgs";
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
      fenix,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
        lib = pkgs.lib;

        fenixPkgs = fenix.packages.${system};
        rustStable = fenixPkgs.stable.toolchain;
        rustPlatform = pkgs.makeRustPlatform {
          cargo = rustStable;
          rustc = rustStable;
        };

        # Vendor with real `cargo vendor` (fetchCargoVendor FOD), not crane or
        # importCargoLock — both fought us over Ruffle: crane flattens git deps and
        # strips the playerglobal `.as` files; importCargoLock emits a relative
        # vendored-sources path that didn't resolve once a [patch] forced cargo to
        # re-query. cargo's own vendor produces a config cargo fully trusts offline.
        # The swf [patch] in Cargo.toml dedups the crates.io/git swf so vendoring
        # doesn't hit "found duplicate version".
        baseVendor = rustPlatform.fetchCargoVendor {
          src = lib.cleanSource ./.;
          hash = "sha256-4TTo2JR+1KEibGTg+7Z1Bqqwgi2nnBK46rkGVe8K6cE=";
        };
        # `ruffle_core`'s build.rs compiles its playerglobal from
        # `repo_root("../")/core/src/avm{1,2}/globals/`, but vendoring flattens the
        # crate so `../core` misses. Point repo_root at the crate itself and map
        # `core/src -> ../src`, then refresh the vendor checksum for build.rs.
        cargoDeps = pkgs.runCommandLocal "ribb-cargo-vendor-dir" { } ''
          cp -a --no-preserve=mode,ownership ${baseVendor} $out
          chmod -R u+w $out
          # fetchCargoVendor nests crates under source-git-*/ and source-registry-*/.
          # ruffle_core's git-vendored checksum is `files:{}` (no per-file check),
          # so editing build.rs needs no checksum update.
          rc=$(dirname "$(find $out -maxdepth 3 -path '*/ruffle_core-*/build.rs' | head -1)")
          sed -i 's|Path::new("../")|Path::new(".")|' "$rc/build.rs"
          mkdir "$rc/core"
          ln -s ../src "$rc/core/src"
        '';

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

        ribb = rustPlatform.buildRustPackage {
          pname = "ribb";
          version = "0.1.0";
          src = lib.cleanSource ./.;
          inherit cargoDeps;
          doCheck = false;

          # The swf path-[patch] would otherwise make cargo re-resolve (needing
          # the crates-io index, which the offline vendor doesn't carry). The
          # committed Cargo.lock already reflects the patch, so --locked forbids
          # re-resolution and cargo just uses the lock + vendor.
          cargoBuildFlags = [ "--locked" ];

          nativeBuildInputs = [
            pkgs.pkg-config
            # bindgen (ffmpeg-sys-next) needs libclang for the libav bindings.
            pkgs.llvmPackages.libclang
            # Ruffle's build script compiles the AVM2 playerglobal with a JDK.
            pkgs.jdk
            pkgs.makeWrapper
            # Installs desktopItem into $out/share/applications via postInstall.
            pkgs.copyDesktopItems
          ];

          desktopItems = [ desktopItem ];
          buildInputs = [
            ffmpegNative
            pkgs.alsa-lib
          ];

          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
          BINDGEN_EXTRA_CLANG_ARGS = "-isystem ${pkgs.glibc.dev}/include -isystem ${pkgs.llvmPackages.libclang.lib}/lib/clang/${lib.versions.major pkgs.llvmPackages.libclang.version}/include";

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
        };

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

        winRust = fenixPkgs.combine [
          fenixPkgs.stable.rustc
          fenixPkgs.stable.cargo
          fenixPkgs.targets.x86_64-pc-windows-gnu.stable.rust-std
        ];
        # Cross pkgs' makeRustPlatform so buildRustPackage's host platform (hence
        # --target and the cc/linker) is Windows, while the fenix toolchain (built
        # for the build host) provides the windows-gnu std.
        winPlatform = crossPkgs.makeRustPlatform {
          cargo = winRust;
          rustc = winRust;
        };
        winCC = "${crossPkgs.stdenv.cc}/bin/${crossPkgs.stdenv.cc.targetPrefix}cc";

        ribb-windows = winPlatform.buildRustPackage {
          pname = "ribb";
          version = "0.1.0";
          src = lib.cleanSource ./.;
          inherit cargoDeps;
          doCheck = false;
          cargoBuildFlags = [ "--locked" ];

          # nixpkgs' fixup only runs `strip --strip-debug` on binaries, and rustc's
          # own `strip = true` (Cargo.toml) doesn't clear the COFF symbol table on
          # the windows-gnu target — ~44k symbols (~3.7M) survive. Force a real
          # `--strip-all` over $out/bin to drop them.
          stripAllList = [ "bin" ];

          # Build scripts (incl. Ruffle's JDK-free rascal playerglobal) run on the
          # host; the cross toolchain targets Windows.
          depsBuildBuild = [ crossPkgs.stdenv.cc ];
          nativeBuildInputs = [
            pkgs.pkg-config
            pkgs.llvmPackages.libclang
            pkgs.jdk
          ];

          # Env vars go in `env` (the cross stdenv already populates some via
          # structuredAttrs, so top-level attrs would collide).
          env = {
            CARGO_BUILD_TARGET = "x86_64-pc-windows-gnu";
            CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER = winCC;
            TARGET_CC = winCC;
            # Link the prebuilt static ffmpeg via host pkg-config reading mingw .pc.
            PKG_CONFIG_ALLOW_CROSS = "1";
            PKG_CONFIG_ALL_STATIC = "1";
            PKG_CONFIG_PATH = "${ffmpegStatic.dev}/lib/pkgconfig";
            LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
            BINDGEN_EXTRA_CLANG_ARGS = "--target=x86_64-w64-mingw32 -isystem ${crossPkgs.stdenv.cc.libc.dev}/include -isystem ${pkgs.llvmPackages.libclang.lib}/lib/clang/${lib.versions.major pkgs.llvmPackages.libclang.version}/include";
            # Statically link the mingw C runtime / libgcc / winpthread.
            RUSTFLAGS = "-C target-feature=+crt-static";
          };

          meta = {
            description = "rust iced booru browser (Windows)";
            license = lib.licenses.mit;
          };
        };
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
