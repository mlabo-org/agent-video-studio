#!/bin/bash
# Rebuilds macos/AppIcon.icns from macos/AppIcon.svg.
# Run it after editing the SVG; scripts/install.sh copies the .icns into the app.
# Uses only tools that ship with macOS and Xcode (swift, iconutil).
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
svg="$repo_root/macos/AppIcon.svg"
out="$repo_root/macos/AppIcon.icns"

work="$(cd "${TMPDIR:-/tmp}" && pwd -P)/agent-video-studio-icon.$$"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/AppIcon.iconset"

cat > "$work/render.swift" <<'SWIFT'
import AppKit

let args = CommandLine.arguments
guard args.count == 3, let image = NSImage(contentsOfFile: args[1]) else {
    FileHandle.standardError.write("usage: render <svg> <iconset dir>\n".data(using: .utf8)!)
    exit(1)
}
let sizes: [(String, Int)] = [
    ("icon_16x16", 16), ("icon_16x16@2x", 32), ("icon_32x32", 32), ("icon_32x32@2x", 64),
    ("icon_128x128", 128), ("icon_128x128@2x", 256), ("icon_256x256", 256), ("icon_256x256@2x", 512),
    ("icon_512x512", 512), ("icon_512x512@2x", 1024),
]
for (name, pixels) in sizes {
    guard let bitmap = NSBitmapImageRep(
        bitmapDataPlanes: nil, pixelsWide: pixels, pixelsHigh: pixels, bitsPerSample: 8, samplesPerPixel: 4,
        hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)
    else { exit(1) }
    bitmap.size = NSSize(width: pixels, height: pixels)
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: bitmap)
    NSGraphicsContext.current?.imageInterpolation = .high
    image.draw(in: NSRect(x: 0, y: 0, width: pixels, height: pixels), from: .zero, operation: .copy, fraction: 1)
    NSGraphicsContext.restoreGraphicsState()
    guard let png = bitmap.representation(using: .png, properties: [:]) else { exit(1) }
    try png.write(to: URL(fileURLWithPath: args[2]).appendingPathComponent("\(name).png"))
}
SWIFT

swift "$work/render.swift" "$svg" "$work/AppIcon.iconset"
iconutil --convert icns --output "$work/AppIcon.icns" "$work/AppIcon.iconset"
mv "$work/AppIcon.icns" "$out"
printf '%s\n' "$out"
