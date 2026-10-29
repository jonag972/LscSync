// Renders an SVG to PNG with AppKit (macOS 14+).
// usage: swift tools/render-svg.swift <input.svg> <output.png> <size | WxH px> [viewBox x y width height]
import AppKit

let args = CommandLine.arguments
let dims = args.count > 3 ? args[3].split(separator: "x").compactMap { Int($0) } : []
guard args.count == 4 || args.count == 8, dims.count == 1 || dims.count == 2 else {
    FileHandle.standardError.write("usage: render-svg.swift input.svg output.png size|WxH [x y w h]\n".data(using: .utf8)!)
    exit(2)
}
let (width, height) = (dims[0], dims.count == 2 ? dims[1] : dims[0])

var svg = try String(contentsOfFile: args[1], encoding: .utf8)
if args.count == 8 {
    // Crop (e.g. Windows: full-bleed icon, without the macOS grid margin).
    let viewBox = args[4...7].joined(separator: " ")
    svg = svg.replacingOccurrences(of: #"viewBox="[^"]*""#, with: "viewBox=\"\(viewBox)\"", options: .regularExpression)
}

guard let image = NSImage(data: svg.data(using: .utf8)!) else {
    FileHandle.standardError.write("SVG illisible : \(args[1])\n".data(using: .utf8)!)
    exit(1)
}

let rep = NSBitmapImageRep(
    bitmapDataPlanes: nil, pixelsWide: width, pixelsHigh: height, bitsPerSample: 8,
    samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
    bytesPerRow: 0, bitsPerPixel: 0)!
rep.size = NSSize(width: width, height: height)
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
NSGraphicsContext.current?.imageInterpolation = .high
image.draw(in: NSRect(x: 0, y: 0, width: width, height: height))
NSGraphicsContext.restoreGraphicsState()
try rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: args[2]))
