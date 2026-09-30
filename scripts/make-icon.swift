// The app icon's 1024pt master, drawn with AppKit rather than shipped as a
// vector asset: the glyph is the same five-bar waveform the menu bar draws, so
// keeping it in code keeps the two from drifting. Run via scripts/make-icon.sh,
// which renders this and folds the sizes into assets/AppIcon.icns.
//
//   swift scripts/make-icon.swift out.png

import AppKit

let S: CGFloat = 1024
let inset: CGFloat = 100
let bodyR: CGFloat = 185

let ink = NSColor(srgbRed: 0.12, green: 0.12, blue: 0.11, alpha: 1)

func rounded(_ r: NSRect, _ rad: CGFloat) -> NSBezierPath {
    NSBezierPath(roundedRect: r, xRadius: rad, yRadius: rad)
}

func render(_ draw: (NSRect) -> Void) -> NSBitmapImageRep {
    let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(S), pixelsHigh: Int(S),
        bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
        colorSpaceName: .calibratedRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    let body = NSRect(x: inset, y: inset, width: S - 2*inset, height: S - 2*inset)
    let shape = rounded(body, bodyR)
    NSGraphicsContext.current!.cgContext.saveGState()
    shape.addClip()
    NSGradient(colors: [NSColor(srgbRed: 0.976, green: 0.972, blue: 0.960, alpha: 1),
                        NSColor(srgbRed: 0.898, green: 0.890, blue: 0.870, alpha: 1)])!
        .draw(in: body, angle: -90)
    NSGraphicsContext.current!.cgContext.restoreGState()
    // hairline edge so the icon keeps an edge on a white wallpaper
    NSColor(srgbRed: 0, green: 0, blue: 0, alpha: 0.10).setStroke()
    let edge = rounded(body.insetBy(dx: 1.5, dy: 1.5), bodyR - 1.5); edge.lineWidth = 3; edge.stroke()
    draw(body)
    NSGraphicsContext.restoreGraphicsState()
    return rep
}

// The menu bar glyph at icon size: five rounded bars, 6/10/14/10/6 tall on a
// 2-wide, 1.5-gap grid, scaled up together so the proportions match exactly.
func waveform(_ body: NSRect) {
    let heights: [CGFloat] = [6, 10, 14, 10, 6]
    let barW: CGFloat = 2, gap: CGFloat = 1.5
    let glyphW = 5 * barW + 4 * gap
    let scale: CGFloat = 480 / 14
    let left = body.midX - glyphW * scale / 2
    ink.setFill()
    for (i, h) in heights.enumerated() {
        let x = left + CGFloat(i) * (barW + gap) * scale
        let r = NSRect(x: x, y: body.midY - h * scale / 2, width: barW * scale, height: h * scale)
        rounded(r, barW * scale / 2).fill()
    }
}

let out = CommandLine.arguments[1]
let rep = render(waveform)
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: out))
