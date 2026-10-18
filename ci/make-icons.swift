// Makes Freya's icons from the artwork in assets/icon-art.png: the artwork on a rounded
// square following the macOS icon grid (824 of 1024 points, with a soft shadow and transparent
// corners), as the PNG sizes cargo-packager and Freya use and as a Windows .ico.
//
//   swift ci/make-icons.swift [assets/icon-art.png] [assets/icons] [preview.png]
import AppKit

let srgb = CGColorSpace(name: CGColorSpace.sRGB)!

/// The icon on a 1024×1024 canvas, y up, drawn at `scale` pixels per point.
func drawIcon(in ctx: CGContext, art: CGImage, scale: CGFloat) {
    let tile = CGRect(x: 100, y: 100, width: 824, height: 824)
    let path = CGPath(roundedRect: tile, cornerWidth: 185, cornerHeight: 185, transform: nil)

    ctx.saveGState()
    // A shadow's offset and blur are in pixels, whatever the context is scaled by: at 1024
    // points they spread over the margin, and small icons need them scaled down to match, or
    // the shadow reaches their edges.
    ctx.setShadow(offset: CGSize(width: 0, height: -10 * scale), blur: 28 * scale,
                  color: CGColor(gray: 0, alpha: 0.30))
    ctx.addPath(path)
    ctx.setFillColor(CGColor(gray: 1, alpha: 1))
    ctx.fillPath()
    ctx.restoreGState()

    ctx.saveGState()
    ctx.addPath(path)
    ctx.clip()
    ctx.interpolationQuality = .high
    ctx.draw(art, in: tile)
    ctx.restoreGState()
}

func render(_ size: Int, art: CGImage) -> Data {
    let ctx = CGContext(data: nil, width: size, height: size, bitsPerComponent: 8,
                        bytesPerRow: 0, space: srgb,
                        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    let scale = CGFloat(size) / 1024
    ctx.scaleBy(x: scale, y: scale)
    drawIcon(in: ctx, art: art, scale: scale)
    return NSBitmapImageRep(cgImage: ctx.makeImage()!).representation(using: .png, properties: [:])!
}

/// A Windows icon holding PNG images, as Windows Vista and later read them.
func ico(_ images: [(Int, Data)]) -> Data {
    var out = Data()
    func u16(_ v: Int) { out.append(contentsOf: [UInt8(v & 0xFF), UInt8(v >> 8 & 0xFF)]) }
    func u32(_ v: Int) { u16(v & 0xFFFF); u16(v >> 16) }
    u16(0); u16(1); u16(images.count)
    var offset = 6 + 16 * images.count
    for (size, png) in images {
        out.append(UInt8(size >= 256 ? 0 : size))
        out.append(UInt8(size >= 256 ? 0 : size))
        out.append(0); out.append(0)
        u16(1); u16(32)
        u32(png.count); u32(offset)
        offset += png.count
    }
    for (_, png) in images { out.append(png) }
    return out
}

let args = CommandLine.arguments
let source = args.count > 1 ? args[1] : "assets/icon-art.png"
let outDir = URL(fileURLWithPath: args.count > 2 ? args[2] : "assets/icons")
guard let image = CGImageSourceCreateWithURL(URL(fileURLWithPath: source) as CFURL, nil),
      let art = CGImageSourceCreateImageAtIndex(image, 0, nil)
else {
    FileHandle.standardError.write("make-icons: cannot read \(source)\n".data(using: .utf8)!)
    exit(1)
}
try FileManager.default.createDirectory(at: outDir, withIntermediateDirectories: true)
for size in [32, 64, 128, 256] {
    try render(size, art: art).write(to: outDir.appendingPathComponent("\(size)x\(size).png"))
}
try render(512, art: art).write(to: outDir.appendingPathComponent("icon.png"))
try ico([16, 24, 32, 48, 64, 128, 256].map { ($0, render($0, art: art)) })
    .write(to: outDir.appendingPathComponent("icon.ico"))
if args.count > 3 {
    try render(1024, art: art).write(to: URL(fileURLWithPath: args[3]))
}
