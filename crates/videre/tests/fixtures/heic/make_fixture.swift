// Generates grid_rot90.heic: a synthetic test pattern (never a photo) saved
// the way an iPhone saves a portrait shot: a tiled `grid` primary image, an
// embedded thumbnail, and EXIF Orientation 6, which ImageIO writes as an
// `irot` of 270 degrees shared by the grid and the thumbnail.
//
//   swift make_fixture.swift grid_rot90.heic
import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

let width = 2048, height = 1536
let space = CGColorSpaceCreateDeviceRGB()
let ctx = CGContext(
    data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
    space: space, bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)!
// Flat grey with one blue band, so the tiles compress small.
ctx.setFillColor(CGColor(red: 0.5, green: 0.5, blue: 0.5, alpha: 1))
ctx.fill(CGRect(x: 0, y: 0, width: width, height: height))
ctx.setFillColor(CGColor(red: 0.2, green: 0.3, blue: 0.8, alpha: 1))
ctx.fill(CGRect(x: 0, y: 0, width: width, height: height / 4))
// The marker: solid red, 256x256, in the top-left corner of the stored
// pixels. CoreGraphics' origin is bottom-left, hence the y.
ctx.setFillColor(CGColor(red: 1, green: 0, blue: 0, alpha: 1))
ctx.fill(CGRect(x: 0, y: height - 256, width: 256, height: 256))
let image = ctx.makeImage()!

let out = URL(fileURLWithPath: CommandLine.arguments[1])
let dest = CGImageDestinationCreateWithURL(
    out as CFURL, UTType.heic.identifier as CFString, 1, nil)!
let props: [CFString: Any] = [
    kCGImagePropertyOrientation: 6,
    kCGImageDestinationLossyCompressionQuality: 0.3,
    kCGImageDestinationEmbedThumbnail: true,
]
CGImageDestinationAddImage(dest, image, props as CFDictionary)
precondition(CGImageDestinationFinalize(dest), "could not write \(out.path)")
