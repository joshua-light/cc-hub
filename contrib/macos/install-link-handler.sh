#!/bin/sh
# Install the cc-hub:// URL-scheme handler on macOS.
#
# Compiles link-handler.applescript into "~/Applications/cc-hub Link.app",
# declares the `cc-hub` scheme in its Info.plist, hides it from the Dock, and
# registers it with Launch Services. Afterwards any cc-hub:// URL — from a
# browser, `open 'cc-hub://…'`, a Shortcut — runs `cc-hub open <url>`.
#
# The handler needs an absolute path to the cc-hub binary. Default: this
# repo's target/release/cc-hub. Override with CC_HUB_BIN=/path/to/cc-hub.
# Re-run after moving the binary; re-running is idempotent.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
bin=${CC_HUB_BIN:-"$repo/target/release/cc-hub"}
app="$HOME/Applications/cc-hub Link.app"
plist="$app/Contents/Info.plist"
lsregister=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister

if [ ! -x "$bin" ]; then
    echo "cc-hub binary not found at $bin — run 'cargo build --release' first, or set CC_HUB_BIN" >&2
    exit 1
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
sed "s|__CC_HUB__|$bin|g" "$here/link-handler.applescript" > "$work/link-handler.applescript"

mkdir -p "$HOME/Applications"
rm -rf "$app"
osacompile -o "$app" "$work/link-handler.applescript"

pb() { /usr/libexec/PlistBuddy -c "$1" "$plist"; }
pb "Delete :CFBundleIdentifier" >/dev/null 2>&1 || true
pb "Add :CFBundleIdentifier string dev.cc-hub.link"
pb "Add :CFBundleURLTypes array"
pb "Add :CFBundleURLTypes:0 dict"
pb "Add :CFBundleURLTypes:0:CFBundleURLName string cc-hub deep link"
pb "Add :CFBundleURLTypes:0:CFBundleURLSchemes array"
pb "Add :CFBundleURLTypes:0:CFBundleURLSchemes:0 string cc-hub"
pb "Add :LSUIElement bool true"

"$lsregister" -f "$app"

echo "installed: $app"
echo "handler:   $bin open <cc-hub://url>"
echo "try:       open 'cc-hub://review?depth=light&pr=https://bitbucket.example.com/projects/X/repos/some-repo/pull-requests/1'"
