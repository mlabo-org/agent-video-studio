#!/bin/bash
# Builds Agent Video Studio and installs what the hosts run:
#   ~/Applications/Agent Video Studio.app      the editor
#   ~/.local/bin/agent-video-studio-mcp        the MCP server (.mcp.json starts this)
#
# Usage: AVS_SIGN_IDENTITY="<code-signing identity>" scripts/install.sh
#        AVS_SIGN_IDENTITY=- scripts/install.sh      (no certificate: ad-hoc signature)
#
# With a certificate macOS keeps the app's privacy grants (microphone, folder
# access) across rebuilds. An ad-hoc signature changes with every build, so macOS
# asks for those grants again after each install; it is used only when asked for
# with "-". The build runs in a temporary directory outside the checkout and is
# removed afterwards. Needs: macOS 14 or later, a Rust toolchain (cargo), and the
# Xcode command line tools (codesign).
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
app_name="Agent Video Studio.app"
bundle_id="org.mlabo.agent-video-studio"
app_dir="${AVS_APP_DIR:-$HOME/Applications}"
bin_dir="${AVS_BIN_DIR:-$HOME/.local/bin}"

if [ -z "${AVS_SIGN_IDENTITY:-}" ]; then
  echo "AVS_SIGN_IDENTITY is not set. Set it to the code-signing identity to use" >&2
  echo "(list them with: security find-identity -v -p codesigning)," >&2
  echo "or to - for an ad-hoc signature when this machine has no certificate." >&2
  exit 1
fi

work="$(cd "${TMPDIR:-/tmp}" && pwd -P)/agent-video-studio-install.$$"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work"

MACOSX_DEPLOYMENT_TARGET=14.0 cargo build --release --locked \
  --manifest-path "$repo_root/Cargo.toml" \
  --target-dir "$work/target" \
  --package avs-app --package avs-mcp

bundle="$work/$app_name"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
cp "$work/target/release/agent-video-studio" "$bundle/Contents/MacOS/agent-video-studio"
cp "$repo_root/macos/Info.plist" "$bundle/Contents/Info.plist"
if [ -f "$repo_root/macos/AppIcon.icns" ]; then
  cp "$repo_root/macos/AppIcon.icns" "$bundle/Contents/Resources/AppIcon.icns"
  /usr/libexec/PlistBuddy -c "Add :CFBundleIconFile string AppIcon.icns" "$bundle/Contents/Info.plist"
fi
/usr/bin/codesign --force --sign "$AVS_SIGN_IDENTITY" --identifier "$bundle_id" "$bundle"
/usr/bin/codesign --verify --strict --deep "$bundle"
if [ "$AVS_SIGN_IDENTITY" != "-" ] && /usr/bin/codesign -d -r- "$bundle" 2>&1 | grep -q "cdhash"; then
  echo "The app was signed without a certificate (its requirement names a cdhash)." >&2
  exit 1
fi

mcp="$work/agent-video-studio-mcp"
cp "$work/target/release/agent-video-studio-mcp" "$mcp"
/usr/bin/codesign --force --sign "$AVS_SIGN_IDENTITY" --identifier "$bundle_id.mcp" "$mcp"

# Put new files beside their destination and rename them into place: overwriting a
# binary a running process has open makes macOS kill that process.
mkdir -p "$app_dir" "$bin_dir"
staged_app="$app_dir/.$app_name.new.$$"
rm -rf "$staged_app"
cp -R "$bundle" "$staged_app"
if [ -e "$app_dir/$app_name" ]; then
  old_app="$app_dir/.$app_name.old.$$"
  mv "$app_dir/$app_name" "$old_app"
  mv "$staged_app" "$app_dir/$app_name"
  rm -rf "$old_app"
else
  mv "$staged_app" "$app_dir/$app_name"
fi
staged_mcp="$bin_dir/.agent-video-studio-mcp.new.$$"
cp "$mcp" "$staged_mcp"
chmod 755 "$staged_mcp"
mv "$staged_mcp" "$bin_dir/agent-video-studio-mcp"

printf '%s\n' "$app_dir/$app_name" "$bin_dir/agent-video-studio-mcp"
