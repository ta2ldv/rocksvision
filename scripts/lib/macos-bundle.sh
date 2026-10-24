# macOS .app bundle helper (sourced after lib/common.sh; needs ROOT, RV_TMP, RV_EXE, RV_VERSION).
# Extracted from the former scripts/build-app.sh: approved-icon SHA gate, Info.plist from
# src/branding.rs + Cargo.toml, .icns via sips/iconutil, ad-hoc codesign. No installer, DMG or
# notarization.

# Checks the macOS command line tools (compiler/linker for the vendored C libraries and
# iconutil/codesign). Stops with an explanation when they need user interaction.
rv_macos_prereqs() {
  if ! xcode-select -p >/dev/null 2>&1; then
    xcode-select --install >/dev/null 2>&1 || true
    rv_die "the Xcode Command Line Tools are not installed. A system dialog to install them was opened (it needs your confirmation); re-run this script when the installation has finished."
  fi
  if ! out=$(xcrun clang --version 2>&1); then
    case "$out" in
      *license*) rv_die "the Xcode license has not been accepted. Run 'sudo xcodebuild -license' (interactive, needs an administrator) and re-run." ;;
      *) rv_die "xcrun clang does not work: $out" ;;
    esac
  fi
  for t in sips iconutil plutil codesign ditto; do
    command -v "$t" >/dev/null 2>&1 || rv_die "$t not found (part of macOS / the Command Line Tools)"
  done
}

# Approved logo, used unmodified (1024 px source).
icon_src_rel="logo/rocksvision-cat-large.png"
rv_macos_icon_check() {
  icon_src="$ROOT/$icon_src_rel"
  icon_sha="500e8cb4bf8c51702421610f98d57538fb79bb550325c9d83c4c0ca792debc50"
  [ "$(rv_sha256 "$icon_src")" = "$icon_sha" ] || rv_die "$icon_src is not the approved logo"
}

# Single source: name/identifier/year/author from src/branding.rs.
rv_brand() { sed -n "s/^pub const $1: &str = \"\(.*\)\";/\1/p" "$ROOT/src/branding.rs" | head -n1; }

# rv_macos_bundle <binary> <out dir> <LSMinimumSystemVersion>  -> <out dir>/<NAME>.app (RV_APP)
rv_macos_bundle() {
  bin=$1
  out=$2
  min_os=$3
  NAME=$(rv_brand NAME)
  BUNDLE_ID=$(rv_brand BUNDLE_ID)
  YEAR=$(rv_brand YEAR)
  AUTHOR=$(rv_brand AUTHOR)
  for v in NAME BUNDLE_ID YEAR AUTHOR; do
    eval "val=\${$v}"
    [ -n "$val" ] || rv_die "$v not found in src/branding.rs"
  done
  rv_macos_icon_check
  iconset="$RV_TMP/AppIcon.iconset"
  rm -rf "$iconset"
  mkdir -p "$iconset"
  for s in 16 32 128 256 512; do
    sips -z "$s" "$s" "$icon_src" --out "$iconset/icon_${s}x${s}.png" >/dev/null
    d=$((s * 2))
    sips -z "$d" "$d" "$icon_src" --out "$iconset/icon_${s}x${s}@2x.png" >/dev/null
  done

  RV_APP="$out/$NAME.app"
  rm -rf "$RV_APP"
  mkdir -p "$RV_APP/Contents/MacOS" "$RV_APP/Contents/Resources"
  cp "$bin" "$RV_APP/Contents/MacOS/$RV_EXE"
  iconutil -c icns "$iconset" -o "$RV_APP/Contents/Resources/AppIcon.icns"
  cp "$ROOT/LICENSE" "$RV_APP/Contents/Resources/LICENSE"
  cp "$ROOT/THIRD-PARTY-LICENSES.md" "$RV_APP/Contents/Resources/THIRD-PARTY-LICENSES.md"

  cat >"$RV_APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>${NAME}</string>
  <key>CFBundleDisplayName</key><string>${NAME}</string>
  <key>CFBundleIdentifier</key><string>${BUNDLE_ID}</string>
  <key>CFBundleExecutable</key><string>${RV_EXE}</string>
  <key>NSHumanReadableCopyright</key><string>${NAME} v${RV_VERSION} - ${YEAR} by ${AUTHOR}</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${RV_VERSION}</string>
  <key>CFBundleVersion</key><string>${RV_VERSION}</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>LSMinimumSystemVersion</key><string>${min_os}</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSPrincipalClass</key><string>NSApplication</string>
</dict>
</plist>
PLIST

  plutil -lint "$RV_APP/Contents/Info.plist" >/dev/null || rv_die "Info.plist is invalid"
  # Ad-hoc signature so Gatekeeper treats the bundle as one unit locally (not notarization).
  codesign --force --sign - "$RV_APP" >/dev/null 2>&1 || rv_warn "ad-hoc codesign failed (bundle still usable locally)"
}

# Full macOS flow shared by build-macos-arm64.sh / build-macos-x64.sh.
# rv_macos_build <platform dir name> <target triple or empty for host> <triple for BUILD-INFO>
rv_macos_build() {
  platform=$1
  target=$2
  triple=$3
  rv_require_inputs src/branding.rs "$icon_src_rel"
  rv_macos_icon_check
  rv_macos_prereqs
  if [ "$(uname -m)" = arm64 ]; then host_triple=aarch64-apple-darwin; else host_triple=x86_64-apple-darwin; fi
  if [ "$triple" = aarch64-apple-darwin ]; then arch=arm64; else arch=x86_64; fi
  rv_ensure_rust "$host_triple" "$target"
  rv_build "$target"
  lipo -archs "$RV_BIN" 2>/dev/null | grep -qw "$arch" || rv_die "$RV_BIN is not a $triple binary ($(lipo -archs "$RV_BIN" 2>/dev/null || true))"
  rv_stage_dist "$platform"
  rv_macos_bundle "$RV_BIN" "$RV_DIST" 11.0
  cp "$ROOT/LICENSE" "$RV_DIST/LICENSE"
  cp "$ROOT/THIRD-PARTY-LICENSES.md" "$RV_DIST/THIRD-PARTY-LICENSES.md"
  rv_build_info "$RV_DIST/BUILD-INFO.txt" "$platform" "$triple"
  rv_sha256sums "$RV_DIST"
  zip_name="$RV_EXE-$RV_VERSION-$platform.zip"
  # ditto keeps the bundle structure, symlinks and extended attributes.
  ditto -c -k --keepParent "$RV_DIST" "$RV_STAGE/$zip_name"
  rv_archive_sum "$RV_STAGE/$zip_name"
  rv_publish_dist "$platform"
  RV_APP="$RV_DIST/$NAME.app"
  zip="$ROOT/dist/$zip_name"
  printf 'Built: %s\nArchive: %s\nSHA-256: %s\n' "$RV_APP" "$zip" "$(cut -d' ' -f1 "$zip.sha256")"
}
